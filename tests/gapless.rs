//! Gapless sequencing rules for `velo`'s audio path (S7).
//!
//! These tests pin the observable contract of the audio layer without touching a
//! sound device: a sequence of tracks must play back as one continuous stream,
//! with no silence inserted, no padding and no dropped frames at the join.

use std::path::{Path, PathBuf};

use velo::audio::{self, AudioError, TrackSpec};

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

/// A unique scratch path per test and file, cleaned up by the caller.
fn scratch_path(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("velo-{}-{tag}.wav", std::process::id()))
}

fn constant_frames(frames: usize, channels: u16, value: i16) -> Vec<i16> {
    vec![value; frames * usize::from(channels)]
}

/// A per-frame ramp in a range that never reaches zero, so a dropped, duplicated
/// or reordered frame changes the sample values and cannot hide behind a constant.
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

#[test]
fn gapless_join_inserts_no_silence_and_drops_no_frames() {
    let spec = TrackSpec {
        sample_rate: 44_100,
        channels: 2,
    };
    let first = scratch_path("gapless-a");
    let second = scratch_path("gapless-b");

    let a_frames = 1_000;
    let b_frames = 1_000;
    let a_samples = ramp_frames(a_frames, spec.channels, 100);
    let b_samples = ramp_frames(b_frames, spec.channels, 1_000);
    write_wav(&first, spec, &a_samples);
    write_wav(&second, spec, &b_samples);

    let (drained_spec, samples) =
        audio::drain_gapless(&[first.clone(), second.clone()]).expect("two valid tracks drain");

    let _ = std::fs::remove_file(&first);
    let _ = std::fs::remove_file(&second);

    assert_eq!(drained_spec, spec, "the sequence reports the device spec");

    let mut written = a_samples.clone();
    written.extend_from_slice(&b_samples);
    let expected = as_f32(&written);
    assert_eq!(
        samples.len(),
        expected.len(),
        "exactly {a_frames}+{b_frames} frames: no padding and no dropped frames"
    );
    assert!(
        samples
            .iter()
            .zip(&expected)
            .all(|(got, want)| (got - want).abs() < PCM16_TOLERANCE),
        "the join is sample-exact: no frame was dropped, duplicated or reordered"
    );
    assert!(
        samples.iter().all(|sample| *sample != 0.0),
        "no sample is silent: no silence was inserted at the join"
    );

    let join = a_frames * usize::from(spec.channels);
    assert!((samples[join - 1] - expected[join - 1]).abs() < PCM16_TOLERANCE);
    assert!(
        (samples[join] - expected[join]).abs() < PCM16_TOLERANCE,
        "the first sample after the join is already track B"
    );
}

#[test]
fn a_single_track_drains_without_padding() {
    let spec = TrackSpec {
        sample_rate: 48_000,
        channels: 1,
    };
    let only = scratch_path("single");
    let frames = 777;
    let written = constant_frames(frames, spec.channels, 42);
    write_wav(&only, spec, &written);

    let (drained_spec, samples) =
        audio::drain_gapless(std::slice::from_ref(&only)).expect("one valid track");

    let _ = std::fs::remove_file(&only);

    assert_eq!(drained_spec, spec);
    assert_eq!(
        samples.len(),
        written.len(),
        "one track keeps its frame count"
    );
    assert!(samples.iter().all(|sample| *sample > 0.0));
}

#[test]
fn a_spec_change_between_tracks_is_rejected() {
    let first = scratch_path("mismatch-a");
    let second = scratch_path("mismatch-b");
    let stereo = TrackSpec {
        sample_rate: 44_100,
        channels: 2,
    };
    let mono = TrackSpec {
        sample_rate: 48_000,
        channels: 1,
    };

    write_wav(&first, stereo, &constant_frames(64, stereo.channels, 1));
    write_wav(&second, mono, &constant_frames(64, mono.channels, 1));

    let result = audio::drain_gapless(&[first.clone(), second.clone()]);

    let _ = std::fs::remove_file(&first);
    let _ = std::fs::remove_file(&second);

    match result {
        Err(AudioError::SpecMismatch { expected, got }) => {
            assert_eq!(expected, stereo, "the mismatch names the sequence spec");
            assert_eq!(got, mono, "the mismatch names the offending track");
        }
        Err(other) => panic!("expected SpecMismatch, got {other:?}"),
        Ok((_, samples)) => panic!(
            "a spec change must not be silently accepted; drained {} samples",
            samples.len()
        ),
    }
}
