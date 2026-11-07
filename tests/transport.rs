//! Transport rules (S8): the gain ramp, seeking by offset and the seek target.
//!
//! Everything here is exercised without a sound device: the ramp and the seek
//! target are pure, and a seek is observed through [`velo::audio::TrackSource`]
//! the same way the producer thread reads a track.

use std::path::{Path, PathBuf};
use std::time::Duration;

use velo::audio::{GainRamp, TrackSource, TrackSpec, seek_target};

/// Writes a minimal PCM16 WAV file so the tests need no external tooling.
fn write_wav(path: &Path, spec: TrackSpec, frames: &[i16]) {
    let bits_per_sample: u16 = 16;
    let block_align = spec.channels * bits_per_sample / 8;
    let byte_rate = spec.sample_rate * u32::from(block_align);
    let data_len = u32::try_from(frames.len() * 2).expect("test data fits in a RIFF chunk");

    let mut out = Vec::with_capacity(44 + frames.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&spec.channels.to_le_bytes());
    out.extend_from_slice(&spec.sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits_per_sample.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for sample in frames {
        out.extend_from_slice(&sample.to_le_bytes());
    }

    std::fs::write(path, out).expect("write test wav");
}

/// A unique scratch path per test and file, removed by the caller.
fn scratch_path(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("velo-transport-{tag}-{}.wav", std::process::id()))
}

/// A per-frame ramp in a range that never reaches zero, so a wrong seek point
/// changes the sample values and cannot hide behind a constant.
fn ramp_frames(frames: usize, channels: u16, offset: i16) -> Vec<i16> {
    (0..frames)
        .flat_map(|frame| {
            let value = offset + i16::try_from(frame % 500).expect("small ramp") + 1;
            std::iter::repeat_n(value, usize::from(channels))
        })
        .collect()
}

/// The `i16` PCM the test wrote, as the `f32` samples a decoder must produce.
fn as_f32(samples: &[i16]) -> Vec<f32> {
    samples
        .iter()
        .map(|sample| f32::from(*sample) / 32_768.0)
        .collect()
}

/// Half a PCM16 step: any off-by-one frame exceeds this, float noise does not.
const PCM16_TOLERANCE: f32 = 1e-5;

/// What one audio buffer is allowed to move the gain per sample. A ramp over
/// the buffer sizes a device asks for (hundreds of samples) moves far less;
/// the bound exists to catch a gain that jumps instead of fading.
const MAX_STEP_PER_SAMPLE: f32 = 0.01;

#[test]
fn the_gain_ramp_reaches_the_target_within_one_buffer() {
    let mut ramp = GainRamp::new(0.0);
    let mut samples = vec![1.0; 512];

    ramp.ramp_to(&mut samples, 0.6);

    assert!(
        (ramp.current() - 0.6).abs() < 1e-6,
        "the ramp ends at the target, got {}",
        ramp.current()
    );
    assert!(
        (samples[511] - 0.6).abs() < 1e-5,
        "the last written sample already carries the target gain, got {}",
        samples[511]
    );
    // The fade is monotonic: every sample is louder than the one before it.
    assert!(samples.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn the_ramp_never_steps_more_than_a_small_amount_per_sample() {
    // The anti-click rule: a full silent-to-loud fade across one buffer moves
    // the gain a little per sample, never in a jump.
    let mut ramp = GainRamp::new(0.0);
    let mut samples = vec![1.0; 512];

    ramp.ramp_to(&mut samples, 1.0);

    let mut previous = 0.0_f32;
    for (index, gain) in samples.iter().enumerate() {
        assert!(
            (gain - previous).abs() <= MAX_STEP_PER_SAMPLE,
            "sample {index} moved the gain from {previous} to {gain}"
        );
        previous = *gain;
    }
}

#[test]
fn the_ramp_stays_put_once_it_arrives() {
    let mut ramp = GainRamp::new(0.0);
    let mut samples = vec![1.0; 256];
    ramp.ramp_to(&mut samples, 0.4);

    // A second buffer at the same target changes nothing: the arrived gain is
    // the state, so there is no drift and no second fade.
    let mut samples = vec![1.0; 256];
    ramp.ramp_to(&mut samples, 0.4);
    assert!((ramp.current() - 0.4).abs() < 1e-6);
    assert!(
        samples.iter().all(|gain| (gain - 0.4).abs() < 1e-5),
        "every sample carries exactly the arrived gain"
    );
}

#[test]
fn a_seek_offset_of_n_frames_leaves_total_minus_n_frames() {
    let spec = TrackSpec {
        sample_rate: 44_100,
        channels: 2,
    };
    let path = scratch_path("seek-length");
    let total_frames = 10_000;
    let written = ramp_frames(total_frames, spec.channels, 100);
    write_wav(&path, spec, &written);

    // The whole track drains to its total length.
    let mut whole = TrackSource::open(&path).expect("the track opens");
    let mut total = 0_usize;
    while let Some(block) = whole.read_block().expect("decoding works") {
        total += block.len() / usize::from(spec.channels);
    }

    // Seeking N frames into it leaves exactly the rest.
    let skipped = 4_000_u64;
    let mut source = TrackSource::open_at(&path, skipped).expect("the seeked track opens");
    let mut remaining = Vec::new();
    while let Some(block) = source.read_block().expect("decoding works") {
        remaining.extend_from_slice(&block);
    }

    let _ = std::fs::remove_file(&path);

    let remaining_frames = remaining.len() / usize::from(spec.channels);
    let expected = total - usize::try_from(skipped).expect("small offset");
    assert!(
        remaining_frames.abs_diff(expected) <= 4,
        "seeking {skipped} frames leaves {expected} frames, got {remaining_frames}"
    );

    // And the stream continues at the right place: no silence, no rewind —
    // the first remaining sample is the one written at frame `skipped`.
    let expected_samples = as_f32(
        &written[usize::try_from(skipped).expect("small offset") * usize::from(spec.channels)..],
    );
    assert!(
        remaining
            .iter()
            .zip(&expected_samples)
            .all(|(got, want)| (got - want).abs() < PCM16_TOLERANCE),
        "the seeked stream is sample-exact from the offset on"
    );
}

#[test]
fn an_offset_past_the_end_clamps_to_the_end_instead_of_failing() {
    let spec = TrackSpec {
        sample_rate: 44_100,
        channels: 2,
    };
    let path = scratch_path("seek-clamp");
    write_wav(&path, spec, &ramp_frames(1_000, spec.channels, 100));

    let mut source =
        TrackSource::open_at(&path, 1_000_000).expect("a past-end offset clamps, it never errors");

    let _ = std::fs::remove_file(&path);

    // The stream is the end of the track: nothing left to play, but the
    // source still reports its format for the device.
    assert_eq!(source.spec(), spec);
    assert!(
        source.read_block().expect("draining works").is_none(),
        "an offset clamped to the end leaves no audio"
    );
}

#[test]
fn the_seek_target_clamps_at_both_ends() {
    let track = Some(Duration::from_secs(30));

    // Seeking back past the start lands at the start.
    assert_eq!(
        seek_target(Duration::from_secs(10), -60, track),
        Duration::ZERO
    );
    // Seeking forward past the end lands at the end.
    assert_eq!(
        seek_target(Duration::from_secs(25), 60, track),
        Duration::from_secs(30)
    );
    // Inside the track, the delta is exact.
    assert_eq!(
        seek_target(Duration::from_secs(10), 5, track),
        Duration::from_secs(15)
    );
    assert_eq!(
        seek_target(Duration::from_secs(10), -5, track),
        Duration::from_secs(5)
    );
    // Without a known duration only the lower end is clamped; the player
    // clamps a past-end offset when it opens the track.
    assert_eq!(
        seek_target(Duration::from_secs(10), -5, None),
        Duration::from_secs(5)
    );
}
