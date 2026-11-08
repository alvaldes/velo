//! Integrated loudness (T13, S15): EBU R128 / ITU-R BS.1770-4 measurement.
//!
//! The anchors are honest ones: at 1 kHz the K-weighting response is +0.70 dB,
//! which cancels BS.1770's −0.691 LUFS offset, so a *stereo* 1 kHz sine with
//! each channel at −N dBFS peak integrates to −N LUFS. Every fixture here is
//! written to `std::env::temp_dir()`, never to a real library.

use std::fs;
use std::path::{Path, PathBuf};

use velo::audio::TrackSpec;
use velo::audio::loudness::{TARGET_LUFS, integrated_album, meter_for};

const STEREO: TrackSpec = TrackSpec {
    sample_rate: 44_100,
    channels: 2,
};

/// A unique scratch path per test, removed by the caller.
fn scratch_path(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("velo-loudness-{tag}-{}.wav", std::process::id()))
}

/// Writes a 16-bit PCM WAV whose sample at (`frame`, `channel`) is the value
/// `sample` returns, in i16 units. Caller removes `path` afterwards.
fn write_wav(path: &Path, spec: TrackSpec, frames: usize, sample: impl Fn(usize, usize) -> f64) {
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
    for frame in 0..frames {
        for channel in 0..usize::from(spec.channels) {
            let value = sample(frame, channel).round();
            #[expect(
                clippy::cast_possible_truncation,
                reason = "the try_from below clamps the rounded amplitude"
            )]
            let rounded = value as i64;
            let value = i16::try_from(rounded).expect("amplitude stays in 16-bit range");
            out.extend_from_slice(&value.to_le_bytes());
        }
    }
    fs::write(path, out).expect("write test wav");
}

/// A stereo 1 kHz sine whose every channel peaks at `amplitude` (i16 units).
fn sine(frame: usize, amplitude: f64) -> f64 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "test fixtures stay far below u32::MAX frames"
    )]
    let frame = frame as u32;
    let t = f64::from(frame) / f64::from(STEREO.sample_rate);
    amplitude * (2.0 * std::f64::consts::PI * 1_000.0 * t).sin()
}

// ---- the known answers ------------------------------------------------------

#[test]
fn a_stereo_1khz_sine_at_minus_20_dbfs_integrates_to_minus_20_lufs() {
    // 0.1 full scale = −20 dBFS per channel; K(1 kHz) +0.70 dB and the −0.691
    // LUFS offset cancel, so the integrated value is the dBFS level itself.
    let path = scratch_path("minus20");
    write_wav(&path, STEREO, 2 * 44_100, |frame, _| sine(frame, 3_276.8));

    let meter = meter_for(&path).expect("the file decodes");
    let _ = fs::remove_file(&path);
    let measurement = meter.integrated().expect("a sine has a loudness");

    assert!(
        (measurement.integrated_lufs - (-20.0)).abs() < 0.1,
        "a −20 dBFS stereo sine must integrate to −20 LUFS, got {}",
        measurement.integrated_lufs
    );
    // ReplayGain 2.0 gain against velo's −18 LUFS target.
    let gain = measurement.gain_db();
    assert!(
        (gain - 2.0).abs() < 0.1,
        "target −18 against −20 LUFS is +2 dB, got {gain}"
    );
    assert!(
        (measurement.sample_peak - 3_276.8 / 32_768.0).abs() < 1e-3,
        "sample peak is the sine's own amplitude, got {}",
        measurement.sample_peak
    );
    assert_eq!(TARGET_LUFS, -18.0, "ReplayGain 2.0's target is −18 LUFS");
}

#[test]
fn a_stereo_1khz_sine_at_minus_6_dbfs_integrates_to_minus_6_lufs() {
    let path = scratch_path("minus6");
    write_wav(&path, STEREO, 2 * 44_100, |frame, _| sine(frame, 16_384.0));

    let meter = meter_for(&path).expect("the file decodes");
    let _ = fs::remove_file(&path);
    let measurement = meter.integrated().expect("a sine has a loudness");

    assert!(
        (measurement.integrated_lufs - (-6.0)).abs() < 0.1,
        "a −6 dBFS stereo sine must integrate to −6 LUFS, got {}",
        measurement.integrated_lufs
    );
}

#[test]
fn a_file_with_no_audio_returns_no_measurement() {
    // A WAV with a zero-length data chunk: it decodes, and measures nothing.
    let path = scratch_path("empty");
    write_wav(&path, STEREO, 0, |_, _| 0.0);

    let meter = meter_for(&path).expect("an empty file still opens");
    let _ = fs::remove_file(&path);

    assert!(
        meter.blocks().is_empty(),
        "no audio means no gated blocks, got {}",
        meter.blocks().len()
    );
    assert!(
        meter.integrated().is_none(),
        "no audio must return no measurement rather than a number"
    );
}

// ---- true peak versus sample peak -------------------------------------------

#[test]
fn a_full_scale_quarter_rate_sine_reads_a_true_peak_well_above_its_sample_peak() {
    // BS.1770-4 Annex 2's own example (Appendix 1): a tone at a quarter of
    // the sampling frequency, phased so every sample lands at full scale,
    // really peaks √2 (≈ +3 dB) higher *between* the samples. The sample peak
    // stays at 1.0; only the 4× oversampled true peak sees the overshoot.
    let path = scratch_path("true-peak");
    let sqrt2 = std::f64::consts::SQRT_2;
    write_wav(&path, STEREO, 2 * 44_100, |frame, _| {
        let n = f64::from(u32::try_from(frame).expect("test frames fit u32"));
        32_767.0 * sqrt2 * (std::f64::consts::FRAC_PI_4 + std::f64::consts::FRAC_PI_2 * n).sin()
    });

    let meter = meter_for(&path).expect("the file decodes");
    let _ = fs::remove_file(&path);
    let measurement = meter.integrated().expect("a sine has a loudness");
    assert!(
        measurement.sample_peak <= 1.0,
        "the sample peak is every sample's own magnitude, at or under full scale, got {}",
        measurement.sample_peak
    );
    assert!(
        measurement.true_peak > 1.35,
        "the true peak must catch the √2 inter-sample overshoot (got {} against a sample peak of {})",
        measurement.true_peak,
        measurement.sample_peak
    );
    assert!(
        measurement.true_peak > measurement.sample_peak,
        "true peak {} must exceed the sample peak {} — that difference is the feature",
        measurement.true_peak,
        measurement.sample_peak
    );
}

#[test]
fn a_low_sine_shows_only_a_fraction_of_a_decibel_of_true_peak_overshoot() {
    // Control: a 1 kHz tone's crests land on samples, so the oversampled peak
    // agrees with the sample peak to a fraction of a percent (measured ≈ 0.008
    // dB apart) — the filter is not inflating peaks wholesale.
    let path = scratch_path("true-peak-control");
    let amplitude = 29_222.0; // ≈ −1 dBFS
    write_wav(&path, STEREO, 2 * 44_100, |frame, _| sine(frame, amplitude));

    let meter = meter_for(&path).expect("the file decodes");
    let _ = fs::remove_file(&path);
    let measurement = meter.integrated().expect("a sine has a loudness");
    assert!(
        measurement.true_peak < measurement.sample_peak * 1.02,
        "a 1 kHz sine's true peak {} must sit within 2 % of its sample peak {}",
        measurement.true_peak,
        measurement.sample_peak
    );
}

// ---- the relative gate ------------------------------------------------------

#[test]
fn the_relative_gate_follows_the_loud_part_not_the_average() {
    // Ten seconds: eight quiet (−40 LUFS) and two loud (−12 LUFS). The
    // un-gated average sits near −19 LUFS; the relative gate must hand back
    // something close to the loud part's own −12 LUFS.
    let path = scratch_path("gated");
    let loud_from = 8 * 44_100;
    let quiet = 327.68; // ≈ −40 dBFS per channel
    let loud = 8_230.96; // ≈ −12 dBFS per channel
    write_wav(&path, STEREO, 10 * 44_100, |frame, _| {
        sine(frame, if frame >= loud_from { loud } else { quiet })
    });

    let meter = meter_for(&path).expect("the file decodes");
    let _ = fs::remove_file(&path);
    let measurement = meter.integrated().expect("the loud part has a loudness");

    assert!(
        (measurement.integrated_lufs - (-12.0)).abs() < 0.5,
        "the gate must follow the loud fifth, got {} LUFS",
        measurement.integrated_lufs
    );
    // And prove the gate did the work: without it the whole file averages far
    // below the loud part.
    let blocks = meter.blocks();
    let un_gated = -0.691 + 10.0 * blocks.iter().sum::<f64>().log10()
        - 10.0 * f64::from(u32::try_from(blocks.len()).expect("few blocks")).log10();
    assert!(
        measurement.integrated_lufs > un_gated + 5.0,
        "integrated {} must sit far above the un-gated average {un_gated}",
        measurement.integrated_lufs
    );
}

// ---- album versus track -----------------------------------------------------

#[test]
fn album_gain_is_a_second_measurement_between_the_track_gains() {
    // Two tracks at −20 and −14 LUFS: each keeps its own track gain, and the
    // album gain — the pooled blocks gated together — sits between them.
    let quiet_path = scratch_path("album-quiet");
    let loud_path = scratch_path("album-loud");
    write_wav(&quiet_path, STEREO, 44_100, |frame, _| sine(frame, 3_276.8));
    write_wav(&loud_path, STEREO, 44_100, |frame, _| sine(frame, 6_538.0));

    let quiet = meter_for(&quiet_path).expect("the file decodes");
    let loud = meter_for(&loud_path).expect("the file decodes");
    let _ = fs::remove_file(&quiet_path);
    let _ = fs::remove_file(&loud_path);

    let quiet_track = quiet.integrated().expect("measured");
    let loud_track = loud.integrated().expect("measured");
    let quiet_gain = quiet_track.gain_db();
    let loud_gain = loud_track.gain_db();
    assert!(
        (quiet_gain - 2.0).abs() < 0.1,
        "track gain for −20 LUFS, got {quiet_gain}"
    );
    assert!(
        (loud_gain - (-4.0)).abs() < 0.1,
        "track gain for −14 LUFS, got {loud_gain}"
    );

    let album = integrated_album([&quiet, &loud]).expect("two measured tracks");
    let album_gain = album.gain_db();
    assert!(
        album_gain < quiet_gain && album_gain > loud_gain,
        "album gain {album_gain} must sit between the track gains {quiet_gain} and {loud_gain}"
    );
    // The pooled −20 and −14 LUFS halves average to ≈ −16.04 LUFS.
    assert!(
        (album.integrated_lufs - (-16.04)).abs() < 0.1,
        "the album integrates its pooled blocks, got {}",
        album.integrated_lufs
    );
    assert_eq!(
        album.sample_peak, loud_track.sample_peak,
        "the album peak is the loudest track's peak"
    );
}

#[test]
fn a_meter_is_its_own_album_of_one() {
    let path = scratch_path("album-of-one");
    write_wav(&path, STEREO, 44_100, |frame, _| sine(frame, 3_276.8));
    let meter = meter_for(&path).expect("the file decodes");
    let _ = fs::remove_file(&path);

    let alone = meter.integrated().expect("measured");
    let album = integrated_album([&meter]).expect("measured");
    assert_eq!(alone.integrated_lufs, album.integrated_lufs);
    assert_eq!(alone.sample_peak, album.sample_peak);
}
