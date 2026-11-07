//! `ReplayGain` rules (S15, T11): parse the tags, choose a gain, cap it against
//! the declared peak, and apply it in the audio path.
//!
//! The parsing and choosing tests are pure. The application tests drive
//! [`drain_headless_with_gain`], the same loop the producer thread runs, so
//! the proof reaches the audio rather than stopping at numbers.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use lofty::config::WriteOptions;
use lofty::tag::TagType;
use lofty::tag::{ItemKey, Tag, TagExt as _};

use velo::audio::TrackSpec;
use velo::audio::output::{TrackFailure, drain_headless, drain_headless_with_gain};
use velo::audio::replay_gain::{Gain, Loudness, capped_gain, parse_db, parse_peak};
use velo::config::ReplayGainMode;

/// The linear factor a `ReplayGain` dB value stands for: `10^(dB / 20)`, the
/// conversion the producer's own gain must apply.
fn linear(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

/// A loudness table the choosing tests build from (gain, peak) pairs.
fn loudness(track: (Option<f32>, Option<f32>), album: (Option<f32>, Option<f32>)) -> Loudness {
    Loudness {
        track_gain_db: track.0,
        track_peak: track.1,
        album_gain_db: album.0,
        album_peak: album.1,
    }
}

// ---- parsing ---------------------------------------------------------------

#[test]
fn a_db_value_parses_to_its_number_and_the_player_applies_the_linear_factor() {
    // Parsing is pure: strings in, numbers out — the dB number as written.
    assert_eq!(parse_db("-7.35 dB"), Some(-7.35));
    // The producer turns it into a linear factor: -6.02 dB, the ReplayGain
    // half-loudness step, is 10^(-6.02/20), i.e. just about one half.
    let gain = Gain::for_track(
        ReplayGainMode::Track,
        Some(&loudness((Some(-6.02), None), (None, None))),
    );
    assert!(
        (gain.factor() - linear(-6.02)).abs() < 1e-6,
        "-6.02 dB must become its own linear factor, got {}",
        gain.factor()
    );
    assert!((gain.factor() - 0.5).abs() < 0.002);
}

#[test]
fn a_db_value_without_a_unit_or_with_extra_whitespace_parses() {
    let planar = parse_db("-7.35");
    let no_unit = parse_db("-7.35 dB");
    assert_eq!(planar, no_unit);
    assert_eq!(parse_db(" -6.02dB "), parse_db("-6.02 dB"));
    assert_eq!(parse_db("\t+3.5 dB\n"), parse_db("+3.5 dB"));
}

#[test]
fn garbage_yields_no_gain_instead_of_a_wrong_one() {
    assert_eq!(parse_db("loud"), None);
    assert_eq!(parse_db(""), None);
    assert_eq!(parse_db("   "), None);
    assert_eq!(parse_db("dB"), None);
    assert_eq!(parse_db("-7.35 dB and a half"), None);
    // `inf` and `NaN` parse as floats; neither is a gain.
    assert_eq!(parse_db("inf dB"), None);
    assert_eq!(parse_db("NaN"), None);
}

#[test]
fn a_peak_parses_as_a_positive_number() {
    // A real-world tag value; the expected literal keeps the test tidy.
    assert_eq!(parse_peak(" 0.987654 "), Some(0.987_654));
    assert_eq!(parse_peak("1.0"), Some(1.0));
    assert_eq!(parse_peak(" noise "), None);
    assert_eq!(parse_peak("-0.4"), None, "a negative peak is nonsense");
    assert_eq!(parse_peak("inf"), None);
    assert_eq!(parse_peak(""), None);
}

// ---- the clipping cap ------------------------------------------------------

#[test]
fn a_cap_that_lands_exactly_on_full_scale_stays() {
    // peak 0.5 × gain 2.0 = 1.0 exactly, and both numbers are exact in f32.
    assert_eq!(capped_gain(2.0, Some(0.5)), 2.0);
}

#[test]
fn a_cap_a_micron_above_full_scale_is_reduced() {
    let over = f32::next_up(2.0);
    // 1/0.5 is exactly 2.0 in f32, so the cap reproduces full scale exactly.
    assert_eq!(capped_gain(over, Some(0.5)), 2.0);
    assert_eq!(capped_gain(3.0, Some(0.5)), 2.0);
}

#[test]
fn an_unknown_peak_applies_the_gain_as_read() {
    assert_eq!(capped_gain(2.5, None), 2.5);
}

// ---- choosing --------------------------------------------------------------

#[test]
fn off_applies_nothing() {
    let gain = Gain::for_track(
        ReplayGainMode::Off,
        Some(&loudness((Some(-6.02), Some(0.9)), (Some(0.0), Some(1.0)))),
    );
    assert!(!gain.applies(), "`off` must not touch the samples");
    assert_eq!(gain.factor(), 1.0);
    assert_eq!(Gain::for_track(ReplayGainMode::Off, None), Gain::identity());
}

#[test]
fn track_mode_uses_the_track_tags() {
    let gain = Gain::for_track(
        ReplayGainMode::Track,
        Some(&loudness((Some(-6.02), None), (Some(-3.01), None))),
    );
    assert!(
        (gain.factor() - linear(-6.02)).abs() < 1e-6,
        "`track` must use the track gain, got {}",
        gain.factor()
    );
}

#[test]
fn track_mode_without_a_track_tag_applies_nothing() {
    let gain = Gain::for_track(
        ReplayGainMode::Track,
        Some(&loudness((None, None), (Some(-3.01), None))),
    );
    assert_eq!(gain, Gain::identity());
    assert_eq!(
        Gain::for_track(ReplayGainMode::Track, None),
        Gain::identity()
    );
}

#[test]
fn album_mode_prefers_the_album_tags() {
    let gain = Gain::for_track(
        ReplayGainMode::Album,
        Some(&loudness(
            (Some(-6.02), Some(0.9)),
            (Some(-3.01), Some(0.8)),
        )),
    );
    assert!(
        (gain.factor() - linear(-3.01)).abs() < 1e-6,
        "`album` must prefer the album gain, got {}",
        gain.factor()
    );
}

#[test]
fn album_gain_falls_back_to_track_gain_when_absent() {
    let gain = Gain::for_track(
        ReplayGainMode::Album,
        Some(&loudness((Some(-6.02), None), (None, None))),
    );
    assert!(
        (gain.factor() - linear(-6.02)).abs() < 1e-6,
        "album mode without album tags falls back to the track gain, got {}",
        gain.factor()
    );
    assert_eq!(
        Gain::for_track(ReplayGainMode::Album, None),
        Gain::identity()
    );
}

#[test]
fn a_positive_gain_is_capped_by_its_declared_peak() {
    // +6.02 dB would double a track whose peak is 0.8 → 1.6: capped at 1.25.
    let gain = Gain::for_track(
        ReplayGainMode::Track,
        Some(&loudness((Some(6.02), Some(0.8)), (None, None))),
    );
    assert!(
        (gain.factor() - 1.25).abs() < 1e-3,
        "the gain must be capped so peak × gain stays at 1.0, got {}",
        gain.factor()
    );
}

// ---- reading from a real file ----------------------------------------------

/// A unique scratch path per test and file, removed by the caller.
fn scratch_path(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("velo-replay-gain-{tag}-{}.wav", std::process::id()))
}

/// Writes a WAV whose every frame carries the same `amplitude`.
fn write_constant_wav(path: &Path, spec: TrackSpec, frames: usize, amplitude: i16) -> Vec<i16> {
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
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&spec.channels.to_le_bytes());
    out.extend_from_slice(&spec.sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits_per_sample.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    let mut written = Vec::with_capacity(frames * usize::from(spec.channels));
    for _ in 0..frames {
        for _ in 0..spec.channels {
            out.extend_from_slice(&amplitude.to_le_bytes());
            written.push(amplitude);
        }
    }
    fs::write(path, out).expect("write test wav");
    written
}

/// Tags `path` with `ReplayGain` values through lofty, the same way a
/// `ReplayGain` tagger would.
fn tag_with(path: &Path, values: &[(ItemKey, &str)]) {
    let mut tag = Tag::new(TagType::Id3v2);
    for (key, value) in values {
        tag.insert_text(*key, (*value).to_owned());
    }
    tag.save_to_path(path, WriteOptions::default())
        .expect("tag the fixture");
}

/// The `f32` value one `i16` PCM sample decodes to.
fn as_f32(sample: i16) -> f32 {
    f32::from(sample) / 32_768.0
}

#[test]
fn a_file_without_replaygain_tags_yields_no_gain() {
    let path = scratch_path("untagged");
    write_constant_wav(
        &path,
        TrackSpec {
            sample_rate: 44_100,
            channels: 2,
        },
        32,
        100,
    );

    let gain = Gain::read_from(&path, ReplayGainMode::Track);
    let _ = fs::remove_file(&path);

    assert_eq!(gain, Gain::identity(), "no tags, no gain");
}

#[test]
fn a_file_with_an_untidy_or_nonsense_tag_yields_no_gain() {
    let path = scratch_path("garbage-tag");
    write_constant_wav(
        &path,
        TrackSpec {
            sample_rate: 44_100,
            channels: 2,
        },
        32,
        100,
    );
    tag_with(
        &path,
        &[
            (ItemKey::ReplayGainTrackGain, "not a number"),
            (ItemKey::ReplayGainTrackPeak, "0.9"),
        ],
    );
    let gain = Gain::read_from(&path, ReplayGainMode::Track);
    let _ = fs::remove_file(&path);

    assert_eq!(gain, Gain::identity(), "a nonsense tag is no gain");
}

#[test]
fn the_track_tags_a_fixture_carries_reach_the_gain() {
    let spec = TrackSpec {
        sample_rate: 44_100,
        channels: 2,
    };
    let path = scratch_path("tagged");
    write_constant_wav(&path, spec, 32, 100);
    tag_with(
        &path,
        &[
            (ItemKey::ReplayGainTrackGain, "-3.01 dB"),
            (ItemKey::ReplayGainTrackPeak, "0.5"),
        ],
    );

    let track = Gain::read_from(&path, ReplayGainMode::Track);
    let album = Gain::read_from(&path, ReplayGainMode::Album);
    let _ = fs::remove_file(&path);

    assert!(
        (track.factor() - linear(-3.01)).abs() < 1e-6,
        "the track tags are read and applied, got {}",
        track.factor()
    );
    assert_eq!(album, track, "album mode falls back to the same track gain");
}

// ---- applying in the audio path -------------------------------------------

fn drain_with(queue: Vec<PathBuf>, mode: ReplayGainMode) -> Vec<f32> {
    let mut samples = Vec::new();
    let stop = AtomicBool::new(false);
    drain_headless_with_gain(
        Arc::new(Mutex::new(queue)),
        0,
        0,
        TrackSpec {
            sample_rate: 44_100,
            channels: 2,
        },
        mode,
        &stop,
        &mut |block: &[f32]| samples.extend_from_slice(block),
        &mut |failure: TrackFailure| panic!("nothing may fail here: {failure:?}"),
    )
    .expect("the drain runs");
    samples
}

#[test]
fn a_tagged_track_reaches_the_audio_at_half_amplitude() {
    const AMPLITUDE: i16 = 20_000;
    let spec = TrackSpec {
        sample_rate: 44_100,
        channels: 2,
    };
    let path = scratch_path("audible");
    write_constant_wav(&path, spec, 64, AMPLITUDE);
    tag_with(&path, &[(ItemKey::ReplayGainTrackGain, "-6.02 dB")]);

    let heard = drain_with(vec![path.clone()], ReplayGainMode::Track);
    let _ = fs::remove_file(&path);

    let want = as_f32(AMPLITUDE);
    // The exact linear factor the producer computes from -6.02 dB.
    let factor = Gain::for_track(
        ReplayGainMode::Track,
        Some(&loudness((Some(-6.02), None), (None, None))),
    )
    .factor();
    // -6.02 dB is the ReplayGain half-step: every frame comes out at half the
    // amplitude it went in with, both channels on its way into the ring.
    assert_eq!(heard.len(), 64 * 2, "the whole track was drained");
    for (index, sample) in heard.iter().enumerate() {
        let expected = want * factor;
        assert!(
            (sample - expected).abs() < 1e-4,
            "sample {index} should be {expected}, got {sample}"
        );
    }
    assert!(
        ((want * factor) * 2.0 - want).abs() < 0.01,
        "the applied gain halves the amplitude: {} against {}",
        want * factor,
        want
    );
}

#[test]
fn off_leaves_the_same_track_unchanged() {
    let spec = TrackSpec {
        sample_rate: 44_100,
        channels: 2,
    };
    let path = scratch_path("audible-off");
    let mut written = write_constant_wav(&path, spec, 64, 20_000);
    tag_with(&path, &[(ItemKey::ReplayGainTrackGain, "-6.02 dB")]);

    let heard = drain_with(vec![path.clone()], ReplayGainMode::Off);

    let _ = fs::remove_file(&path);

    let want: Vec<f32> = written.drain(..).map(as_f32).collect();
    assert_eq!(heard.len(), want.len(), "the whole track was drained");
    for (index, (got, want)) in heard.iter().zip(&want).enumerate() {
        assert!(
            (got - want).abs() < 1e-6,
            "with `off` sample {index} must be untouched: {want}, got {got}"
        );
    }
}

#[test]
fn a_gain_applies_to_every_track_of_a_queue_not_once_for_it() {
    // Two copies of the same tagged track: the second one must carry the gain
    // too, because a gain belongs to a track and is recomputed when it changes.
    let spec = TrackSpec {
        sample_rate: 44_100,
        channels: 2,
    };
    let first = scratch_path("queue-a");
    let second = scratch_path("queue-b");
    write_constant_wav(&first, spec, 32, 20_000);
    write_constant_wav(&second, spec, 32, 20_000);
    tag_with(&first, &[(ItemKey::ReplayGainTrackGain, "-6.02 dB")]);
    tag_with(&second, &[(ItemKey::ReplayGainTrackGain, "-6.02 dB")]);

    // tag_with_value writes to both files in place.
    let heard = drain_with(vec![first.clone(), second.clone()], ReplayGainMode::Track);
    let _ = fs::remove_file(&first);
    let _ = fs::remove_file(&second);

    let want = as_f32(20_000)
        * Gain::for_track(
            ReplayGainMode::Track,
            Some(&loudness((Some(-6.02), None), (None, None))),
        )
        .factor();
    assert_eq!(heard.len(), 64 * 2);
    for (index, sample) in heard.iter().enumerate() {
        assert!(
            (sample - want).abs() < 1e-4,
            "sample {index} should carry the track gain {want}, got {sample}"
        );
    }
}

// ---- what existed still stands --------------------------------------------

#[test]
fn a_queue_drained_the_untagged_way_still_decodes_exactly() {
    // The pre-ReplayGain contract of `drain_headless` on an untagged file: the
    // drained samples are exactly the decoder's own output.
    let spec = TrackSpec {
        sample_rate: 44_100,
        channels: 2,
    };
    let path = scratch_path("preserve");
    let mut written = write_constant_wav(&path, spec, 100, 15_000);

    let mut samples: Vec<f32> = Vec::new();
    let stop = AtomicBool::new(false);
    drain_headless(
        Arc::new(Mutex::new(vec![path.clone()])),
        0,
        0,
        spec,
        &stop,
        &mut |block: &[f32]| samples.extend_from_slice(block),
        &mut |failure: TrackFailure| panic!("nothing may fail here: {failure:?}"),
    )
    .expect("the drain runs");
    let _ = fs::remove_file(&path);

    let want: Vec<f32> = written.drain(..).map(as_f32).collect();
    assert_eq!(samples, want, "an untagged track must be sample-exact");
}
