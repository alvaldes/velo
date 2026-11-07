//! The realtime half of the audio path: a lock-free ring buffer fed by a
//! producer thread and drained by the device callback.
//!
//! The callback does exactly three things: measure how many samples are ready,
//! copy them, and fill the rest with silence. No allocation, no locking, no
//! logging, no decoding. Everything expensive happens on the producer thread.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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

/// A running output stream. Dropping it stops the device.
pub struct Player {
    stream: cpal::Stream,
    spec: TrackSpec,
    playing: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    delivered: Arc<AtomicUsize>,
    played: Arc<AtomicUsize>,
    underruns: Arc<AtomicUsize>,
    /// The queue index the producer is currently decoding; the UI reads it to
    /// mark the playing row.
    current: Arc<AtomicUsize>,
    producer: Option<JoinHandle<Result<(), AudioError>>>,
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
        if paths.is_empty() {
            return Err(AudioError::Unsupported("no tracks to play".to_owned()));
        }

        // Opening once up front means a missing or broken first file fails
        // before a device is opened, and gives us the format to configure.
        let spec = TrackSource::open(&paths[0])?.spec();

        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| AudioError::Device("no audio output device".to_owned()))?;
        let spec = output_spec(&device, spec)?;

        let config = cpal::StreamConfig {
            channels: spec.channels,
            sample_rate: spec.sample_rate,
            buffer_size: cpal::BufferSize::Default,
        };

        let (ring, mut consumer) = RingBuffer::<f32>::new(spec.interleaved_rate() * RING_SECONDS);

        let playing = Arc::new(AtomicBool::new(true));
        let stop = Arc::new(AtomicBool::new(false));
        let producing = Arc::new(AtomicBool::new(true));
        let delivered = Arc::new(AtomicUsize::new(0));
        let played = Arc::new(AtomicUsize::new(0));
        let underruns = Arc::new(AtomicUsize::new(0));
        // The first track is the one being produced until the producer says
        // otherwise; it sets the index before it opens each track.
        let current = Arc::new(AtomicUsize::new(0));

        let producer = {
            let delivered = Arc::clone(&delivered);
            let stop = Arc::clone(&stop);
            let producing = Arc::clone(&producing);
            let current = Arc::clone(&current);
            thread::Builder::new()
                .name("velo-producer".to_owned())
                .spawn(move || {
                    let result = produce(paths, spec, ring, &delivered, &stop, &current);
                    producing.store(false, Ordering::Release);
                    result
                })
                .map_err(|error| AudioError::Device(error.to_string()))?
        };

        // Give the producer a head start before the device starts asking for
        // samples: a callback that finds the ring empty at the start is heard as
        // a click. The wait is bounded, so a broken first file cannot hang here.
        let preroll = spec.interleaved_rate() / 10;
        let deadline = Instant::now() + Duration::from_millis(100);
        while delivered.load(Ordering::Acquire) < preroll && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }

        let stream = {
            let playing = Arc::clone(&playing);
            let played = Arc::clone(&played);
            let underruns = Arc::clone(&underruns);
            let producing = Arc::clone(&producing);

            device
                .build_output_stream(
                    config,
                    move |out: &mut [f32], _info: &cpal::OutputCallbackInfo| {
                        if !playing.load(Ordering::Relaxed) {
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
                            // music, not a gap. Only a starve while the producer
                            // still has work is a real underrun.
                            if producing.load(Ordering::Relaxed) {
                                underruns.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        played.fetch_add(copied, Ordering::Release);
                    },
                    move |error| eprintln!("velo: audio device error: {error}"),
                    None,
                )
                .map_err(|error| AudioError::Device(error.to_string()))?
        };

        stream
            .play()
            .map_err(|error| AudioError::Device(error.to_string()))?;

        Ok(Self {
            stream,
            spec,
            playing,
            stop,
            delivered,
            played,
            underruns,
            current,
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
        while self.delivered.load(Ordering::Acquire) > self.played.load(Ordering::Acquire) {
            thread::sleep(DRAIN_POLL);
        }
        Ok(())
    }

    /// Moments where the device asked for samples the producer had not written
    /// yet. A steady zero here is what "no gap" sounds like.
    #[must_use]
    pub fn underruns(&self) -> usize {
        self.underruns.load(Ordering::Relaxed)
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
        self.playing.store(playing, Ordering::Relaxed);
    }

    /// Asks the producer to stop and closes the device.
    pub fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        drop(self.stream);
    }
}

/// Fills the ring buffer from the file system until the queue is exhausted.
fn produce(
    paths: Vec<PathBuf>,
    expected: TrackSpec,
    mut ring: Producer<f32>,
    delivered: &AtomicUsize,
    stop: &AtomicBool,
    current: &AtomicUsize,
) -> Result<(), AudioError> {
    for (index, path) in paths.into_iter().enumerate() {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }

        // Announce which track is being produced before touching the file, so
        // a reader always sees a track that is either playing or about to.
        current.store(index, Ordering::Release);

        let mut source = TrackSource::open(&path)?;
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
