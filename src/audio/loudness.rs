//! Integrated loudness, EBU R128 / ITU-R BS.1770-4 (T13, S15): the meter
//! behind `velo --replaygain-scan`.
//!
//! ## What is measured
//!
//! The signal is K-weighted (BS.1770-4's high-shelf plus RLB high-pass biquads)
//! and cut into **400 ms blocks with 75 % overlap** (a 100 ms hop). A block's
//! loudness is `−0.691 + 10·log₁₀(Σₖ wₖ·mean-squareₖ)` with the standard's
//! channel weights (L, R, C = 1.0; Ls, Rs = 1.41; anything else = 1.0).
//!
//! **Gating** is the standard's two-stage rule: blocks below −70 LUFS are
//! dropped (absolute gate); the mean power of the survivors sets a relative
//! threshold 10 LU below its own loudness; blocks at or below it are dropped
//! too; the integrated loudness is the mean power of what remains. This is
//! what makes a loud chorus survive eight quiet bars around it.
//!
//! ## Where the coefficients come from
//!
//! At 48 kHz the biquads are the table printed in BS.1770-4 §5. At any other
//! sample rate the same analog parameters — a 1 681.97 Hz shelf with +4.0 dB
//! gain (Q = 0.7072) and a 38.13 Hz high-pass (Q = 0.5003) — are re-derived
//! with the bilinear transform, exactly as `libebur128` and `pyloudnorm` do;
//! a unit test pins the 48 kHz values against the standard's table.
//!
//! ## Honesty
//!
//! * **Peak is true peak** — the largest absolute value of a 4× oversampled
//!   version of the signal, per BS.1770-4 Annex 2: an inter-sample overshoot
//!   that a sample peak misses (a peak that under-reports is a clipping cap
//!   that lets a track clip). The plain sample peak is reported alongside it.
//!   The interpolation filter is the standard's own: an order-48, 4-phase FIR
//!   interpolator whose 12 coefficients per phase are taken **verbatim from
//!   the table printed in Rec. ITU-R BS.1770-4, Annex 2** (each an exact
//!   dyadic rational, so `f64` holds them exactly). The Annex's 12.04 dB
//!   attenuation step is skipped — it exists for integer arithmetic, and this
//!   meter is floating point, as the Annex itself notes.
//! * A file with no audio (or digital silence — every block below the
//!   absolute gate) yields **no measurement**, never a number.
//! * Blocks are only formed from full 400 ms windows: the first 300 ms of a
//!   track are never zero-padded into a block.
//!
//! ## Album gain is a second measurement
//!
//! Album gain is not an average of track gains: the album's blocks are pooled
//! across its files and gated together ([`integrated_album`]). That is the
//! whole point of album gain — one number for the loudness of the album as a
//! program.

use std::collections::VecDeque;
use std::path::Path;

use crate::audio::{AudioError, TrackSource, TrackSpec};

/// The level every measured gain aims at: `ReplayGain` 2.0's −18 LUFS.
///
/// A constant with a doc comment, deliberately not a config key: the config
/// surface around loudness belongs to another task.
pub const TARGET_LUFS: f64 = -18.0;

/// BS.1770's absolute gate: blocks below this are dropped before anything else.
const ABSOLUTE_GATE_LUFS: f64 = -70.0;

/// The relative gate's offset: 10 LU below the absolute-gated mean.
const RELATIVE_GATE_LU: f64 = -10.0;

/// The constant in BS.1770's loudness formula.
const OFFSET_LUFS: f64 = -0.691;

/// One gated block is 400 ms of K-weighted audio.
const BLOCK_SECS: f64 = 0.4;

/// With 75 % overlap, a new block starts every 100 ms.
const HOP_SECS: f64 = 0.1;

/// How many samples a span of seconds is at `rate`, rounded. A sample rate
/// that rounds past a `u32` is absurd; the clamp only keeps it defined.
fn frames_for(secs: f64, rate: u32) -> u32 {
    #[expect(
        clippy::cast_sign_loss,
        reason = "a negative span cannot occur; the cast saturates at 0"
    )]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the fraction is already rounded away"
    )]
    let frames = (secs * f64::from(rate)) as u64;
    u32::try_from(frames).unwrap_or(u32::MAX)
}

/// BS.1770's channel weights: L, R, C at unity; the surrounds at +1.5 dB.
fn channel_weight(channel: usize) -> f64 {
    match channel {
        3 | 4 => 1.41,
        _ => 1.0,
    }
}

/// One biquad section, direct form I.
#[derive(Debug, Clone, Copy)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Biquad {
    fn process(&self, state: &mut BiquadState, x: f64) -> f64 {
        let y = self.b0 * x + self.b1 * state.x1 + self.b2 * state.x2
            - self.a1 * state.y1
            - self.a2 * state.y2;
        state.x2 = state.x1;
        state.x1 = x;
        state.y2 = state.y1;
        state.y1 = y;
        y
    }
}

/// The four delays of one biquad section.
#[derive(Debug, Default, Clone, Copy)]
struct BiquadState {
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

/// BS.1770-4 Annex 2's true-peak interpolation filter, verbatim: an order-48,
/// 4-phase FIR interpolator, 12 coefficients per phase, exactly as printed in
/// the table of Rec. ITU-R BS.1770-4, Annex 2. Every coefficient is an exact
/// dyadic rational, so these `f64` literals are the standard's numbers without
/// rounding. Each phase's coefficients sum to ≈ 1 (the quantised DC gain:
/// 1.0015869140625 for phases 0 and 3, 0.9730224609375 for phases 1 and 2),
/// and phase 2 is phase 1 time-reversed and phase 3 is phase 0 time-reversed —
/// the linear-phase prototype the table decomposes.
const TRUE_PEAK_FILTER: [[f64; 12]; 4] = [
    [
        0.001_708_984_375,
        0.010_986_328_125,
        -0.019_653_320_312_5,
        0.033_203_125,
        -0.059_448_242_187_5,
        0.137_329_101_562_5,
        0.972_167_968_75,
        -0.102_294_921_875,
        0.047_607_421_875,
        -0.026_611_328_125,
        0.014_892_578_125,
        -0.008_300_781_25,
    ],
    [
        -0.029_174_804_687_5,
        0.029_296_875,
        -0.051_757_812_5,
        0.089_111_328_125,
        -0.166_503_906_25,
        0.465_087_890_625,
        0.779_785_156_25,
        -0.200_317_382_812_5,
        0.101_562_5,
        -0.058_227_539_062_5,
        0.033_081_054_687_5,
        -0.018_920_898_437_5,
    ],
    [
        -0.018_920_898_437_5,
        0.033_081_054_687_5,
        -0.058_227_539_062_5,
        0.101_562_5,
        -0.200_317_382_812_5,
        0.779_785_156_25,
        0.465_087_890_625,
        -0.166_503_906_25,
        0.089_111_328_125,
        -0.051_757_812_5,
        0.029_296_875,
        -0.029_174_804_687_5,
    ],
    [
        -0.008_300_781_25,
        0.014_892_578_125,
        -0.026_611_328_125,
        0.047_607_421_875,
        -0.102_294_921_875,
        0.972_167_968_75,
        0.137_329_101_562_5,
        -0.059_448_242_187_5,
        0.033_203_125,
        -0.019_653_320_312_5,
        0.010_986_328_125,
        0.001_708_984_375,
    ],
];

/// How many input samples one phase of [`TRUE_PEAK_FILTER`] spans.
const TRUE_PEAK_TAPS: usize = 12;

/// The 4× oversampler of BS.1770-4 Annex 2, per channel: every input sample
/// yields the signal's four values on the 4× grid, computed from the last 12
/// input samples. The filter's group delay only shifts where the estimates
/// sit in time, which peak measurement does not care about; the input sample
/// itself is always part of the maximum as well.
#[derive(Debug, Default, Clone)]
struct Interpolator {
    history: [f64; TRUE_PEAK_TAPS],
}

impl Interpolator {
    /// Consumes one sample, returns its four 4× grid values.
    fn push(&mut self, sample: f64) -> [f64; 4] {
        self.history.copy_within(1.., 0);
        self.history[TRUE_PEAK_TAPS - 1] = sample;
        let mut out = [0.0; 4];
        for (phase, value) in TRUE_PEAK_FILTER.iter().zip(&mut out) {
            *value = phase
                .iter()
                .zip(self.history.iter().rev())
                .map(|(coefficient, sample)| coefficient * sample)
                .sum();
        }
        out
    }
}

/// BS.1770-4's K-weighting: a high shelf into a high-pass, per channel.
#[derive(Debug, Clone, Copy)]
struct KWeighting {
    shelf: Biquad,
    high_pass: Biquad,
}

impl KWeighting {
    /// The weighting for one sample rate.
    ///
    /// The parameters are the standard's analog prototype (a +4.0 dB shelf at
    /// 1 681.97 Hz, Q = 0.7072; a high-pass at 38.13 Hz, Q = 0.5003), turned
    /// into biquads with the bilinear transform. At 48 kHz this reproduces the
    /// standard's own coefficient table exactly — see the test below.
    fn new(rate: u32) -> Self {
        Self {
            shelf: Self::shelf(rate),
            high_pass: Self::high_pass(rate),
        }
    }

    /// The high shelf: +4 dB above ≈ 1 682 Hz.
    fn shelf(rate: u32) -> Biquad {
        let f0 = 1_681.974_450_955_533;
        let gain_db = 3.999_843_853_973_347;
        let q = 0.707_175_236_955_419_6;
        let k = f64::tan(std::f64::consts::PI * f0 / f64::from(rate));
        let vh = 10.0_f64.powf(gain_db / 20.0);
        let vb = vh.powf(0.499_666_774_154_541_6);
        let a0 = 1.0 + k / q + k * k;
        Biquad {
            b0: (vh + vb * k / q + k * k) / a0,
            b1: 2.0 * (k * k - vh) / a0,
            b2: (vh - vb * k / q + k * k) / a0,
            a1: 2.0 * (k * k - 1.0) / a0,
            a2: (1.0 - k / q + k * k) / a0,
        }
    }

    /// The RLB high-pass at ≈ 38 Hz.
    fn high_pass(rate: u32) -> Biquad {
        let f0 = 38.135_470_876_024_44;
        let q = 0.500_327_037_323_877_3;
        let k = f64::tan(std::f64::consts::PI * f0 / f64::from(rate));
        let a0 = 1.0 + k / q + k * k;
        Biquad {
            b0: 1.0,
            b1: -2.0,
            b2: 1.0,
            a1: 2.0 * (k * k - 1.0) / a0,
            a2: (1.0 - k / q + k * k) / a0,
        }
    }
}

/// One channel's filter and window state.
#[derive(Debug, Default)]
struct Channel {
    shelf: BiquadState,
    high_pass: BiquadState,
    /// The K-weighted samples of the current 400 ms window.
    window: VecDeque<f64>,
    /// The running sum of squares over `window`.
    sum_sq: f64,
    /// The 4× oversampler that estimates the true peak.
    interpolator: Interpolator,
}

/// One measured value: an integrated loudness, the file's sample peak and its
/// true peak.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Measurement {
    /// The gated integrated loudness, in LUFS.
    pub integrated_lufs: f64,
    /// Sample peak: the largest absolute decoded sample.
    pub sample_peak: f32,
    /// True peak: the largest absolute value of the 4× oversampled signal
    /// (BS.1770-4 Annex 2's filter — see the module docs). It catches the
    /// inter-sample overshoot a sample peak misses, and is at least the
    /// sample peak.
    pub true_peak: f32,
}

impl Measurement {
    /// The `ReplayGain` 2.0 gain this measurement asks for: target minus measured.
    #[must_use]
    pub fn gain_db(&self) -> f64 {
        TARGET_LUFS - self.integrated_lufs
    }
}

/// Accumulates K-weighted block powers and a sample peak, file by file.
///
/// Feed it decoded, interleaved samples with [`LoudnessMeter::push`] (the same
/// blocks the player decodes, priming trimmed, via [`crate::audio::TrackSource`])
/// and read the gated result with [`LoudnessMeter::integrated`]. Sample rates
/// may change between pushes: the window restarts, the block powers collected
/// so far stay.
#[derive(Debug, Default)]
pub struct LoudnessMeter {
    spec: Option<TrackSpec>,
    filters: Vec<KWeighting>,
    channels: Vec<Channel>,
    block_len: u32,
    hop: u32,
    seen: u64,
    blocks: Vec<f64>,
    peak: f32,
    /// The largest absolute oversampled value so far, in f64 for exactness.
    true_peak: f64,
}

impl LoudnessMeter {
    /// An empty meter.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            spec: None,
            filters: Vec::new(),
            channels: Vec::new(),
            block_len: 0,
            hop: 0,
            seen: 0,
            blocks: Vec::new(),
            peak: 0.0,
            true_peak: 0.0,
        }
    }

    /// Feeds one block of decoded, interleaved samples.
    pub fn push(&mut self, samples: &[f32], spec: TrackSpec) {
        if self.spec != Some(spec) {
            // A new file, or (rarely) a rate change inside one: the window
            // restarts at the new format; the powers already collected stay.
            self.block_len = frames_for(BLOCK_SECS, spec.sample_rate);
            self.hop = frames_for(HOP_SECS, spec.sample_rate);
            self.filters = (0..usize::from(spec.channels))
                .map(|_| KWeighting::new(spec.sample_rate))
                .collect();
            self.channels = (0..usize::from(spec.channels))
                .map(|_| Channel::default())
                .collect();
            self.seen = 0;
            self.spec = Some(spec);
        }

        let channel_count = usize::from(spec.channels);
        let block_len = u64::from(self.block_len);
        let hop = u64::from(self.hop);
        let block_len_f64 = f64::from(self.block_len);
        let block_len_usize = self.block_len as usize;
        for frame in samples.chunks_exact(channel_count) {
            for (channel, &sample) in frame.iter().enumerate() {
                let magnitude = sample.abs();
                if magnitude > self.peak {
                    self.peak = magnitude;
                }
                for value in self.channels[channel].interpolator.push(f64::from(sample)) {
                    let magnitude = value.abs();
                    if magnitude > self.true_peak {
                        self.true_peak = magnitude;
                    }
                }
                let filtered = self.filters[channel]
                    .shelf
                    .process(&mut self.channels[channel].shelf, f64::from(sample));
                let filtered = self.filters[channel]
                    .high_pass
                    .process(&mut self.channels[channel].high_pass, filtered);
                let channel_state = &mut self.channels[channel];
                channel_state.sum_sq += filtered * filtered;
                channel_state.window.push_back(filtered);
                if channel_state.window.len() > block_len_usize
                    && let Some(oldest) = channel_state.window.pop_front()
                {
                    channel_state.sum_sq -= oldest * oldest;
                }
            }
            // A block is emitted at every hop boundary once the first full
            // window exists — checked per frame, because one push can span
            // several hops.
            self.seen += 1;
            if self.seen >= block_len && (self.seen - block_len).is_multiple_of(hop) {
                let power: f64 = self
                    .channels
                    .iter()
                    .enumerate()
                    .map(|(channel, channel_state)| {
                        channel_weight(channel) * channel_state.sum_sq / block_len_f64
                    })
                    .sum();
                self.blocks.push(power);
            }
        }
    }

    /// The per-block channel-weighted powers collected so far: what
    /// [`integrated_album`] pools across a whole album.
    #[must_use]
    pub fn blocks(&self) -> &[f64] {
        &self.blocks
    }

    /// The sample peak seen so far.
    #[must_use]
    pub const fn sample_peak(&self) -> f32 {
        self.peak
    }

    /// The gated integrated loudness, or `None` when no block survives the
    /// absolute gate — no audio, or digital silence.
    #[must_use]
    pub fn integrated(&self) -> Option<Measurement> {
        integrated_from(&self.blocks, self.peak, self.true_peak)
    }
}

/// Gates a run of block powers into one integrated loudness (see the module
/// docs for the rule). `None` when nothing survives the absolute gate. The
/// reported true peak is the larger of the oversampled peak and the plain
/// sample peak.
#[must_use]
pub fn integrated_from(blocks: &[f64], sample_peak: f32, true_peak: f64) -> Option<Measurement> {
    let loudness = |power: f64| OFFSET_LUFS + 10.0 * power.log10();
    let gated: Vec<f64> = blocks
        .iter()
        .copied()
        .filter(|power| loudness(*power) > ABSOLUTE_GATE_LUFS)
        .collect();
    if gated.is_empty() {
        return None;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "a file holds far fewer blocks than f64 counts exactly"
    )]
    let mean = gated.iter().sum::<f64>() / gated.len() as f64;
    let relative_threshold = OFFSET_LUFS + 10.0 * mean.log10() + RELATIVE_GATE_LU;
    let over: Vec<f64> = gated
        .into_iter()
        .filter(|power| loudness(*power) > relative_threshold)
        .collect();
    // Unreachable in exact arithmetic — the loudest block always clears its
    // own mean by more than 10 LU — but never a panic in floating point.
    if over.is_empty() {
        return None;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "a file holds far fewer blocks than f64 counts exactly"
    )]
    let integrated = OFFSET_LUFS + 10.0 * (over.iter().sum::<f64>() / over.len() as f64).log10();
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the peak is reported as f32 like every ReplayGain peak"
    )]
    let true_peak = true_peak.max(f64::from(sample_peak)) as f32;
    Some(Measurement {
        integrated_lufs: integrated,
        sample_peak,
        true_peak,
    })
}

/// Integrates a whole album: every meter's blocks pooled and gated together,
/// the peaks the loudest file's. This is a second measurement, not an average
/// of track measurements — that is the point of album gain.
#[must_use]
pub fn integrated_album<'a>(
    meters: impl IntoIterator<Item = &'a LoudnessMeter>,
) -> Option<Measurement> {
    let mut blocks = Vec::new();
    let mut peak = 0.0_f32;
    let mut true_peak = 0.0_f64;
    for meter in meters {
        blocks.extend_from_slice(meter.blocks());
        peak = peak.max(meter.sample_peak());
        true_peak = true_peak.max(meter.true_peak);
    }
    integrated_from(&blocks, peak, true_peak)
}

/// Decodes a whole file into a meter, through the player's own
/// [`TrackSource`] — the same content the listener hears, priming trimmed.
///
/// # Errors
///
/// Returns the errors of [`TrackSource::open`] and [`TrackSource::read_block`].
pub fn meter_for(path: &Path) -> Result<LoudnessMeter, AudioError> {
    let mut source = TrackSource::open(path)?;
    let mut meter = LoudnessMeter::new();
    while let Some(block) = source.read_block()? {
        meter.push(&block, source.spec());
    }
    Ok(meter)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_48khz_coefficients_reproduce_the_standard_table() {
        // The table printed in BS.1770-4 §5, to nine decimal places.
        let k = KWeighting::new(48_000);
        assert!((k.shelf.b0 - 1.535_124_859_586_97).abs() < 1e-9);
        assert!((k.shelf.b1 - (-2.691_696_189_406_38)).abs() < 1e-9);
        assert!((k.shelf.b2 - 1.198_392_810_852_85).abs() < 1e-9);
        assert!((k.shelf.a1 - (-1.690_659_293_182_41)).abs() < 1e-9);
        assert!((k.shelf.a2 - 0.732_480_774_215_85).abs() < 1e-9);
        assert!((k.high_pass.b0 - 1.0).abs() < 1e-12);
        assert!((k.high_pass.b1 - (-2.0)).abs() < 1e-12);
        assert!((k.high_pass.b2 - 1.0).abs() < 1e-12);
        assert!((k.high_pass.a1 - (-1.990_047_454_833_98)).abs() < 1e-9);
        assert!((k.high_pass.a2 - 0.990_072_250_366_21).abs() < 1e-9);
    }

    #[test]
    fn the_absolute_gate_drops_silence_rather_than_reporting_it() {
        // Digital silence: every power is 0, every loudness −∞, nothing passes.
        let blocks = vec![0.0_f64; 50];
        assert!(integrated_from(&blocks, 0.0, 0.0).is_none());
    }

    // ---- true peak (BS.1770-4 Annex 2) ------------------------------------

    #[test]
    fn the_annex_2_table_is_the_linear_phase_prototype_it_claims_to_be() {
        // Phases 2 and 3 are phases 1 and 0 time-reversed: a transcription
        // error in any single coefficient breaks this. The DC gains are the
        // exact sums of the printed (dyadic) coefficients.
        assert_eq!(TRUE_PEAK_FILTER[3], {
            let mut phase = TRUE_PEAK_FILTER[0];
            phase.reverse();
            phase
        });
        assert_eq!(TRUE_PEAK_FILTER[2], {
            let mut phase = TRUE_PEAK_FILTER[1];
            phase.reverse();
            phase
        });
        assert_eq!(TRUE_PEAK_FILTER[0].iter().sum::<f64>(), 1.001_586_914_062_5);
        assert_eq!(TRUE_PEAK_FILTER[1].iter().sum::<f64>(), 0.973_022_460_937_5);
    }

    #[test]
    fn the_interpolator_reproduces_dc_at_each_phase_printed_gain() {
        // Constant 1.0 in: once the 12-sample history is full, each phase
        // outputs exactly the sum of its own printed coefficients — the
        // quantised DC gains of the standard's table (1.0015869140625 for
        // phases 0 and 3, 0.9730224609375 for phases 1 and 2).
        let mut interpolator = Interpolator::default();
        let mut last = [0.0; 4];
        for _ in 0..30 {
            last = interpolator.push(1.0);
        }
        for (phase, value) in TRUE_PEAK_FILTER.iter().zip(last) {
            let dc = phase.iter().sum::<f64>();
            assert!(
                (value - dc).abs() < 1e-9,
                "phase must sit at its own DC gain {dc}, got {value}"
            );
        }
    }

    #[test]
    fn the_meter_reads_a_true_peak_above_the_sample_peak_on_the_annex_example() {
        // Annex 2, Appendix 1's own example: a full-scale tone at a quarter
        // of the sampling rate, phased so every sample sits at full scale,
        // peaks √2 higher between the samples. Pushed directly, no file.
        let mut meter = LoudnessMeter::new();
        let samples: Vec<f32> = (0..44_100)
            .map(|n| {
                let n = f64::from(n);
                let sine = std::f64::consts::SQRT_2
                    * (std::f64::consts::FRAC_PI_4 + std::f64::consts::FRAC_PI_2 * n).sin();
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "the fixture's samples are f32 like every decoded sample"
                )]
                let sample = sine as f32;
                sample
            })
            .collect();
        let stereo: Vec<f32> = samples.iter().flat_map(|&s| [s, s]).collect();
        meter.push(
            &stereo,
            TrackSpec {
                sample_rate: 44_100,
                channels: 2,
            },
        );

        let measurement = meter.integrated().expect("a loud sine measures");
        assert!(
            measurement.sample_peak <= 1.0,
            "every sample sits at full scale, got {}",
            measurement.sample_peak
        );
        assert!(
            measurement.true_peak > 1.35,
            "the oversampled signal must reach ≈ √2, got {}",
            measurement.true_peak
        );
    }
}
