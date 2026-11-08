//! Bit-perfect playback rules (T17, S7, S12): with `bit_perfect = true` the
//! samples leaving the decoder reach the device unmodified — no resampling,
//! no `ReplayGain`, no software volume.
//!
//! The reopen decision is a pure function over two `TrackSpec`s and the mode,
//! tested on its own. The audio proof runs through
//! [`velo::audio::output::drain_headless_bit_perfect`], the same sequence loop
//! the producer thread runs: a drained bit-perfect stream must be
//! *bit-identical* to the decoder's own output. The interface rules are driven
//! on `App`, which never needs a device here.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use lofty::config::WriteOptions;
use lofty::tag::TagType;
use lofty::tag::{ItemKey, Tag, TagExt as _};

use velo::audio::TrackSource;
use velo::audio::output::{
    Entry, TrackFailure, drain_headless_bit_perfect, drain_headless_with_gain,
};
use velo::audio::{TrackSpec, output::needs_stream_reopen};
use velo::config::ReplayGainMode;
use velo::ui::App;

const RATE_44K: TrackSpec = TrackSpec {
    sample_rate: 44_100,
    channels: 2,
};
const RATE_48K: TrackSpec = TrackSpec {
    sample_rate: 48_000,
    channels: 2,
};

// ---- the pure decision ------------------------------------------------------

#[test]
fn equal_specs_never_reopen_the_stream() {
    // Consecutive tracks that share a spec stay one continuous stream, under
    // both modes: reopening for no reason would put a gap in every album.
    assert!(!needs_stream_reopen(RATE_44K, RATE_44K, true));
    assert!(!needs_stream_reopen(RATE_44K, RATE_44K, false));
}

#[test]
fn a_rate_difference_reopens_only_when_bit_perfect_is_on() {
    // Without the setting the rate difference is converted away, as always.
    assert!(!needs_stream_reopen(RATE_44K, RATE_48K, false));
    assert!(!needs_stream_reopen(RATE_48K, RATE_44K, false));
    // With it, the stream is closed and reopened at the track's rate.
    assert!(needs_stream_reopen(RATE_44K, RATE_48K, true));
    assert!(needs_stream_reopen(RATE_48K, RATE_44K, true));
}

#[test]
fn a_layout_difference_never_reopens() {
    // Channel layout maps instead (T20, L25): mono into a stereo device is a
    // device constraint, not a DSP choice, and it must not cost a stream swap.
    let mono = TrackSpec {
        sample_rate: 44_100,
        channels: 1,
    };
    assert!(!needs_stream_reopen(RATE_44K, mono, true));
    let five_point_one = TrackSpec {
        sample_rate: 44_100,
        channels: 6,
    };
    assert!(!needs_stream_reopen(RATE_44K, five_point_one, true));
}

// ---- the headless audio proof ----------------------------------------------

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
    fs::write(path, out).expect("write test wav");
}

/// A unique scratch path per test and file, removed by the caller.
fn scratch_path(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("velo-bit-perfect-{tag}-{}.wav", std::process::id()))
}

/// A per-frame ramp in a range that never reaches zero, so a resampled or
/// scaled stream cannot hide behind a constant.
fn ramp_frames(frames: usize, channels: u16, offset: i16) -> Vec<i16> {
    (0..frames)
        .flat_map(|frame| {
            let value = offset + i16::try_from(frame % 500).expect("small ramp") + 1;
            std::iter::repeat_n(value, usize::from(channels))
        })
        .collect()
}

/// Tags `path` with `ReplayGain` values through lofty, the way a `ReplayGain`
/// tagger would.
fn tag_with(path: &Path, values: &[(ItemKey, &str)]) {
    let mut tag = Tag::new(TagType::Id3v2);
    for (key, value) in values {
        tag.insert_text(*key, (*value).to_owned());
    }
    tag.save_to_path(path, WriteOptions::default())
        .expect("tag the fixture");
}

/// What the decoder itself produces, untouched by anything downstream: the
/// standard every bit-perfect drain is measured against.
fn decoder_output(path: &Path) -> Vec<f32> {
    let mut source = TrackSource::open(path).expect("the fixture decodes");
    let mut samples = Vec::new();
    while let Some(block) = source.read_block().expect("decode") {
        samples.extend_from_slice(&block);
    }
    samples
}

/// Drains one queue under bit-perfect playback, panicking on skips: these
/// fixtures are all decodable.
fn drain_bit_perfect(queue: Vec<PathBuf>, expected: TrackSpec, mode: ReplayGainMode) -> Vec<f32> {
    let mut samples = Vec::new();
    let stop = AtomicBool::new(false);
    drain_headless_bit_perfect(
        Arc::new(Mutex::new(queue.into_iter().map(Entry::from).collect())),
        0,
        0,
        expected,
        mode,
        &stop,
        &mut |block: &[f32]| samples.extend_from_slice(block),
        &mut |failure: TrackFailure| panic!("nothing may fail here: {failure:?}"),
    )
    .expect("the drain runs");
    samples
}

#[test]
fn bit_perfect_plays_a_track_at_its_own_rate_without_resampling() {
    // The stream is running at 44.1 kHz and the track is 48 kHz: without the
    // setting the track is resampled into it; with it, the stream follows the
    // track and the decoder's samples pass through untouched.
    let path = scratch_path("rate");
    write_wav(&path, RATE_48K, &ramp_frames(4_000, 2, 100));

    let heard = drain_bit_perfect(vec![path.clone()], RATE_44K, ReplayGainMode::Track);
    let want = decoder_output(&path);
    let _ = fs::remove_file(&path);

    assert_eq!(
        heard.len(),
        want.len(),
        "no resampling: the drained length is the decoder's own frame count"
    );
    assert_eq!(heard, want, "the drained samples are bit-identical");
}

#[test]
fn bit_perfect_skips_replaygain() {
    // A tagged track must reach the device at its decoded level, not at the
    // gain's: scaling samples is exactly what bit-perfect refuses.
    let path = scratch_path("tagged");
    write_wav(&path, RATE_44K, &ramp_frames(4_000, 2, 100));
    tag_with(&path, &[(ItemKey::ReplayGainTrackGain, "-6.02 dB")]);

    let heard = drain_bit_perfect(vec![path.clone()], RATE_44K, ReplayGainMode::Track);
    let want = decoder_output(&path);
    let _ = fs::remove_file(&path);

    assert_eq!(heard.len(), want.len());
    assert_eq!(
        heard, want,
        "the tagged track is bit-identical too: the gain was skipped"
    );
}

#[test]
fn a_queue_of_mixed_rates_plays_every_track_at_its_own_rate() {
    // The boundary is the point of the mode: the 44.1 kHz track plays as it
    // is, the stream reopens once for the two 48 kHz tracks, and neither of
    // them is resampled. Consecutive tracks sharing a spec stay one stream.
    let first = scratch_path("mixed-44k");
    let second = scratch_path("mixed-48k-a");
    let third = scratch_path("mixed-48k-b");
    write_wav(&first, RATE_44K, &ramp_frames(1_000, 2, 100));
    write_wav(&second, RATE_48K, &ramp_frames(1_000, 2, 1_000));
    write_wav(&third, RATE_48K, &ramp_frames(1_000, 2, 2_000));

    let heard = drain_bit_perfect(
        vec![first.clone(), second.clone(), third.clone()],
        RATE_44K,
        ReplayGainMode::Off,
    );
    let mut want = decoder_output(&first);
    want.extend(decoder_output(&second));
    want.extend(decoder_output(&third));
    let _ = fs::remove_file(&first);
    let _ = fs::remove_file(&second);
    let _ = fs::remove_file(&third);

    assert_eq!(heard.len(), want.len());
    assert_eq!(heard, want, "every track reached the drain unmodified");
}

#[test]
fn turning_bit_perfect_off_restores_the_conversion_and_the_gain() {
    // The same fixture pair the bit-perfect tests pass with: without the
    // setting, the 48 kHz track is resampled into the 44.1 kHz stream and the
    // tagged track is scaled by its gain — the pre-T17 behaviour, restored.
    let path = scratch_path("off");
    write_wav(&path, RATE_48K, &ramp_frames(4_000, 2, 100));
    tag_with(&path, &[(ItemKey::ReplayGainTrackGain, "-6.02 dB")]);

    let mut samples = Vec::new();
    let stop = AtomicBool::new(false);
    drain_headless_with_gain(
        Arc::new(Mutex::new(vec![Entry::File(path.clone())])),
        0,
        0,
        RATE_44K,
        ReplayGainMode::Track,
        &stop,
        &mut |block: &[f32]| samples.extend_from_slice(block),
        &mut |failure: TrackFailure| panic!("nothing may fail here: {failure:?}"),
    )
    .expect("the drain runs");
    let decoded = decoder_output(&path);
    let _ = fs::remove_file(&path);

    // Converted: the length is the 44.1 kHz one, not the decoder's 48 kHz one.
    let converted = usize::try_from(4_000_u64 * 44_100 / 48_000).expect("small") * 2;
    assert_ne!(samples.len(), decoded.len(), "the rate was converted");
    assert!(
        samples.len().abs_diff(converted) <= 2,
        "resampled to the stream's rate: {} against ~{converted}",
        samples.len()
    );
    // Gained: about half the decoded level, -6.02 dB being the half step;
    // the resampler's overshoot puts the measured peak a little above it.
    let peak_decoded = decoded.iter().fold(0.0_f32, |max, s| max.max(s.abs()));
    let peak_heard = samples.iter().fold(0.0_f32, |max, s| max.max(s.abs()));
    assert!(
        (peak_heard / peak_decoded - 0.5).abs() < 0.1,
        "the gain was applied: peak ratio {}",
        peak_heard / peak_decoded
    );
}

// ---- the interface: the volume is pinned, and it says so -------------------

#[test]
fn bit_perfect_pins_the_volume_to_100_and_says_so_once() {
    let mut app = App::new(Vec::new());
    app.set_bit_perfect(true);

    app.set_volume(70);
    assert_eq!(app.volume(), 100, "the config's volume must not survive");
    let said = app.status().to_owned();
    assert!(
        said.contains("bit-perfect") && said.contains("100%"),
        "one status line must explain the pinning, got: {said}"
    );

    // And it stays pinned — `+`/`-` cannot scale samples either — without
    // saying it a second time.
    app.set_volume(50);
    assert_eq!(app.volume(), 100);
    assert_eq!(app.status(), said, "the explanation is given once");
}

#[test]
fn bit_perfect_skips_replaygain_in_the_interface_too() {
    let mut app = App::new(Vec::new());
    app.set_bit_perfect(true);
    app.set_replay_gain(ReplayGainMode::Track);
    assert_eq!(
        app.replay_gain(),
        ReplayGainMode::Off,
        "the interface must not claim a gain it does not apply"
    );
}

#[test]
fn without_bit_perfect_the_volume_still_moves() {
    let mut app = App::new(Vec::new());
    app.set_volume(70);
    assert_eq!(app.volume(), 70, "the old behaviour, unchanged");
    assert_eq!(app.status(), "", "no bit-perfect line without the setting");
}
