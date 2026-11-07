//! Rate conversion rules (S7, T2).
//!
//! A sequence may hold tracks at different sample rates: a 96 kHz FLAC next to a
//! 44.1 kHz MP3. The player converts each track to the output format so the
//! queue never stops, and the conversion must preserve pitch and level — a
//! resampler that shifts either is worse than the gap it was meant to remove.

use std::f64::consts::PI;

use velo::audio::resample::RateConverter;
use velo::audio::{AudioError, TrackSpec};

const CD: TrackSpec = TrackSpec {
    sample_rate: 44_100,
    channels: 2,
};
const DAT: TrackSpec = TrackSpec {
    sample_rate: 48_000,
    channels: 2,
};

/// An interleaved sine, the simplest signal whose pitch and level are readable.
fn sine(frames: usize, spec: TrackSpec, hz: f64, amplitude: f64) -> Vec<f32> {
    (0..frames)
        .flat_map(|frame| {
            #[expect(
                clippy::cast_precision_loss,
                reason = "test signals are far below f64's exact integer range"
            )]
            let time = frame as f64 / f64::from(spec.sample_rate);
            #[expect(
                clippy::cast_possible_truncation,
                reason = "test signals are sine values well inside f32's range"
            )]
            let value = (amplitude * (2.0 * PI * hz * time).sin()) as f32;
            std::iter::repeat_n(value, usize::from(spec.channels))
        })
        .collect()
}

/// Estimates pitch by counting upward zero crossings of the first channel.
fn measure_hz(samples: &[f32], spec: TrackSpec) -> f64 {
    let mono: Vec<f32> = samples
        .chunks(usize::from(spec.channels))
        .map(|frame| frame[0])
        .collect();
    let crossings = mono
        .windows(2)
        .filter(|pair| pair[0] < 0.0 && pair[1] >= 0.0)
        .count();
    #[expect(
        clippy::cast_precision_loss,
        reason = "test signals are far below f64's exact integer range"
    )]
    let seconds = mono.len() as f64 / f64::from(spec.sample_rate);
    #[expect(
        clippy::cast_precision_loss,
        reason = "a crossing count fits in f64 exactly"
    )]
    let crossing_rate = crossings as f64;
    crossing_rate / seconds
}

/// RMS of the middle half of the signal, where the resampler's edges cannot
/// flatter or punish it.
fn measure_amplitude(samples: &[f32], spec: TrackSpec) -> f64 {
    let channels = usize::from(spec.channels);
    let frames: Vec<f32> = samples.chunks(channels).map(|frame| frame[0]).collect();
    let middle = &frames[frames.len() / 4..frames.len() * 3 / 4];
    let energy: f64 = middle
        .iter()
        .map(|sample| f64::from(*sample) * f64::from(*sample))
        .sum();
    #[expect(
        clippy::cast_precision_loss,
        reason = "test signals are far below f64's exact integer range"
    )]
    let count = middle.len() as f64;
    (energy / count).sqrt() * std::f64::consts::SQRT_2
}

fn converted_frames(converter: &mut RateConverter, input: &[f32], spec: TrackSpec) -> Vec<f32> {
    let mut out = converter.convert(input).expect("conversion succeeds");
    out.extend(converter.flush().expect("flush succeeds"));
    assert_eq!(
        out.len() % usize::from(spec.channels),
        0,
        "the converted stream holds whole frames"
    );
    out
}

#[test]
#[expect(
    clippy::similar_names,
    reason = "the converter and what it returns are naturally named alike"
)]
fn an_identical_spec_is_an_exact_passthrough() {
    let input = sine(1_000, CD, 440.0, 0.5);
    let mut converter = RateConverter::new(CD, CD).expect("same rate needs no resampler");

    let converted = converter.convert(&input).expect("conversion succeeds");
    let flushed = converter.flush().expect("flush succeeds");

    assert_eq!(
        converted, input,
        "no sample is touched when nothing must change"
    );
    assert!(
        flushed.is_empty(),
        "a passthrough resampler holds nothing back for later"
    );
}

#[test]
fn a_converted_track_keeps_its_length() {
    let input_frames = 48_000; // one second at 48 kHz
    let input = sine(input_frames, DAT, 440.0, 0.5);
    let mut converter = RateConverter::new(DAT, CD).expect("48k to 44.1k is supported");

    let out = converted_frames(&mut converter, &input, CD);
    let out_frames = out.len() / usize::from(CD.channels);

    let expected = 44_100;
    let tolerance = expected / 100; // 1%: resampler latency, not lost audio
    assert!(
        out_frames.abs_diff(expected) <= tolerance,
        "one second stays one second: expected about {expected} frames, got {out_frames}"
    );
}

#[test]
fn a_converted_track_keeps_its_pitch_and_level() {
    let input = sine(48_000, DAT, 1_000.0, 0.5);
    let mut converter = RateConverter::new(DAT, CD).expect("48k to 44.1k is supported");

    let out = converted_frames(&mut converter, &input, CD);

    let measured_hz = measure_hz(&out, CD);
    assert!(
        (measured_hz - 1_000.0).abs() < 20.0,
        "a 1 kHz tone must still be 1 kHz after conversion, measured {measured_hz:.1} Hz"
    );

    let measured_amplitude = measure_amplitude(&out, CD);
    assert!(
        (measured_amplitude - 0.5).abs() < 0.02,
        "the level must survive conversion: expected ~0.5, measured {measured_amplitude:.3}"
    );
}

#[test]
fn a_track_that_fills_whole_chunks_still_flushes_its_tail() {
    // 48 chunks of 1024 frames: the converter holds nothing back, so the track's
    // entire tail arrives through flush alone and the trim must not swallow it.
    let input_frames = 48 * 1024;
    let input = sine(input_frames, DAT, 440.0, 0.5);
    let mut converter = RateConverter::new(DAT, CD).expect("48k to 44.1k is supported");

    let out = converted_frames(&mut converter, &input, CD);
    let out_frames = out.len() / usize::from(CD.channels);

    let expected = input_frames * 44_100 / 48_000;
    let tolerance = expected / 100;
    assert!(
        out_frames.abs_diff(expected) <= tolerance,
        "the tail belongs to the track: expected about {expected} frames, got {out_frames}"
    );

    let tail = &out[out.len() - 64..];
    assert!(
        tail.iter().any(|sample| sample.abs() > 0.01),
        "flushing must emit the real tail, not trim it down to silence"
    );
}

#[test]
fn a_channel_layout_change_is_rejected() {
    let stereo = TrackSpec {
        sample_rate: 44_100,
        channels: 2,
    };
    let mono = TrackSpec {
        sample_rate: 44_100,
        channels: 1,
    };

    match RateConverter::new(stereo, mono) {
        Err(AudioError::SpecMismatch { expected, got }) => {
            assert_eq!(expected, mono, "the mismatch names the output format");
            assert_eq!(got, stereo, "the mismatch names the track format");
        }
        Err(other) => panic!("expected SpecMismatch, got {other:?}"),
        Ok(_) => panic!("a layout change must not be silently accepted"),
    }
}
