//! The audio path: decode, sequence and stream.
//!
//! Design rule inherited from `gentle-stream` (`player.py`): the audio callback
//! only copies memory. Decoding, resampling, `ReplayGain` ([`replay_gain`], T11)
//! and sequencing happen on a producer thread one buffer ahead, never inside
//! the callback.

use std::fmt;
use std::fs::File;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub mod output;
pub mod replay_gain;
pub mod resample;
pub mod spectrum;

pub use output::{GainRamp, Player, seek_target};

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CodecParameters, Decoder, DecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::TimeBase;

/// Frames produced per block by [`TrackSource::read_block`].
pub const BLOCK_FRAMES: usize = 1024;

/// The sample format a decoded track is played at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackSpec {
    pub sample_rate: u32,
    pub channels: u16,
}

impl TrackSpec {
    /// Samples per second across every channel, interleaved.
    pub(crate) const fn interleaved_rate(self) -> usize {
        self.sample_rate as usize * self.channels as usize
    }
}

/// Everything that can go wrong before a sample reaches the device.
#[derive(Debug)]
pub enum AudioError {
    /// The file could not be read.
    Io(std::io::Error),
    /// The container or codec was recognised but decoding failed.
    Decode(String),
    /// The file should have been decodable but its format is not supported.
    Unsupported(String),
    /// No usable audio device, or the device rejected the stream.
    Device(String),
    /// Two tracks in one gapless sequence disagree on their sample format.
    SpecMismatch { expected: TrackSpec, got: TrackSpec },
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "cannot read the file: {error}"),
            Self::Decode(message) => write!(f, "cannot decode the file: {message}"),
            Self::Unsupported(message) => write!(f, "unsupported format: {message}"),
            Self::Device(message) => write!(f, "audio device: {message}"),
            Self::SpecMismatch { expected, got } => write!(
                f,
                "sample format mismatch: sequence is {} Hz/{} ch, track is {} Hz/{} ch",
                expected.sample_rate, expected.channels, got.sample_rate, got.channels
            ),
        }
    }
}

impl std::error::Error for AudioError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

/// One decoded track, read block by block.
///
/// The decoder is created eagerly so a broken file fails at open time instead of
/// in the middle of a sequence, where it would be heard as a gap.
pub struct TrackSource {
    reader: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    spec: TrackSpec,
    finished: bool,
    /// A block decoded by [`TrackSource::open`] to learn the sample format of a
    /// container that does not declare it; it must be played first.
    pending: Option<Vec<f32>>,
    /// The track's time base, which converts between frames and the timestamps
    /// a seek is expressed in.
    time_base: Option<TimeBase>,
    /// The track's total frame count, when the container declares it.
    n_frames: Option<u64>,
    /// After a seek: the timestamp the decoded output must not start before.
    /// Packets read before it contribute only their frames from it onwards.
    skip_ts: Option<u64>,
}

/// The sample format a container declares, when it declares one at all.
///
/// An M4A/AAC track does not: the channel layout only exists inside the audio
/// specific config, which the decoder parses from the first packet.
fn declared_spec(params: &CodecParameters) -> Option<TrackSpec> {
    let sample_rate = params.sample_rate?;
    let channels = u16::try_from(params.channels?.count()).ok()?;
    Some(TrackSpec {
        sample_rate,
        channels,
    })
}

impl TrackSource {
    /// Opens a file and reads its stream format without decoding audio yet.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::Io`] when the file cannot be read,
    /// [`AudioError::Unsupported`] when the container or codec is not supported,
    /// and [`AudioError::Decode`] when the first packet fails to decode.
    pub fn open(path: &Path) -> Result<Self, AudioError> {
        let file = File::open(path).map_err(AudioError::Io)?;
        let stream = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());

        let mut hint = Hint::new();
        if let Some(extension) = path.extension().and_then(std::ffi::OsStr::to_str) {
            hint.with_extension(extension);
        }

        let format_options = FormatOptions {
            // Trims encoder delay and padding where the container declares them,
            // which is what makes an MP3 or AAC join silent in the first place.
            enable_gapless: true,
            ..FormatOptions::default()
        };

        let probed = symphonia::default::get_probe()
            .format(&hint, stream, &format_options, &MetadataOptions::default())
            .map_err(|error| AudioError::Unsupported(error.to_string()))?;

        let reader = probed.format;
        let (track_id, codec_params) = {
            let track = reader.default_track().ok_or_else(|| {
                AudioError::Unsupported(format!("{} has no audio track", path.display()))
            })?;
            (track.id, track.codec_params.clone())
        };

        let decoder = symphonia::default::get_codecs()
            .make(&codec_params, &DecoderOptions::default())
            .map_err(|error| AudioError::Unsupported(error.to_string()))?;

        let declared = declared_spec(&codec_params);
        let mut source = Self {
            reader,
            decoder,
            track_id,
            // A placeholder until the decoder reports the real format below.
            spec: declared.unwrap_or(TrackSpec {
                sample_rate: 0,
                channels: 0,
            }),
            finished: false,
            pending: None,
            time_base: codec_params.time_base,
            n_frames: codec_params.n_frames,
            skip_ts: None,
        };

        if declared.is_none() {
            // The device must be configured before the first sample plays, so
            // the format has to be resolved now rather than at playback time.
            let first = source.decode_next()?.ok_or_else(|| {
                AudioError::Unsupported(format!("{} has no playable audio", path.display()))
            })?;
            source.pending = Some(first);
        }

        Ok(source)
    }

    /// The sample format this track will produce.
    #[must_use]
    pub const fn spec(&self) -> TrackSpec {
        self.spec
    }

    /// Opens a file and positions it `offset_frames` from its first frame, as
    /// a seek does: the player is rebuilt from this offset, it never injects
    /// anything into a running engine (T6).
    ///
    /// An offset past the end of the track clamps to the end: the stream then
    /// simply has nothing left, it does not fail.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`TrackSource::open`]. A seek itself never
    /// fails here: an offset the reader refuses is treated as the end.
    pub fn open_at(path: &Path, offset_frames: u64) -> Result<Self, AudioError> {
        let mut source = Self::open(path)?;
        source.seek_to_frames(offset_frames);
        Ok(source)
    }

    /// Positions the stream at `offset_frames` from the first frame.
    fn seek_to_frames(&mut self, offset_frames: u64) {
        if offset_frames == 0 {
            return;
        }

        let offset = self
            .n_frames
            .map_or(offset_frames, |total| offset_frames.min(total));

        // Clamped to (or past) the end: the stream finishes before it starts.
        if self.n_frames.is_some_and(|total| offset >= total) {
            self.finished = true;
            self.pending = None;
            return;
        }

        let Some(time_base) = self.time_base else {
            // Without a time base the offset cannot be expressed; without a
            // declared frame count the safest clamp is the end of the track.
            self.finished = true;
            self.pending = None;
            return;
        };
        let ts = frames_to_ts(offset, time_base, self.spec.sample_rate);

        // Audio decoded before the seek point must not reach the output.
        self.pending = None;
        match self.reader.seek(
            SeekMode::Accurate,
            SeekTo::TimeStamp {
                ts,
                track_id: self.track_id,
            },
        ) {
            Ok(seeked) => {
                // Accurate seeks land at or before the requested position, so
                // the packets that follow still hold frames to drop.
                self.skip_ts = Some(seeked.required_ts);
                // The next packet is discontinuous with the last decoded one:
                // the decoder keeps cross-packet state (an MP3 bit reservoir,
                // say) that a seek invalidates.
                self.decoder.reset();
            }
            Err(_) => {
                // The reader refused the seek — an offset past the end of a
                // track with no declared length, most likely. Clamp to the
                // end instead of failing a transport action.
                self.finished = true;
            }
        }
    }

    /// How long this track is, when the container states it.
    ///
    /// The interface needs it to clamp a seek and to show a total. A queue that
    /// came from the library has lengths from the index, but a queue that came
    /// from the command line has nothing but what the decoder knows (T21).
    #[must_use]
    pub fn duration(&self) -> Option<Duration> {
        let frames = self.n_frames?;
        let rate = u64::from(self.spec.sample_rate.max(1));
        Some(Duration::from_millis(frames.saturating_mul(1000) / rate))
    }

    /// Returns the next block of interleaved `f32` samples, or `None` at the end.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::Decode`] when a packet cannot be decoded. The track
    /// cannot be resumed after that: the caller decides whether to skip it.
    pub fn read_block(&mut self) -> Result<Option<Vec<f32>>, AudioError> {
        if let Some(block) = self.pending.take() {
            return Ok(Some(block));
        }
        self.decode_next()
    }

    /// Pulls packets until one decodes into samples.
    ///
    /// The decoder, not the container, is the authority on the sample format: a
    /// container that declares nothing gets its spec filled in here.
    fn decode_next(&mut self) -> Result<Option<Vec<f32>>, AudioError> {
        if self.finished {
            return Ok(None);
        }

        loop {
            let packet = match self.reader.next_packet() {
                Ok(packet) => packet,
                Err(SymphoniaError::IoError(error)) if error.kind() == ErrorKind::UnexpectedEof => {
                    self.finished = true;
                    return Ok(None);
                }
                Err(SymphoniaError::ResetRequired) => {
                    self.finished = true;
                    return Ok(None);
                }
                Err(error) => return Err(AudioError::Decode(error.to_string())),
            };

            if packet.track_id() != self.track_id {
                continue;
            }

            let decoded = self
                .decoder
                .decode(&packet)
                .map_err(|error| AudioError::Decode(error.to_string()))?;

            let signal = *decoded.spec();
            let channels = u16::try_from(signal.channels.count())
                .map_err(|_| AudioError::Unsupported("too many channels".to_owned()))?;
            self.spec = TrackSpec {
                sample_rate: signal.rate,
                channels,
            };

            let mut buffer = SampleBuffer::<f32>::new(decoded.capacity() as u64, signal);
            buffer.copy_interleaved_ref(decoded);

            let mut samples = buffer.samples().to_vec();

            // A seek: hold back everything before the timestamp it asked for.
            if let Some(skip_ts) = self.skip_ts {
                let channels = usize::from(self.spec.channels);
                let drop_ts = skip_ts.saturating_sub(packet.ts());
                let drop_frames = self.time_base.map_or(0, |time_base| {
                    ts_to_frames(drop_ts, time_base, self.spec.sample_rate)
                });
                let packet_frames = samples.len() / channels;
                let drop = usize::try_from(drop_frames).unwrap_or(usize::MAX);
                if drop == 0 {
                    self.skip_ts = None;
                } else if drop < packet_frames {
                    samples.drain(..drop * channels);
                    self.skip_ts = None;
                } else {
                    // The whole packet lies before the seek target; the next
                    // one starts closer to it.
                    continue;
                }
            }

            // A packet that decodes to nothing must not surface as an empty
            // block: to the caller that is indistinguishable from the end.
            if !samples.is_empty() {
                return Ok(Some(samples));
            }
        }
    }
}

/// Converts a span of track timestamps into decoded frames.
fn ts_to_frames(ts: u64, time_base: TimeBase, sample_rate: u32) -> u64 {
    let frames = u128::from(ts) * u128::from(time_base.numer) * u128::from(sample_rate.max(1));
    u64::try_from(frames / u128::from(time_base.denom)).unwrap_or(u64::MAX)
}

/// Converts decoded frames into a track timestamp for [`TrackSource::seek_to_frames`].
fn frames_to_ts(frames: u64, time_base: TimeBase, sample_rate: u32) -> u64 {
    let ts = u128::from(frames) * u128::from(time_base.denom);
    let rate = u128::from(sample_rate.max(1)) * u128::from(time_base.numer);
    u64::try_from(ts / rate).unwrap_or(u64::MAX)
}

/// Drains several tracks as one continuous stream, as the gapless path must.
///
/// Returns the shared sample format and the concatenated interleaved samples.
/// A track whose format differs from the first one is rejected, because joining
/// them without resampling would shift pitch and insert a gap.
///
/// # Errors
///
/// Propagates every error of [`TrackSource::open`] and
/// [`TrackSource::read_block`], returns [`AudioError::SpecMismatch`] for a track
/// that cannot be joined, and [`AudioError::Unsupported`] for an empty list.
pub fn drain_gapless(paths: &[PathBuf]) -> Result<(TrackSpec, Vec<f32>), AudioError> {
    let mut expected: Option<TrackSpec> = None;
    let mut samples = Vec::new();

    for path in paths {
        let mut source = TrackSource::open(path)?;
        let spec = source.spec();

        match expected {
            None => expected = Some(spec),
            Some(sequence) if sequence != spec => {
                return Err(AudioError::SpecMismatch {
                    expected: sequence,
                    got: spec,
                });
            }
            Some(_) => {}
        }

        while let Some(block) = source.read_block()? {
            samples.extend_from_slice(&block);
        }
    }

    let spec = expected.ok_or_else(|| AudioError::Unsupported("no tracks to play".to_owned()))?;
    debug_assert_eq!(
        samples.len() % usize::from(spec.channels),
        0,
        "an interleaved stream must hold whole frames"
    );

    Ok((spec, samples))
}
