//! The realtime half of the audio path: a lock-free ring buffer fed by a
//! producer thread and drained by the device callback.
//!
//! The callback does exactly three things: measure how many samples are ready,
//! copy them, and fill the rest with silence. No allocation, no locking, no
//! logging, no decoding. Everything expensive happens on the producer thread.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rtrb::{Producer, RingBuffer};

use super::resample::RateConverter;
use super::{AudioError, TrackSource, TrackSpec};

/// Two seconds of audio in flight: long enough to survive a slow decode, short
/// enough to keep pause and stop responsive.
const RING_SECONDS: usize = 2;

/// How long the producer waits when the ring is full, i.e. how far ahead the
/// device is allowed to lag before we poll again.
const POLL_INTERVAL: Duration = Duration::from_millis(2);

/// How long the main thread waits before re-checking whether playback drained.
const DRAIN_POLL: Duration = Duration::from_millis(20);

/// A linear gain ramp applied where clicks are born: in the callback, between
/// one written sample and the next (S8).
///
/// Volume changes and pauses never step the gain; they fade across the buffer
/// being written. The ramp owns the current gain, so once it has arrived at a
/// target, writing more samples at that target changes nothing — there is no
/// drift and no second fade.
#[derive(Clone, Copy, Debug)]
pub struct GainRamp {
    current: f32,
}

impl GainRamp {
    /// Starts the ramp at `current` gain.
    #[must_use]
    pub const fn new(current: f32) -> Self {
        Self { current }
    }

    /// The gain the last ramp ended at.
    #[must_use]
    pub const fn current(&self) -> f32 {
        self.current
    }

    /// Whether the gain is zero, i.e. the output is silent.
    #[must_use]
    pub const fn is_silent(&self) -> bool {
        self.current == 0.0
    }

    /// Multiplies `samples` by a gain that moves linearly, sample by sample,
    /// from the current gain to `target`, which it reaches at the last sample.
    /// A callback calls this once per buffer, so the fade always completes
    /// within one buffer and never moves more than `delta / buffer` per
    /// sample — the anti-click rule.
    pub fn ramp_to(&mut self, samples: &mut [f32], target: f32) {
        #[expect(
            clippy::cast_precision_loss,
            reason = "an audio buffer holds far fewer samples than f32 counts exactly"
        )]
        let count = samples.len() as f32;
        let step = (target - self.current) / count;
        let mut gain = self.current;
        for sample in samples.iter_mut() {
            gain += step;
            *sample *= gain;
        }
        // The arrived state is the target itself, not an accumulation of
        // rounded steps, so the next buffer at the same target is a no-op.
        self.current = target;
    }
}

/// The offset a seek lands on: `position` moved by `delta_secs`, clamped into
/// `0..=duration`. A `None` duration only clamps the lower end; the player
/// clamps an offset past the end of the track when it opens it.
#[must_use]
pub fn seek_target(position: Duration, delta_secs: i64, duration: Option<Duration>) -> Duration {
    let moved = if delta_secs >= 0 {
        position.saturating_add(Duration::from_secs(delta_secs.unsigned_abs()))
    } else {
        position.saturating_sub(Duration::from_secs(delta_secs.unsigned_abs()))
    };
    duration.map_or(moved, |duration| moved.min(duration))
}

/// A running output stream. Dropping it stops the device.
pub struct Player {
    stream: cpal::Stream,
    spec: TrackSpec,
    /// The counters the callback reads and writes; the same handles its
    /// closure holds.
    wires: CallbackWires,
    stop: Arc<AtomicBool>,
    delivered: Arc<AtomicUsize>,
    /// The queue index the producer is currently decoding; the UI reads it to
    /// mark the playing row.
    current: Arc<AtomicUsize>,
    /// Which track the producer is on and how many ring samples had been
    /// delivered when it started it, in one record. Publishing these as two
    /// pieces let the UI read an index whose start had not been written yet and
    /// report a position of zero — a seek from there lands at the delta, not at
    /// the delta from where the music actually is.
    progress: Arc<Mutex<Progress>>,
    /// The track a seek landed on and how deep into it, in interleaved samples
    /// — the same unit as the played counter, which is what made a stereo seek
    /// report half its position when this was stored as frames.
    /// Without this the played counter makes the clock restart at zero in the
    /// middle of a song after every seek.
    start_track: usize,
    start_offset_samples: usize,
    producer: Option<JoinHandle<Result<(), AudioError>>>,
}

/// Where the producer is: the queue index and the delivered count at which the
/// current track began. Only the producer writes it, only the UI reads it.
#[derive(Clone, Copy, Debug, Default)]
struct Progress {
    track: usize,
    start: usize,
}

/// The atomics the device callback reads and writes. Created once per player
/// and shared with its closure.
#[derive(Clone)]
struct CallbackWires {
    playing: Arc<AtomicBool>,
    producing: Arc<AtomicBool>,
    played: Arc<AtomicUsize>,
    underruns: Arc<AtomicUsize>,
    volume: Arc<AtomicU8>,
}

impl Player {
    /// Plays every path as one continuous stream, in order, with no gap between
    /// them. The device runs at the first track's own format when it accepts
    /// it, otherwise at the device default, and every track is converted to
    /// that format so the queue never stops (T2).
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::Io`] when a path cannot be read,
    /// [`AudioError::Unsupported`] when its format is not decodable,
    /// [`AudioError::Device`] when no output device accepts the format, and
    /// [`AudioError::SpecMismatch`] when a track's channel layout cannot be
    /// joined to the output.
    pub fn play_gapless(paths: Vec<PathBuf>) -> Result<Self, AudioError> {
        Self::play_gapless_at(paths, 0, Duration::ZERO)
    }

    /// Plays every path from `track_index` onwards as one continuous stream,
    /// starting the first one `offset` into it. A seek is a rebuild from this
    /// offset, never a command injected into a running engine (T6). A few
    /// milliseconds of silence while the stream restarts are expected.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::Unsupported`] when the list is empty or
    /// `track_index` is past it, and otherwise the same errors as
    /// [`Player::play_gapless`].
    pub fn play_gapless_at(
        paths: Vec<PathBuf>,
        track_index: usize,
        offset: Duration,
    ) -> Result<Self, AudioError> {
        if paths.is_empty() {
            return Err(AudioError::Unsupported("no tracks to play".to_owned()));
        }
        if track_index >= paths.len() {
            return Err(AudioError::Unsupported(
                "no such track in the queue".to_owned(),
            ));
        }

        // Opening once up front means a missing or broken first file fails
        // before a device is opened, and gives us the format to configure.
        let spec = TrackSource::open(&paths[track_index])?.spec();
        let offset_frames = duration_to_frames(offset, spec.sample_rate);

        let (device, config, spec) = open_output(spec)?;
        // `offset_frames` is in the track's own frames, which is what the decoder
        // needs. The played counter, however, counts interleaved samples — rate
        // times channels — so the clock's offset has to be scaled the same way
        // or a stereo seek reports half the position it landed on.
        let clock_offset_samples = offset_samples(offset, spec);

        let (ring, consumer) = RingBuffer::<f32>::new(spec.interleaved_rate() * RING_SECONDS);

        let stop = Arc::new(AtomicBool::new(false));
        let delivered = Arc::new(AtomicUsize::new(0));
        // The requested track is the one being produced until the producer says
        // otherwise; it sets the index before it opens each track.
        let current = Arc::new(AtomicUsize::new(track_index));
        let progress = Arc::new(Mutex::new(Progress {
            track: track_index,
            start: 0,
        }));
        let wires = CallbackWires {
            playing: Arc::new(AtomicBool::new(true)),
            producing: Arc::new(AtomicBool::new(true)),
            played: Arc::new(AtomicUsize::new(0)),
            underruns: Arc::new(AtomicUsize::new(0)),
            volume: Arc::new(AtomicU8::new(100)),
        };

        let producer = {
            let delivered = Arc::clone(&delivered);
            let stop = Arc::clone(&stop);
            let current = Arc::clone(&current);
            let progress = Arc::clone(&progress);
            let producing = Arc::clone(&wires.producing);
            thread::Builder::new()
                .name("velo-producer".to_owned())
                .spawn(move || {
                    let job = Sequence {
                        paths,
                        start_track: track_index,
                        offset_frames,
                    };
                    let result = produce(job, spec, ring, &delivered, &stop, &current, &progress);
                    producing.store(false, Ordering::Release);
                    result
                })
                .map_err(|error| AudioError::Device(error.to_string()))?
        };

        // Give the producer a head start before the device starts asking for
        // samples: a callback that finds the ring empty at the start is heard as
        // a click. The wait is bounded, so a broken first file cannot hang here.
        wait_for_preroll(&delivered, spec.interleaved_rate() / 10);

        let stream = build_stream(&device, config, consumer, &wires)?;

        stream
            .play()
            .map_err(|error| AudioError::Device(error.to_string()))?;

        Ok(Self {
            stream,
            spec,
            wires,
            stop,
            delivered,
            current,
            progress,
            start_track: track_index,
            start_offset_samples: usize::try_from(clock_offset_samples).unwrap_or(usize::MAX),
            producer: Some(producer),
        })
    }

    /// The format the device was opened at.
    #[must_use]
    pub const fn spec(&self) -> TrackSpec {
        self.spec
    }

    /// Blocks until every queued track has been produced and played.
    ///
    /// # Errors
    ///
    /// Propagates the first decoding error the producer thread hit, and returns
    /// [`AudioError::Decode`] when that thread stopped unexpectedly.
    pub fn wait_until_finished(&mut self) -> Result<(), AudioError> {
        let outcome = match self.producer.take() {
            Some(handle) => handle.join().map_err(|_| {
                AudioError::Decode("the producer thread stopped unexpectedly".to_owned())
            })?,
            None => Ok(()),
        };
        outcome?;

        // The producer never runs ahead of the ring, so the queue is empty only
        // when everything it delivered has also been consumed by the device.
        while self.delivered.load(Ordering::Acquire) > self.wires.played.load(Ordering::Acquire) {
            thread::sleep(DRAIN_POLL);
        }
        Ok(())
    }

    /// Moments where the device asked for samples the producer had not written
    /// yet. A steady zero here is what "no gap" sounds like.
    #[must_use]
    pub fn underruns(&self) -> usize {
        self.wires.underruns.load(Ordering::Relaxed)
    }

    /// The index, within the list passed to [`Player::play_gapless`], of the
    /// track whose samples are currently being produced. The producer sets it
    /// before it opens each track; readers only load it.
    #[must_use]
    pub fn current_index(&self) -> usize {
        self.current.load(Ordering::Acquire)
    }

    /// Silences the device without stopping the stream.
    pub fn set_playing(&self, playing: bool) {
        self.wires.playing.store(playing, Ordering::Relaxed);
    }

    /// Sets the target volume in percent, 0 to 100. The callback fades its
    /// gain there across one buffer; it never jumps.
    pub fn set_volume(&self, percent: u8) {
        self.wires.volume.store(percent.min(100), Ordering::Relaxed);
    }

    /// The volume in percent.
    #[must_use]
    pub fn volume(&self) -> u8 {
        self.wires.volume.load(Ordering::Relaxed)
    }

    /// Elapsed time within the current track, counted by the callback itself:
    /// the samples it played minus the samples delivered before the track
    /// started. A track still being buffered reports zero; a paused player
    /// does not advance, because it does not play.
    #[must_use]
    pub fn position(&self) -> Duration {
        let progress = match self.progress.lock() {
            Ok(progress) => *progress,
            Err(poisoned) => *poisoned.into_inner(),
        };
        let samples = elapsed_samples(
            self.wires.played.load(Ordering::Acquire),
            progress.start,
            progress.track,
            self.start_track,
            self.start_offset_samples,
        );
        #[expect(
            clippy::cast_precision_loss,
            reason = "a track holds fewer frames than f64 represents exactly"
        )]
        let seconds = samples as f64 / self.spec.interleaved_rate() as f64;
        Duration::from_secs_f64(seconds)
    }

    /// Asks the producer to stop and closes the device.
    pub fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        drop(self.stream);
    }
}

/// Where a sequence starts: which track, and how far into it.
struct Sequence {
    paths: Vec<PathBuf>,
    start_track: usize,
    offset_frames: u64,
}

/// Builds the device stream: the callback that drains the ring. Its whole job
/// is copying samples and applying the gain ramp — no allocation, no locks,
/// no logging, no decoding.
fn build_stream(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut consumer: rtrb::Consumer<f32>,
    wires: &CallbackWires,
) -> Result<cpal::Stream, AudioError> {
    let CallbackWires {
        playing,
        producing,
        played,
        underruns,
        volume,
    } = wires.clone();
    // The gain lives in the callback and is only ever moved by the ramp: no
    // jump at start-up, on pause, on resume or on volume.
    let mut gain = GainRamp::new(0.0);

    device
        .build_output_stream(
            config,
            move |out: &mut [f32], _info: &cpal::OutputCallbackInfo| {
                let target = if playing.load(Ordering::Relaxed) {
                    f32::from(volume.load(Ordering::Relaxed)) / 100.0
                } else {
                    0.0
                };

                // Paused and fully faded out: silence, and the ring keeps its
                // audio, so the resume has something to play instead of the
                // player having eaten it.
                if gain.is_silent() && target == 0.0 {
                    out.fill(0.0);
                    return;
                }

                let ready = consumer.slots().min(out.len());
                let mut copied = 0;
                for slot in &mut out[..ready] {
                    let Ok(sample) = consumer.pop() else {
                        break;
                    };
                    *slot = sample;
                    copied += 1;
                }

                if copied < out.len() {
                    out[copied..].fill(0.0);
                    // Silence once the queue is drained is the end of the
                    // music, not a gap. Only a starve while the producer still
                    // has work is a real underrun.
                    if producing.load(Ordering::Relaxed) {
                        underruns.fetch_add(1, Ordering::Relaxed);
                    }
                }

                // One linear fade per buffer carries the gain to the target:
                // pause and volume changes cannot click.
                gain.ramp_to(out, target);
                played.fetch_add(copied, Ordering::Release);
            },
            move |error| eprintln!("velo: audio device error: {error}"),
            None,
        )
        .map_err(|error| AudioError::Device(error.to_string()))
}

/// Fills the ring buffer from the file system until the queue is exhausted.
fn produce(
    job: Sequence,
    expected: TrackSpec,
    mut ring: Producer<f32>,
    delivered: &AtomicUsize,
    stop: &AtomicBool,
    current: &AtomicUsize,
    progress: &Mutex<Progress>,
) -> Result<(), AudioError> {
    let Sequence {
        paths,
        start_track,
        offset_frames,
    } = job;
    for (index, path) in paths.into_iter().enumerate() {
        if index < start_track {
            continue;
        }
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }

        // Announce which track is being produced before touching the file, so
        // a reader always sees a track that is either playing or about to.
        current.store(index, Ordering::Release);

        // The track a seek landed on starts at its offset: the player is
        // rebuilt there, nothing is injected into a running engine.
        let mut source = if index == start_track && offset_frames > 0 {
            TrackSource::open_at(&path, offset_frames)?
        } else {
            TrackSource::open(&path)?
        };

        // One write publishes both halves, so a reader can never see a track
        // without the point it began.
        let record = Progress {
            track: index,
            start: delivered.load(Ordering::Acquire),
        };
        match progress.lock() {
            Ok(mut current) => *current = record,
            Err(poisoned) => *poisoned.into_inner() = record,
        }

        let spec = source.spec();
        // A rate difference is converted away; only a layout difference cannot
        // be joined, because remixing channels changes the music itself.
        let mut converter = RateConverter::new(spec, expected)?;

        while let Some(block) = source.read_block()? {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            let resampled = converter.convert(&block)?;
            push_all(&mut ring, &resampled, delivered, stop);
        }

        // The resampler holds the track's last frames back until it sees what
        // follows them; without this the tail is heard as a cut-off ending.
        let flushed = converter.flush()?;
        push_all(&mut ring, &flushed, delivered, stop);
    }
    Ok(())
}

/// Converts a playback offset into interleaved samples at `spec`'s format.
///
/// The played counter counts interleaved samples, so a seek offset must be
/// expressed the same way. Storing it as frames made a stereo seek report half
/// the position it had landed on, and every later seek compounded the error.
fn offset_samples(offset: Duration, spec: TrackSpec) -> u64 {
    duration_to_frames(offset, spec.sample_rate) * u64::from(spec.channels)
}

/// Converts a playback offset into frames of the track's own sample rate.
fn duration_to_frames(offset: Duration, sample_rate: u32) -> u64 {
    let rate = u64::from(sample_rate);
    offset.as_secs() * rate + u64::from(offset.subsec_millis()) * rate / 1000
}

/// Opens the default output device at `track`'s own format when it accepts it,
/// otherwise at the device default, and the stream config to match.
fn open_output(
    track: TrackSpec,
) -> Result<(cpal::Device, cpal::StreamConfig, TrackSpec), AudioError> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| AudioError::Device("no audio output device".to_owned()))?;
    let spec = output_spec(&device, track)?;
    let config = cpal::StreamConfig {
        channels: spec.channels,
        sample_rate: spec.sample_rate,
        buffer_size: cpal::BufferSize::Default,
    };
    Ok((device, config, spec))
}

/// Waits, bounded, until the ring holds enough audio for the device to start
/// without its first callback finding it empty and being heard as a click.
fn wait_for_preroll(delivered: &AtomicUsize, preroll: usize) {
    let deadline = Instant::now() + Duration::from_millis(100);
    while delivered.load(Ordering::Acquire) < preroll && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(1));
    }
}

/// The format the device runs at: the first track's own format when the
/// device accepts it, otherwise the device default, to which every track is
/// then converted.
fn output_spec(device: &cpal::Device, track: TrackSpec) -> Result<TrackSpec, AudioError> {
    let accepts_track = device
        .supported_output_configs()
        .map_err(|error| AudioError::Device(error.to_string()))?
        .any(|range| {
            range.channels() == track.channels
                && range.min_sample_rate() <= track.sample_rate
                && track.sample_rate <= range.max_sample_rate()
                && range.sample_format() == cpal::SampleFormat::F32
        });
    if accepts_track {
        return Ok(track);
    }

    let default = device
        .default_output_config()
        .map_err(|error| AudioError::Device(error.to_string()))?;
    let default_rate = default.sample_rate();
    let output = TrackSpec {
        sample_rate: default_rate,
        channels: default.channels(),
    };
    if output.channels == track.channels {
        return Ok(output);
    }

    // A multichannel default device (an HDMI receiver, say) must not turn a
    // stereo track into an error: any configuration with a matching layout
    // works, at the device's own default rate.
    let accepts_layout = device
        .supported_output_configs()
        .map_err(|error| AudioError::Device(error.to_string()))?
        .any(|range| {
            range.channels() == track.channels
                && range.sample_format() == cpal::SampleFormat::F32
                && range.min_sample_rate() <= default_rate
                && default_rate <= range.max_sample_rate()
        });
    if accepts_layout {
        return Ok(TrackSpec {
            sample_rate: default_rate,
            channels: track.channels,
        });
    }

    // Resampling repairs a rate difference; nothing repairs a layout
    // difference without remixing, which is rejected instead.
    Err(AudioError::SpecMismatch {
        expected: output,
        got: track,
    })
}

/// Writes as much of `samples` as the ring accepts, sleeping while it is full.
fn push_all(
    ring: &mut Producer<f32>,
    mut samples: &[f32],
    delivered: &AtomicUsize,
    stop: &AtomicBool,
) {
    while !samples.is_empty() {
        if stop.load(Ordering::Relaxed) {
            return;
        }

        let room = ring.slots();
        if room == 0 {
            thread::sleep(POLL_INTERVAL);
            continue;
        }

        let wanted = room.min(samples.len());
        let mut pushed = 0;
        for &sample in &samples[..wanted] {
            if ring.push(sample).is_err() {
                break;
            }
            pushed += 1;
        }

        if pushed == 0 {
            thread::sleep(POLL_INTERVAL);
            continue;
        }

        delivered.fetch_add(pushed, Ordering::Release);
        samples = &samples[pushed..];
    }
}

/// Elapsed frames within the current track.
///
/// The played counter starts at zero when the device opens, but a seek opens it
/// deep inside the track the user asked for, so that track's offset has to be
/// added back or the clock restarts mid-song. Tracks after the seek target are
/// played from their beginning and take no offset.
const fn elapsed_samples(
    played: usize,
    track_start: usize,
    track: usize,
    seek_track: usize,
    seek_offset: usize,
) -> usize {
    let within_track = played.saturating_sub(track_start);
    if track == seek_track {
        within_track.saturating_add(seek_offset)
    } else {
        within_track
    }
}

#[cfg(test)]
mod tests {
    use super::elapsed_samples;

    #[test]
    fn playing_from_the_start_counts_only_what_was_played() {
        assert_eq!(elapsed_samples(1_000, 0, 0, 0, 0), 1_000);
    }

    #[test]
    fn a_seek_is_added_to_the_clock_instead_of_restarting_it() {
        // Seeks 5 s into the first track at 44.1 kHz, then plays 1 000 frames.
        assert_eq!(elapsed_samples(1_000, 0, 0, 0, 220_500), 221_500);
    }

    #[test]
    fn a_later_track_still_counts_from_its_own_beginning() {
        assert_eq!(elapsed_samples(3_000, 2_000, 1, 0, 220_500), 1_000);
    }

    #[test]
    fn a_seek_offset_counts_interleaved_samples_not_frames() {
        use super::{TrackSpec, offset_samples};
        use std::time::Duration;
        let stereo = TrackSpec {
            sample_rate: 44_100,
            channels: 2,
        };
        // One second of stereo at 44.1 kHz is 88 200 samples, not 44 100: the
        // counter it is subtracted from counts interleaved samples.
        assert_eq!(offset_samples(Duration::from_secs(1), stereo), 88_200);
        let mono = TrackSpec {
            sample_rate: 44_100,
            channels: 1,
        };
        assert_eq!(offset_samples(Duration::from_secs(1), mono), 44_100);
    }

    #[test]
    fn a_negative_clock_is_clamped_to_zero() {
        assert_eq!(elapsed_samples(0, 500, 1, 0, 220_500), 0);
    }
}
