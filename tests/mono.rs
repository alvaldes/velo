//! The mono downmix (issue #8, S1, S4): both output channels carry the whole
//! mix — the average of the channels, never their sum, so the peak does not
//! grow. The downmix is a step in the producer's sample path: after
//! `ReplayGain`, before the ring — and the toggle reaches that path through a
//! shared cell, so it lands without restarting the stream.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use lofty::config::WriteOptions;
use lofty::tag::{ItemKey, Tag, TagExt as _, TagType};

use velo::audio::TrackSpec;
use velo::audio::output::{Entry, MonoCell, TrackFailure, drain_headless_with_mono, mono_mix};
use velo::config::ReplayGainMode;

/// The linear factor a `ReplayGain` dB value stands for: `10^(dB / 20)`.
fn linear(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

fn stereo_spec() -> TrackSpec {
    TrackSpec {
        sample_rate: 44_100,
        channels: 2,
    }
}

/// A throwaway path in the system temp directory, outside the worktree.
fn scratch_path(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("velo-mono-{tag}-{}.wav", std::process::id()))
}

/// Writes a 16-bit PCM stereo WAV with every left sample at `left` and every
/// right sample at `right` (i16 units).
fn write_stereo_wav(path: &Path, spec: TrackSpec, frames: usize, left: i16, right: i16) {
    assert_eq!(spec.channels, 2, "this helper writes stereo only");
    let bits_per_sample: u16 = 16;
    let block_align = spec.channels * bits_per_sample / 8;
    let byte_rate = spec.sample_rate * u32::from(block_align);
    let data_len = u32::try_from(frames * usize::from(spec.channels) * 2)
        .expect("test data fits in a RIFF chunk");

    let mut out = Vec::with_capacity(44 + usize::try_from(data_len).expect("fits"));
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16_u32.to_le_bytes());
    out.extend_from_slice(&1_u16.to_le_bytes()); // PCM
    out.extend_from_slice(&spec.channels.to_le_bytes());
    out.extend_from_slice(&spec.sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits_per_sample.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for _ in 0..frames {
        out.extend_from_slice(&left.to_le_bytes());
        out.extend_from_slice(&right.to_le_bytes());
    }
    fs::write(path, out).expect("write test wav");
}

/// Writes the `ReplayGain` track-gain tag the tests read back.
fn tag_with(path: &Path, values: &[(ItemKey, &str)]) {
    let mut tag = Tag::new(TagType::Id3v2);
    for (key, value) in values {
        tag.insert_text(*key, (*value).to_owned());
    }
    tag.save_to_path(path, WriteOptions::default())
        .expect("tag the fixture");
}

fn as_f32(sample: i16) -> f32 {
    f32::from(sample) / 32_768.0
}

// ---- the downmix itself -----------------------------------------------------

#[test]
fn mono_averages_each_stereo_frame_so_the_peak_does_not_grow() {
    let mut samples = vec![0.8_f32, 0.2, 0.6, -0.4];
    mono_mix(&mut samples, 2);
    // (left + right) / 2 on both channels — not the sum, which would grow the
    // peak: (0.6 + (-0.4)) averages to 0.1 on both, never 1.0.
    assert!(
        samples[..2]
            .iter()
            .all(|sample| (sample - 0.5).abs() < 1e-5),
        "0.8 and 0.2 average to 0.5 on both channels, got {samples:?}"
    );
    assert!(
        samples[2..]
            .iter()
            .all(|sample| (sample - 0.1).abs() < 1e-5),
        "0.6 and -0.4 average to 0.1 on both channels, got {samples:?}"
    );
}

#[test]
fn mono_mix_leaves_mono_frames_and_a_partial_frame_alone() {
    // One channel per frame: the mean of one sample is that sample.
    let mut samples = vec![0.25, -0.75];
    mono_mix(&mut samples, 1);
    assert_eq!(samples, vec![0.25, -0.75]);

    // A trailing sample that is not a whole frame is not a frame: it is left
    // exactly as it came, the same rule the spectrum tap keeps.
    let mut samples = vec![0.8, 0.2, 0.9];
    mono_mix(&mut samples, 2);
    assert_eq!(samples, vec![0.5, 0.5, 0.9]);
}

// ---- the shared cell (S4) ---------------------------------------------------

#[test]
fn the_mono_cell_defaults_to_off_and_toggles_in_place() {
    let cell = MonoCell::default();
    assert!(!cell.get(), "a fresh cell is stereo");
    cell.set(true);
    assert!(cell.get(), "the cell holds the toggle");
    cell.set(false);
    assert!(!cell.get());
}

// ---- the sample path (S1, S4) -----------------------------------------------

/// Drains `queue` under `mode` with the given mono cell, collecting every
/// block the producer pushed — the exact loop the producer thread runs.
fn drain_with(
    queue: Vec<PathBuf>,
    mode: ReplayGainMode,
    mono: &MonoCell,
    on_block: &mut dyn FnMut(&[f32]),
) {
    let stop = AtomicBool::new(false);
    drain_headless_with_mono(
        Arc::new(Mutex::new(queue.into_iter().map(Entry::from).collect())),
        0,
        0,
        stereo_spec(),
        mode,
        mono,
        &stop,
        on_block,
        &mut |failure: TrackFailure| panic!("nothing may fail here: {failure:?}"),
    )
    .expect("the drain runs");
}

#[test]
fn the_drain_carries_the_averaged_mix_after_the_gain_into_the_ring() {
    const LEFT: i16 = 24_000;
    const RIGHT: i16 = 8_000;
    let spec = stereo_spec();
    let path = scratch_path("averaged");
    write_stereo_wav(&path, spec, 64, LEFT, RIGHT);
    tag_with(&path, &[(ItemKey::ReplayGainTrackGain, "-6.02 dB")]);

    let mono = MonoCell::new(true);
    let mut heard = Vec::new();
    drain_with(
        vec![path.clone()],
        ReplayGainMode::Track,
        &mono,
        &mut |block| {
            heard.extend_from_slice(block);
        },
    );
    let _ = fs::remove_file(&path);

    // ReplayGain first (-6.02 dB, the half-step factor), then the average:
    // both channels carry ((g·L) + (g·R)) / 2, the whole mix at no extra peak.
    let factor = linear(-6.02);
    let left = as_f32(LEFT);
    let right = as_f32(RIGHT);
    let mixed = f32::midpoint(left * factor, right * factor);
    assert_eq!(heard.len(), 64 * 2, "the whole track was drained");
    for (index, sample) in heard.iter().enumerate() {
        assert!(
            (sample - mixed).abs() < 1e-4,
            "sample {index} should be {mixed}, got {sample}"
        );
    }
}

#[test]
fn the_producer_reads_the_mono_cell_between_blocks_without_a_restart() {
    const LEFT: i16 = 24_000;
    const RIGHT: i16 = 8_000;
    let spec = stereo_spec();
    // Half a second: many decoder blocks, so a mid-drain toggle has blocks
    // left to land on.
    let path = scratch_path("mid-drain");
    write_stereo_wav(&path, spec, 22_050, LEFT, RIGHT);

    let mono = MonoCell::default();
    let mut blocks: Vec<Vec<f32>> = Vec::new();
    drain_with(
        vec![path.clone()],
        ReplayGainMode::Off,
        &mono,
        &mut |block| {
            blocks.push(block.to_vec());
            if blocks.len() == 1 {
                // The user pressed `M` after the first block was already on its
                // way: every block after it must carry the mix, and the stream
                // never stopped for it.
                mono.set(true);
            }
        },
    );
    let _ = fs::remove_file(&path);

    assert!(
        blocks.len() >= 2,
        "the fixture must span several blocks for the toggle to be observable, got {}",
        blocks.len()
    );
    let left = as_f32(LEFT);
    let right = as_f32(RIGHT);
    let mixed = f32::midpoint(left, right);
    // Before the toggle: untouched stereo.
    for (index, sample) in blocks[0].iter().enumerate() {
        let want = if index % 2 == 0 { left } else { right };
        assert!(
            (sample - want).abs() < 1e-4,
            "block 0 sample {index} should be {want}, got {sample}"
        );
    }
    // After the toggle, mid-sequence: every later block is the averaged mix.
    for (number, block) in blocks[1..].iter().enumerate() {
        for (index, sample) in block.iter().enumerate() {
            assert!(
                (sample - mixed).abs() < 1e-4,
                "block {} sample {index} should be {mixed}, got {sample}",
                number + 1
            );
        }
    }
}
