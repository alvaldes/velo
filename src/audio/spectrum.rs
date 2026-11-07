//! The spectrum the interface draws (ui-redesign T5): the latest mono samples
//! the device played, windowed, transformed and folded into log-spaced bands.
//!
//! Pure and single-threaded: the samples arrive from the player's tap, which
//! the interface drains on its own thread, so nothing here ever runs in the
//! audio callback. The FFT is a plain radix-2 one — a window this size is a
//! few microseconds of work at thirty frames a second, and not worth a crate.

use std::f32::consts::PI;

/// How many samples one analysis looks at: about 43 ms at 48 kHz, fine enough
/// to tell a bass line from a kick and short enough to follow the music.
pub const WINDOW: usize = 2048;

/// The lowest and highest frequency drawn. Below 40 Hz is felt more than
/// heard; above 16 kHz most music holds nothing worth a bar.
const LOWEST_HZ: f32 = 40.0;
const HIGHEST_HZ: f32 = 16_000.0;

/// The loudness a bar spans: a band at `CEILING_DB` fills it, one at
/// `FLOOR_DB` or below draws nothing.
const FLOOR_DB: f32 = -60.0;
const CEILING_DB: f32 = -6.0;

/// How far a bar may fall per update: about half a second from full to
/// nothing at thirty updates a second. Bars rise at once.
const FALL_PER_UPDATE: f32 = 0.06;

/// The analyzer: the most recent [`WINDOW`] samples and the levels last drawn.
pub struct Spectrum {
    /// The most recent samples, oldest first once rotated by `next`.
    history: Vec<f32>,
    /// Where the next sample is written in `history`.
    next: usize,
    /// Whether samples arrived since the last update; without new music the
    /// bars only fall.
    fresh: bool,
    levels: Vec<f32>,
}

impl Default for Spectrum {
    fn default() -> Self {
        Self::new()
    }
}

impl Spectrum {
    /// An analyzer that has heard nothing.
    #[must_use]
    pub fn new() -> Self {
        Self {
            history: vec![0.0; WINDOW],
            next: 0,
            fresh: false,
            levels: Vec::new(),
        }
    }

    /// Keeps the newest samples; only the last [`WINDOW`] matter.
    pub fn push(&mut self, samples: &[f32]) {
        if samples.is_empty() {
            return;
        }
        let newest = &samples[samples.len().saturating_sub(WINDOW)..];
        for &sample in newest {
            self.history[self.next] = sample;
            self.next = (self.next + 1) % WINDOW;
        }
        self.fresh = true;
    }

    /// Recomputes `bands` levels from the newest window. Without new samples
    /// since the last update, every bar falls instead.
    pub fn update(&mut self, bands: usize, sample_rate: u32) {
        if self.levels.len() != bands {
            self.levels = vec![0.0; bands];
        }
        let targets = if self.fresh {
            self.analyze(bands, sample_rate)
        } else {
            vec![0.0; bands]
        };
        self.fresh = false;
        for (level, target) in self.levels.iter_mut().zip(targets) {
            *level = target.max(*level - FALL_PER_UPDATE).max(0.0);
        }
    }

    /// The levels last computed, `0.0..=1.0`, lowest band first.
    #[must_use]
    pub fn levels(&self) -> &[f32] {
        &self.levels
    }

    /// The level of every band in the newest window, before smoothing.
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "bin indices and window sizes are small and positive"
    )]
    fn analyze(&self, bands: usize, sample_rate: u32) -> Vec<f32> {
        let mut real: Vec<f32> = (0..WINDOW)
            .map(|n| {
                let sample = self.history[(self.next + n) % WINDOW];
                let hann = 0.5 * (1.0 - (2.0 * PI * n as f32 / (WINDOW - 1) as f32).cos());
                sample * hann
            })
            .collect();
        let mut imaginary = vec![0.0; WINDOW];
        fft(&mut real, &mut imaginary);

        // A sine of amplitude A peaks at A·N/4 under a Hann window: scaled
        // back, a bar reads the amplitude of the strongest partial in it.
        let scale = 4.0 / WINDOW as f32;
        let bin_hz = sample_rate as f32 / WINDOW as f32;
        let last_bin = WINDOW / 2 - 1;
        (0..bands)
            .map(|band| {
                let (low, high) = band_range(band, bands, sample_rate);
                let first = ((low / bin_hz).round() as usize).clamp(1, last_bin);
                let last = ((high / bin_hz).round() as usize).clamp(first, last_bin);
                let amplitude = (first..=last)
                    .map(|bin| real[bin].hypot(imaginary[bin]) * scale)
                    .fold(0.0_f32, f32::max);
                let decibels = 20.0 * amplitude.max(1e-9).log10();
                ((decibels - FLOOR_DB) / (CEILING_DB - FLOOR_DB)).clamp(0.0, 1.0)
            })
            .collect()
    }
}

/// The frequencies band `band` of `bands` covers, in Hz: log-spaced from
/// 40 Hz to 16 kHz (or the Nyquist frequency, when that is lower), each band
/// starting exactly where the one below it ends.
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    reason = "band counts and sample rates are exact in f32"
)]
pub fn band_range(band: usize, bands: usize, sample_rate: u32) -> (f32, f32) {
    let highest = HIGHEST_HZ.min(sample_rate as f32 / 2.0);
    let edge =
        |index: usize| LOWEST_HZ * (highest / LOWEST_HZ).powf(index as f32 / bands.max(1) as f32);
    (edge(band), edge(band + 1))
}

/// An in-place iterative radix-2 FFT. Both slices hold `WINDOW` values, a
/// power of two.
#[expect(
    clippy::cast_precision_loss,
    reason = "transform lengths are small powers of two"
)]
fn fft(real: &mut [f32], imaginary: &mut [f32]) {
    let n = real.len();
    debug_assert!(n.is_power_of_two() && imaginary.len() == n);

    // Bit-reversal permutation.
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            real.swap(i, j);
            imaginary.swap(i, j);
        }
    }

    // Butterflies, doubling the span each pass.
    let mut span = 2;
    while span <= n {
        let angle = -2.0 * PI / span as f32;
        let (step_sin, step_cos) = angle.sin_cos();
        for start in (0..n).step_by(span) {
            let (mut twiddle_re, mut twiddle_im) = (1.0_f32, 0.0_f32);
            for k in 0..span / 2 {
                let even = start + k;
                let odd = even + span / 2;
                let odd_re = real[odd] * twiddle_re - imaginary[odd] * twiddle_im;
                let odd_im = real[odd] * twiddle_im + imaginary[odd] * twiddle_re;
                real[odd] = real[even] - odd_re;
                imaginary[odd] = imaginary[even] - odd_im;
                real[even] += odd_re;
                imaginary[even] += odd_im;
                let next_re = twiddle_re * step_cos - twiddle_im * step_sin;
                twiddle_im = twiddle_re * step_sin + twiddle_im * step_cos;
                twiddle_re = next_re;
            }
        }
        span <<= 1;
    }
}
