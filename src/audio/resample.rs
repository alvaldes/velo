//! Sample-rate conversion between a track and the output format (S7, T2).
//!
//! A queue that mixes rates — a 96 kHz FLAC next to a 44.1 kHz MP3 — must play
//! as one stream, so every track is converted to the output spec instead of
//! stopping between tracks. Converting a rate preserves pitch and level;
//! remixing a channel layout preserves neither, so a layout change stays an
//! error rather than a silent re-panning.

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Async, FixedAsync, Indexing, Resampler, SincInterpolationParameters, WindowFunction};

use super::{AudioError, TrackSpec};

/// Input frames per resampler chunk. Matches the decoder's block size, so a
/// converted track normally moves one chunk per [`RateConverter::convert`]
/// call; shorter trailing input is handled by [`RateConverter::flush`].
const CHUNK_FRAMES: usize = 1024;

/// Sinc filter length. Longer filters pass frequencies closer to Nyquist at
/// the cost of a longer startup delay; 256 is rubato's default trade-off.
const SINC_LEN: usize = 256;

/// Converts interleaved f32 blocks from one [`TrackSpec`] to another that may
/// differ only in sample rate.
pub struct RateConverter {
    from: TrackSpec,
    to: TrackSpec,
    /// `None` when both specs are identical: nothing may be touched or held
    /// back, and even a bit-exact resampler cannot promise that.
    resampler: Option<Async<f32>>,
    /// Interleaved input not yet fed to the resampler, because it holds less
    /// than one full chunk.
    pending: Vec<f32>,
    /// Silence, reused for every flush chunk. The resampler needs input past
    /// the last real sample to push its final output out.
    silence: Vec<f32>,
    /// Output frames still to drop: the resampler's first `output_delay`
    /// frames are its own ramp-in from assumed silence, not the track, and
    /// keeping them would smear every track start and shift the timing.
    startup_skip: usize,
    /// Total real input frames consumed, against which the flush length is
    /// trimmed.
    in_frames: usize,
    /// Total output frames produced, across `convert` and `flush`, after the
    /// startup ramp was dropped.
    out_frames: usize,
}

impl RateConverter {
    /// Creates a converter from a track's spec to the output spec.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::SpecMismatch`] when the specs disagree on channel
    /// count: only the sample rate may differ. Returns
    /// [`AudioError::Unsupported`] when the resampler cannot be built.
    pub fn new(from: TrackSpec, to: TrackSpec) -> Result<Self, AudioError> {
        if from.channels != to.channels {
            return Err(AudioError::SpecMismatch {
                expected: to,
                got: from,
            });
        }

        if from.sample_rate == to.sample_rate {
            return Ok(Self {
                from,
                to,
                resampler: None,
                pending: Vec::new(),
                silence: Vec::new(),
                startup_skip: 0,
                in_frames: 0,
                out_frames: 0,
            });
        }

        let ratio = f64::from(to.sample_rate) / f64::from(from.sample_rate);
        let parameters =
            SincInterpolationParameters::new(SINC_LEN, WindowFunction::BlackmanHarris2);
        let resampler = Async::new_sinc(
            ratio,
            1.0,
            &parameters,
            CHUNK_FRAMES,
            usize::from(from.channels),
            FixedAsync::Input,
        )
        .map_err(|error| {
            AudioError::Unsupported(format!(
                "cannot resample {} Hz to {} Hz: {error}",
                from.sample_rate, to.sample_rate
            ))
        })?;

        let silence = vec![0.0; CHUNK_FRAMES * usize::from(from.channels)];
        let startup_skip = resampler.output_delay();
        Ok(Self {
            from,
            to,
            resampler: Some(resampler),
            pending: Vec::new(),
            silence,
            startup_skip,
            in_frames: 0,
            out_frames: 0,
        })
    }

    /// The spec this converter's input is in.
    #[must_use]
    pub const fn from(&self) -> TrackSpec {
        self.from
    }

    /// The spec this converter's output is in.
    #[must_use]
    pub const fn to(&self) -> TrackSpec {
        self.to
    }

    /// Converts one interleaved block. Blocks may be any length; the converter
    /// holds back what does not fill a whole resampler chunk, and
    /// [`RateConverter::flush`] releases it.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::Decode`] when the block is not whole frames, or
    /// when the resampler itself fails.
    pub fn convert(&mut self, block: &[f32]) -> Result<Vec<f32>, AudioError> {
        let Some(resampler) = self.resampler.as_mut() else {
            // Same spec: forward every sample untouched, even the padding a
            // resampler would smear.
            return Ok(block.to_vec());
        };

        let channels = usize::from(self.from.channels);
        if !block.len().is_multiple_of(channels) {
            return Err(AudioError::Decode(
                "a decoded block must hold whole frames".to_owned(),
            ));
        }

        self.pending.extend_from_slice(block);
        let mut out = Vec::new();
        let mut startup_skip = self.startup_skip;
        let mut out_frames = self.out_frames;
        while self.pending.len() / channels >= CHUNK_FRAMES {
            self.in_frames += CHUNK_FRAMES;
            let converted = run_chunk(
                resampler,
                &self.pending[..CHUNK_FRAMES * channels],
                channels,
                None,
            )?;
            let converted = drop_startup(converted, &mut startup_skip, channels);
            out_frames += converted.len() / channels;
            out.extend(converted);
            self.pending.drain(..CHUNK_FRAMES * channels);
        }
        self.startup_skip = startup_skip;
        self.out_frames = out_frames;
        Ok(out)
    }

    /// Returns the tail the resampler still holds, ending the stream.
    ///
    /// The remaining input is padded with silence and the delay is pumped out,
    /// but the result is trimmed to what the real input can account for, so
    /// flushing never adds a silence tail beyond the resampler's delay.
    ///
    /// # Errors
    ///
    /// Propagates [`AudioError::Decode`] from the resampler.
    pub fn flush(&mut self) -> Result<Vec<f32>, AudioError> {
        let Some(resampler) = self.resampler.as_mut() else {
            return Ok(Vec::new());
        };

        let channels = usize::from(self.from.channels);
        let mut out = Vec::new();
        let mut startup_skip = self.startup_skip;
        let mut out_frames = self.out_frames;

        if !self.pending.is_empty() {
            let frames = self.pending.len() / channels;
            self.in_frames += frames;
            // A partial chunk pads itself with silence internally, which is
            // what carries the final samples through the filter.
            let indexing = Indexing::new().partial_len(frames);
            let converted = run_chunk(resampler, &self.pending, channels, Some(&indexing))?;
            let converted = drop_startup(converted, &mut startup_skip, channels);
            out_frames += converted.len() / channels;
            out.extend(converted);
            self.pending.clear();
        }

        // The flush may only add frames the real input justifies: the exact
        // scaled length. The startup delay was dropped, so anything beyond the
        // scaled length is the resampler's trailing silence, trimmed below.
        #[expect(
            clippy::cast_precision_loss,
            reason = "a track holds fewer frames than f64 represents exactly"
        )]
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "ceil of a positive ratio is a whole frame count"
        )]
        let target = (self.in_frames as f64 * resampler.resample_ratio()).ceil() as usize;

        while out_frames < target {
            let converted = run_chunk(resampler, &self.silence, channels, None)?;
            let frames = converted.len() / channels;
            if frames == 0 {
                // The resampler stopped producing: no amount of silence will
                // move it, so flushing further would only spin.
                break;
            }
            let converted = drop_startup(converted, &mut startup_skip, channels);
            out_frames += converted.len() / channels;
            out.extend(converted);
        }
        self.startup_skip = startup_skip;
        self.out_frames = out_frames;

        // Trim from the end of this flush only; earlier output was all real.
        // Saturating: a stalled resampler can leave `out_frames` short of the
        // target, and subtracting that would wrap and discard the whole tail.
        let excess = out_frames.saturating_sub(target).min(out.len() / channels);
        self.out_frames -= excess;
        out.truncate((out.len() / channels - excess) * channels);

        Ok(out)
    }
}

/// Splits off the resampler's startup ramp from converted output, returning
/// what remains.
fn drop_startup(mut data: Vec<f32>, remaining: &mut usize, channels: usize) -> Vec<f32> {
    if *remaining > 0 {
        let skipped = (*remaining).min(data.len() / channels);
        data.drain(..skipped * channels);
        *remaining -= skipped;
    }
    data
}

/// Feeds one interleaved chunk to the resampler and returns its output.
fn run_chunk(
    resampler: &mut Async<f32>,
    chunk: &[f32],
    channels: usize,
    indexing: Option<&Indexing>,
) -> Result<Vec<f32>, AudioError> {
    let frames = chunk.len() / channels;
    let input = InterleavedSlice::new(chunk, channels, frames)
        .map_err(|error| AudioError::Decode(error.to_string()))?;
    let converted = resampler
        .process(&input, indexing)
        .map_err(|error| AudioError::Decode(format!("resampling failed: {error}")))?;
    Ok(converted.take_data())
}

/// How to fit a track's channel layout into the device's.
///
/// A layout difference is not a remix when one side is mono: duplicating one
/// channel or averaging two is the only faithful thing to do, and refusing it
/// made every mono file unplayable. A layout with real downmix coefficients
/// (5.1 into stereo) is still refused — inventing those coefficients is a
/// product decision, not a default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelMap {
    from: u16,
    to: u16,
}

impl ChannelMap {
    /// # Errors
    ///
    /// Returns [`AudioError::Unsupported`] when neither layout is mono and they
    /// differ, because that would need a surround downmix.
    pub fn new(from: u16, to: u16) -> Result<Self, AudioError> {
        if from == to || from == 1 || to == 1 {
            return Ok(Self { from, to });
        }
        Err(AudioError::Unsupported(format!(
            "cannot play a {from}-channel track on a {to}-channel device: \
             a surround downmix is not supported yet"
        )))
    }

    /// Applies the mapping to interleaved samples. Every path is exact for mono.
    #[must_use]
    pub fn apply(self, samples: &[f32]) -> Vec<f32> {
        if self.from == self.to {
            return samples.to_vec();
        }
        if self.from == 1 {
            let mut mapped = Vec::with_capacity(samples.len() * usize::from(self.to));
            for sample in samples {
                for _ in 0..self.to {
                    mapped.push(*sample);
                }
            }
            return mapped;
        }
        let mut mapped = Vec::with_capacity(samples.len() / usize::from(self.from));
        for frame in samples.chunks_exact(usize::from(self.from)) {
            let sum: f32 = frame.iter().sum();
            mapped.push(sum / f32::from(self.from));
        }
        mapped
    }
}

/// Maps interleaved samples from one channel layout to another.
///
/// # Errors
///
/// Returns [`AudioError::Unsupported`] when the layouts cannot be reconciled
/// without a surround downmix.
pub fn remap_channels(samples: &[f32], from: u16, to: u16) -> Result<Vec<f32>, AudioError> {
    Ok(ChannelMap::new(from, to)?.apply(samples))
}
