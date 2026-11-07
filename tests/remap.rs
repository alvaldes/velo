//! A device has one channel layout and a library does not. Mono is everywhere —
//! voice memos, old recordings — and refusing it made a fifth of a real folder
//! unplayable, taking the whole album down when one such file sat in a queue.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use velo::audio::TrackSpec;
use velo::audio::output::{TrackFailure, drain_headless};
use velo::audio::resample::remap_channels;

fn spec(channels: u16) -> TrackSpec {
    TrackSpec {
        sample_rate: 44_100,
        channels,
    }
}

#[test]
fn mono_is_duplicated_into_every_output_channel() {
    let mapped = remap_channels(&[0.25, -0.5, 0.0], 1, 2).expect("mono fits any layout");
    assert_eq!(mapped, vec![0.25, 0.25, -0.5, -0.5, 0.0, 0.0]);
}

#[test]
fn several_channels_are_averaged_into_mono() {
    let mapped = remap_channels(&[0.2, 0.4, -0.6, 0.0], 2, 1).expect("stereo folds to mono");
    let expected = [f32::midpoint(0.2, 0.4), f32::midpoint(-0.6, 0.0)];
    assert_eq!(mapped.len(), 2);
    for (got, want) in mapped.iter().zip(expected) {
        assert!((got - want).abs() < 1e-6, "got {got}, want {want}");
    }
}

#[test]
fn matching_layouts_pass_through_unchanged() {
    let mapped = remap_channels(&[0.1, 0.2, 0.3, 0.4], 2, 2).expect("same layout");
    assert_eq!(mapped, vec![0.1, 0.2, 0.3, 0.4]);
}

#[test]
fn a_surround_layout_is_refused_rather_than_guessed() {
    // Six channels into two is a downmix with real coefficients — a product
    // decision, not something to invent by averaging.
    assert!(remap_channels(&[0.0; 12], 6, 2).is_err());
}

fn scratch(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("velo-remap-{name}-{}", std::process::id()))
}

/// Writes a canonical 16-bit PCM WAV with `channels` channels, silently.
fn write_wav(path: &PathBuf, channels: u16, frames: u32) {
    let data_len = frames * u32::from(channels) * 2;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&44_100u32.to_le_bytes());
    bytes.extend_from_slice(&(44_100 * u32::from(channels) * 2).to_le_bytes());
    bytes.extend_from_slice(&(channels * 2).to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    bytes.resize(44 + data_len as usize, 0);
    std::fs::write(path, bytes).expect("write fixture");
}

#[test]
fn a_mono_track_plays_into_a_stereo_device() {
    let mono = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mono-tone.wav");
    let stop = AtomicBool::new(false);
    let mut samples: Vec<f32> = Vec::new();
    let mut failures: Vec<TrackFailure> = Vec::new();

    drain_headless(
        vec![mono],
        0,
        0,
        spec(2),
        &stop,
        &mut |block| samples.extend_from_slice(block),
        &mut |failure| failures.push(failure),
    )
    .expect("a mono file must play into a stereo device");

    assert!(failures.is_empty(), "mono is not a failure: {failures:?}");
    assert_eq!(samples.len(), 22_050, "one frame in, two samples out");
    for frame in 0..samples.len() / 2 {
        assert_eq!(
            samples[frame * 2],
            samples[frame * 2 + 1],
            "a duplicated channel must be identical"
        );
    }
    assert!(
        samples.iter().any(|sample| sample.abs() > 0.1),
        "the tone must survive the mapping"
    );
}

#[test]
fn a_track_the_device_cannot_hold_is_skipped_not_fatal() {
    let surround = scratch("surround.wav");
    write_wav(&surround, 6, 1_024);
    let stop = AtomicBool::new(false);
    let mut samples: Vec<f32> = Vec::new();
    let mut failures: Vec<TrackFailure> = Vec::new();

    let result = drain_headless(
        vec![surround.clone()],
        0,
        0,
        spec(2),
        &stop,
        &mut |block| samples.extend_from_slice(block),
        &mut |failure| failures.push(failure),
    );

    assert!(
        result.is_ok(),
        "one impossible track must not fail the queue"
    );
    assert!(samples.is_empty(), "it produced nothing to play");
    assert_eq!(failures.len(), 1, "but it was announced exactly once");
    assert_eq!(failures[0].path, surround);
    let _ = std::fs::remove_file(&surround);
}
