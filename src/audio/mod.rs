//! The audio path: decode, sequence and stream.
//!
//! Design rule inherited from `gentle-stream` (`player.py`): the audio callback
//! only copies memory. Decoding, resampling, `ReplayGain` ([`replay_gain`], T11),
//! loudness measurement ([`loudness`], T13) and sequencing happen on a producer
//! thread one buffer ahead, never inside the callback.

use std::fmt;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub mod loudness;
pub mod mp4;
pub mod opus;
pub mod output;
pub mod replay_gain;
pub mod resample;
pub mod spectrum;

pub use output::{GainRamp, Player, seek_target, skip_target};

use symphonia::core::codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, Track, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{TimeBase, Timestamp};

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
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    spec: TrackSpec,
    finished: bool,
    /// A block decoded by [`TrackSource::open`] to learn the sample format of a
    /// container that does not declare it; it must be played first.
    pending: Option<Vec<f32>>,
    /// The track's time base, which converts between frames and the timestamps
    /// a seek is expressed in.
    time_base: Option<TimeBase>,
    /// The track's total frame count, when the container declares it. For a
    /// container whose priming [`TrackSource`] trims itself (isomp4) this is
    /// the raw count, priming included; content frames live inside it.
    n_frames: Option<u64>,
    /// The priming this track trims itself, when the container declares it and
    /// symphonia does not already trim it (T16).
    priming: Option<Priming>,
    /// How much of the priming delay the reader pre-spent on its presentation
    /// timeline, in frames. symphonia's Ogg reader presents the first Opus
    /// packet at `pts = delay` — the pre-skip is spent before the first packet
    /// begins (measured) — while isomp4 presents from 0. The content window
    /// addresses raw decoded frames, so it subtracts what the reader spent.
    pts_shift: u64,
    /// After a seek: the timestamp the decoded output must not start before.
    /// Packets read before it contribute only their frames from it onwards.
    skip_ts: Option<Timestamp>,
}

/// The sample format a container declares, when it declares one at all.
///
/// An M4A/AAC track does not: the channel layout only exists inside the audio
/// specific config, which the decoder parses from the first packet.
fn declared_spec(params: &AudioCodecParameters) -> Option<TrackSpec> {
    let sample_rate = params.sample_rate?;
    let channels = u16::try_from(params.channels.as_ref()?.count()).ok()?;
    Some(TrackSpec {
        sample_rate,
        channels,
    })
}

/// Encoder priming the container declares, in frames: warm-up before the first
/// content frame, flush padding after the last one.
#[derive(Clone, Copy, Default)]
struct Priming {
    delay: u64,
    padding: u64,
}

impl Priming {
    const fn frames(self) -> u64 {
        self.delay + self.padding
    }
}

/// The priming a track must have trimmed, and by whom.
///
/// When symphonia's own decoder was built, it trims the packet trims its
/// readers declare (the MP3/LAME path), so a priming symphonia populated on
/// the track stays symphonia's — hands off. When the decoder is velo's own
/// (Opus), symphonia's Ogg reader puts no trim on the packets (measured: the
/// Opus packet parser reports zero discard), so the container-declared priming
/// is trimmed here. When symphonia populates neither field (isomp4, measured
/// on 0.5.5 and 0.6.1), the priming is read from the container. When symphonia
/// already owns the trim, [`TrackSource`] adds nothing: double-trimming is
/// worse than not trimming.
fn declared_priming(track: &Track, path: Option<&Path>, symphonia_built: bool) -> Option<Priming> {
    if track.delay.is_none() && track.padding.is_none() {
        // symphonia populates neither (isomp4): the priming lives in the
        // container, gated on `ftyp` so no other format's behaviour changes.
        // Only a file has a container to read — a stream has no bytes to seek
        // back to, and its codec either declares the priming on the track or
        // has none.
        return path.and_then(mp4::read_priming).map(|priming| Priming {
            delay: priming.delay,
            padding: priming.padding,
        });
    }
    if symphonia_built {
        // symphonia populated and trims it (MP3's LAME tag) — hands off.
        return None;
    }
    Some(Priming {
        delay: u64::from(track.delay.unwrap_or(0)),
        padding: u64::from(track.padding.unwrap_or(0)),
    })
}

/// Builds the one decoder for a track's codec parameters.
///
/// The only place where `AudioDecoderOptions` is decided. Files decode with
/// the default `gapless: true` — the decoder trims the delay and padding the
/// reader declares per packet, which is what makes an MP3 or AAC join silent.
/// A live stream decodes with `gapless: false`, which drops the trim of
/// **encoder padding** — a station owes nobody a gapless join — but not the
/// trim of a **normative** pre-skip. An Opus stream carries its `OpusHead`
/// inside the body, so the delay that declares is content-neutralising rather
/// than padding, and [`TrackSource`] still removes it: keeping those two apart
/// is why a live stream and the same bytes as a file decode to the same
/// samples.
///
/// Opus is built by [`opus::SymphoniaDecoder`] (velo's own decoder over a
/// statically linked libopus) whenever [`opus::decodable`] answers true — the
/// one function that decides playability; everything else falls through to
/// symphonia's registry exactly as before. The boolean half of the result
/// records who built the decoder, because symphonia's own decoders trim the
/// packet trims its readers declare while velo's own do not — [`TrackSource`]
/// needs to know whether a container-declared priming is already handled.
fn make_decoder(
    params: &AudioCodecParameters,
    gapless: bool,
) -> Result<(Box<dyn AudioDecoder>, bool), AudioError> {
    // `AudioDecoderOptions` is #[non_exhaustive] in symphonia 0.6, so the
    // struct-update form `..Default::default()` is refused across the crate
    // boundary; the builder is the same one decision.
    let options = AudioDecoderOptions::default().gapless(gapless);
    if opus::decodable(params.codec) {
        let decoder = opus::SymphoniaDecoder::try_new(params, options)?;
        return Ok((Box::new(decoder), false));
    }
    let decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &options)
        .map_err(|error| AudioError::Unsupported(error.to_string()))?;
    Ok((decoder, true))
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
        let mut hint = Hint::new();
        if let Some(extension) = path.extension().and_then(std::ffi::OsStr::to_str) {
            hint.with_extension(extension);
        }
        let stream = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());
        Self::build(stream, &hint, true, Some(path), false)
    }

    /// Opens a live stream for decoding as its bytes arrive.
    ///
    /// The source is probed with an empty hint — a stream has no extension —
    /// so recognition falls back to content sniffing alone. It is never
    /// seekable and never reports a duration, even when a tag inside the body
    /// declares a frame count; the sample-format mismatch rule of a gapless
    /// sequence does not apply, because a station can only enter the queue on
    /// its own, never as a sequence member. Encoder padding is not trimmed
    /// here, but a codec's normative pre-skip still is — see
    /// [`make_decoder`]'s note on why those are different things.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::Unsupported`] when the container or codec is not
    /// recognised from the bytes read so far, and [`AudioError::Decode`] when
    /// the first packet fails to decode.
    pub fn open_stream(source: Box<dyn MediaSource>) -> Result<Self, AudioError> {
        let stream = MediaSourceStream::new(source, MediaSourceStreamOptions::default());
        Self::build(stream, &Hint::new(), false, None, true)
    }

    /// Probes, builds the decoder and resolves the sample format, shared by
    /// the file and stream entries. `gapless` is decided by the caller; a file
    /// path additionally enables reading the container's own priming.
    fn build(
        // Both entries hand over a `'static` source — an owned `File` or an
        // owned `Box<dyn MediaSource>` — and the probed reader borrows the
        // stream for its whole life, so the lifetime is pinned here.
        stream: MediaSourceStream<'static>,
        hint: &Hint,
        gapless: bool,
        path: Option<&Path>,
        live: bool,
    ) -> Result<Self, AudioError> {
        let name = path.map_or_else(|| "stream".to_owned(), |path| path.display().to_string());

        let probed = symphonia::default::get_probe()
            .probe(
                hint,
                stream,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .map_err(|error| AudioError::Unsupported(error.to_string()))?;

        let reader = probed;
        let track = reader
            .default_track(TrackType::Audio)
            .ok_or_else(|| AudioError::Unsupported(format!("{name} has no audio track")))?;

        let audio_params = track
            .codec_params
            .as_ref()
            .and_then(symphonia::core::codecs::CodecParameters::audio)
            .ok_or_else(|| AudioError::Unsupported(format!("{name} has no audio track")))?
            .clone();

        let (decoder, symphonia_built) = make_decoder(&audio_params, gapless)?;

        let declared = declared_spec(&audio_params);
        // The pre-skip is a property of the codec, not of a file: Opus's
        // OpusHead travels inside a live stream too, so the reader populates
        // `track.delay` whether or not there is a path. It is normative
        // (RFC 6716) and is trimmed in both cases. The encoder *padding* is a
        // sequence concern — it only matters when something follows — so a
        // station, which has no end to respect, does not carry it.
        let priming = declared_priming(track, path, symphonia_built).map(|priming| {
            if live {
                Priming {
                    delay: priming.delay,
                    padding: 0,
                }
            } else {
                priming
            }
        });
        // Only a delay the reader itself declared on the track is pre-spent on
        // its timeline (Opus, above); isomp4 declares none (T16 reads it from
        // the container) and its packets present from 0.
        let pts_shift = if priming.is_some() {
            track.delay.map_or(0, |delay| {
                declared.map_or(0, |spec| {
                    track.time_base.map_or(0, |time_base| {
                        ts_to_frames(u64::from(delay), time_base, spec.sample_rate)
                    })
                })
            })
        } else {
            0
        };
        // Copy what the track declares before the reader moves into the
        // source: in 0.6 the track's timing lives on `Track`, not in the codec
        // parameters. A live stream never reports a frame count even when a
        // tag declares one — a station has no duration and never seeks (S2).
        let (track_id, time_base, n_frames) = (
            track.id,
            track.time_base,
            if live { None } else { track.num_frames },
        );
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
            time_base,
            n_frames,
            priming,
            pts_shift,
            skip_ts: None,
        };

        if declared.is_none() {
            // The device must be configured before the first sample plays, so
            // the format has to be resolved now rather than at playback time.
            let first = source
                .decode_next()?
                .ok_or_else(|| AudioError::Unsupported(format!("{name} has no playable audio")))?;
            source.pending = Some(first);
        }

        Ok(source)
    }

    /// The sample format this track will produce.
    #[must_use]
    pub const fn spec(&self) -> TrackSpec {
        self.spec
    }

    /// How many content frames this track holds: the raw count minus the
    /// priming this source trims itself. For a track symphonia trims (MP3) the
    /// raw count is already the content count and this is just `n_frames`.
    fn content_frames(&self) -> Option<u64> {
        self.n_frames
            .map(|total| total.saturating_sub(self.priming.map_or(0, Priming::frames)))
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

    /// Positions the stream at `offset_frames` from the first content frame.
    ///
    /// The offset is in content frames: what a listener counts. For a track
    /// whose priming this source trims itself (isomp4, T16), content frame 0 is
    /// raw frame `delay`, so the reader is aimed one priming deeper and the
    /// head trim still applies to whatever precedes the target.
    fn seek_to_frames(&mut self, offset_frames: u64) {
        if offset_frames == 0 {
            return;
        }

        let content = self.content_frames();
        let offset = content.map_or(offset_frames, |total| offset_frames.min(total));

        // Clamped to (or past) the end: the stream finishes before it starts.
        if content.is_some_and(|total| offset_frames >= total) {
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
        let ts = frames_to_ts(
            offset + self.priming.map_or(0, |priming| priming.delay),
            time_base,
            self.spec.sample_rate,
        );

        // Audio decoded before the seek point must not reach the output.
        self.pending = None;
        match self.reader.seek(
            SeekMode::Accurate,
            SeekTo::Timestamp {
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
        let frames = self.content_frames()?;
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
                Ok(Some(packet)) => packet,
                // End of media, or the track list changed: both are the end
                // of this stream.
                Ok(None) | Err(SymphoniaError::ResetRequired) => {
                    self.finished = true;
                    return Ok(None);
                }
                Err(error) => return Err(AudioError::Decode(error.to_string())),
            };

            if packet.track_id != self.track_id {
                continue;
            }

            let decoded = self
                .decoder
                .decode(&packet)
                .map_err(|error| AudioError::Decode(error.to_string()))?;

            let signal = decoded.spec().clone();
            let channels = u16::try_from(signal.channels().count())
                .map_err(|_| AudioError::Unsupported("too many channels".to_owned()))?;
            self.spec = TrackSpec {
                sample_rate: signal.rate(),
                channels,
            };

            // The generic buffer ref carries whatever integer or float format
            // the codec produced; this converts and interleaves it into f32.
            let mut samples = Vec::with_capacity(decoded.samples_interleaved());
            decoded.copy_to_vec_interleaved::<f32>(&mut samples);
            let packet_frames = samples.len() / usize::from(channels);

            // What must not reach the output, in packet frames from the head:
            // a seek holds back everything before the timestamp it asked for,
            // and a container whose priming this source trims itself (isomp4,
            // T16; Opus, T14b) hides its encoder warm-up and padding behind
            // the window [delay, n_frames - padding) over the raw decoded
            // stream. Both are addressed in raw frames, so the deeper one wins
            // at the head. The raw position of a packet is its timestamp minus
            // whatever presentation offset the reader pre-spent (Opus's
            // pre-skip — see `pts_shift`).
            let mut head = 0usize;
            let mut tail = 0usize;
            let mut clear_skip = false;
            if let Some(skip_ts) = self.skip_ts {
                // How far the packet's start lies before the seek target, in
                // timebase ticks; saturates at or below zero once the packet
                // starts at or after it.
                let drop_ts = skip_ts
                    .saturating_delta(packet.pts)
                    .get()
                    .max(0)
                    .unsigned_abs();
                let drop_frames = self.time_base.map_or(0, |time_base| {
                    ts_to_frames(drop_ts, time_base, self.spec.sample_rate)
                });
                let drop = usize::try_from(drop_frames).unwrap_or(usize::MAX);
                if drop == 0 {
                    clear_skip = true;
                } else if drop < packet_frames {
                    head = drop;
                    clear_skip = true;
                } else {
                    // The whole packet lies before the seek target; the next
                    // one starts closer to it.
                    continue;
                }
            }
            if let Some(priming) = self.priming {
                let raw_start = self
                    .time_base
                    .map_or(0, |time_base| {
                        ts_to_frames(
                            packet.pts.get().max(0).unsigned_abs(),
                            time_base,
                            self.spec.sample_rate,
                        )
                    })
                    .saturating_sub(self.pts_shift);
                let raw_end = raw_start + packet_frames as u64;
                let emit_start = raw_start.max(priming.delay);
                let emit_end = self.n_frames.map_or(raw_end, |total| {
                    raw_end.min(total.saturating_sub(priming.padding))
                });
                let window_head = usize::try_from(emit_start - raw_start)
                    .unwrap_or(packet_frames)
                    .min(packet_frames);
                let window_tail = usize::try_from(raw_end - emit_end)
                    .unwrap_or(packet_frames)
                    .min(packet_frames);
                head = head.max(window_head);
                tail = window_tail;
            }
            if clear_skip {
                self.skip_ts = None;
            }
            if head > 0 {
                samples.drain(..head.min(packet_frames) * usize::from(channels));
            }
            if tail > 0 {
                let keep = packet_frames.saturating_sub(head + tail);
                samples.truncate(keep * usize::from(channels));
            }

            // A packet that decodes to nothing must not surface as an empty
            // block: to the caller that is indistinguishable from the end.
            if !samples.is_empty() {
                return Ok(Some(samples));
            }
        }
    }
}

/// Converts a span of track timestamps (timebase ticks) into decoded frames.
/// A negative timestamp contributes no frames.
fn ts_to_frames(ts: u64, time_base: TimeBase, sample_rate: u32) -> u64 {
    let frames = u128::from(ts)
        * u128::from(time_base.numer.get())
        * u128::from(u64::from(sample_rate.max(1)));
    u64::try_from(frames / u128::from(time_base.denom.get())).unwrap_or(u64::MAX)
}

/// Converts decoded frames into a track timestamp for [`TrackSource::seek_to_frames`].
fn frames_to_ts(frames: u64, time_base: TimeBase, sample_rate: u32) -> Timestamp {
    let ts = u128::from(frames) * u128::from(time_base.denom.get());
    let rate = u128::from(u64::from(sample_rate.max(1))) * u128::from(time_base.numer.get());
    u64::try_from(ts / rate)
        .ok()
        .and_then(|ts| i64::try_from(ts).ok())
        .map_or(Timestamp::MAX, Timestamp::new)
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
