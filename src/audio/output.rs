//! The realtime half of the audio path: a lock-free ring buffer fed by a
//! producer thread and drained by the device callback.
//!
//! The callback does exactly four things: measure how many samples are ready,
//! copy them, hand their mono mix to the spectrum tap, and fill the rest with
//! silence. No allocation, no locking, no logging, no decoding. Everything
//! expensive happens on the producer thread or in the interface.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rtrb::{Producer, RingBuffer};

use super::replay_gain::Gain;
use super::resample::{ChannelMap, RateConverter};
use super::{AudioError, TrackSource, TrackSpec};
use crate::config::ReplayGainMode;

/// Two seconds of audio in flight: long enough to survive a slow decode, short
/// enough to keep pause and stop responsive.
const RING_SECONDS: usize = 2;

/// How long the producer waits when the ring is full, i.e. how far ahead the
/// device is allowed to lag before we poll again.
const POLL_INTERVAL: Duration = Duration::from_millis(2);

/// How much mono audio the spectrum tap holds, in fractions of a second: the
/// interface drains it at least ten times a second, so half a second of room
/// means a slow frame drops samples, never the callback's time.
const TAP_DIVISOR: usize = 2;

/// How long the main thread waits before re-checking whether playback drained.
const DRAIN_POLL: Duration = Duration::from_millis(20);

/// A track the producer could not play, published so the interface can say so
/// and the queue can continue without it (T19): a corrupt file must not kill
/// an album, and it must not be silent either.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackFailure {
    /// The track that could not be played.
    pub path: PathBuf,
    /// What went wrong, as the audio path reports it.
    pub message: String,
}

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

/// What follows the end of a track.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Repeat {
    /// The next track, and silence after the last.
    #[default]
    Off,
    /// The next track, and the first again after the last.
    All,
    /// The same track again.
    One,
}

impl Repeat {
    /// The mode the repeat key moves to: off, all, one, and off again.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }

    /// The mode's name as the interface shows it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::All => "all",
            Self::One => "one",
        }
    }
}

/// The repeat mode a running player follows, shared with the interface: the
/// producer reads it at every track boundary, so a change takes effect at the
/// next one without rebuilding the stream. Never read by the device callback.
#[derive(Debug, Default)]
pub struct RepeatCell(AtomicU8);

impl RepeatCell {
    /// A cell holding `mode`.
    #[must_use]
    pub const fn new(mode: Repeat) -> Self {
        Self(AtomicU8::new(mode as u8))
    }

    /// The mode in force.
    #[must_use]
    pub fn get(&self) -> Repeat {
        match self.0.load(Ordering::Relaxed) {
            1 => Repeat::All,
            2 => Repeat::One,
            _ => Repeat::Off,
        }
    }

    /// Changes the mode from the next track boundary on.
    pub fn set(&self, mode: Repeat) {
        self.0.store(mode as u8, Ordering::Relaxed);
    }
}

/// The queue index that follows track `index` of `len` under `repeat`, or
/// `None` when the sequence ends there.
#[must_use]
pub const fn next_track(index: usize, len: usize, repeat: Repeat) -> Option<usize> {
    if len == 0 {
        return None;
    }
    match repeat {
        Repeat::One if index < len => Some(index),
        Repeat::All => Some((index + 1) % len),
        _ if index + 1 < len => Some(index + 1),
        _ => None,
    }
}

/// How far into a track "previous" restarts it instead of going back one.
const RESTART_AFTER: Duration = Duration::from_secs(3);

/// Whether the device stream must be closed and reopened for a track, from the
/// spec the stream is running at, the track's own spec and the
/// `bit_perfect` setting (T17).
///
/// The rule: a *rate* difference reopens only when bit-perfect is on —
/// otherwise it is converted away, as it always was. A *layout*-only
/// difference never reopens: channel layout maps instead (T20), because mono
/// into a stereo device is a device constraint, not a DSP choice, and a
/// layout-only swap would put a gap in every album that mixes the two.
#[must_use]
pub const fn needs_stream_reopen(stream: TrackSpec, track: TrackSpec, bit_perfect: bool) -> bool {
    bit_perfect && stream.sample_rate != track.sample_rate
}

/// The queue index a skip lands on, from track `current` of `len` at
/// `position` into it. Forward is the next track, or `None` past the last.
/// Back restarts the current track once it is [`RESTART_AFTER`] in — the way
/// every player's previous button works — and otherwise goes to the track
/// before; the first track has none, so it starts over.
#[must_use]
pub fn skip_target(current: usize, len: usize, position: Duration, forward: bool) -> Option<usize> {
    if forward {
        let next = current + 1;
        return (next < len).then_some(next);
    }
    if position >= RESTART_AFTER {
        return Some(current);
    }
    Some(current.saturating_sub(1))
}

/// A running output stream. Dropping it stops the device.
pub struct Player {
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
    /// The last track the producer had to skip, taken by the interface on its
    /// tick. A skipped track is not a sequence error, so it never reaches
    /// [`Player::wait_until_finished`] as one.
    failures: Arc<Mutex<VecDeque<TrackFailure>>>,
    /// How deep into its first track the stream began, in interleaved
    /// samples — the same unit as the played counter, which is what made a
    /// stereo seek report half its position when this was stored as frames.
    /// Without this the played counter makes the clock restart at zero in the
    /// middle of a song after every seek.
    start_offset_samples: usize,
    producer: Option<JoinHandle<Result<(), AudioError>>>,
    /// Everything about the device stream that a reopen replaces (T17): the
    /// stream itself, the spec it runs at and the reading end of the spectrum
    /// tap. The producer rebuilds them at a bit-perfect rate change; the
    /// player only reads.
    cell: Arc<StreamCell>,
    /// Messages meant for the user once, published by the producer and the
    /// construction path: the bit-perfect fallback notice, so far.
    notices: Arc<Mutex<VecDeque<String>>>,
    /// Whether this player runs bit-perfect: unity gain, always, whatever is
    /// asked of [`Player::set_volume`].
    bit_perfect: bool,
    /// The repeat mode the producer follows at each track boundary.
    repeat: Arc<RepeatCell>,
}

/// The parts of the output that a bit-perfect reopen swaps: the stream, the
/// spec it runs at, and the spectrum tap's reading end. Shared between the
/// player and the producer thread; every field is behind a lock that is held
/// only for the swap, never across a callback.
struct StreamCell {
    stream: Mutex<Option<cpal::Stream>>,
    spec: Mutex<TrackSpec>,
    tap: Mutex<rtrb::Consumer<f32>>,
}

impl StreamCell {
    fn spec(&self) -> TrackSpec {
        match self.spec.lock() {
            Ok(spec) => *spec,
            Err(poisoned) => *poisoned.into_inner(),
        }
    }

    fn take_stream(&self) -> Option<cpal::Stream> {
        match self.stream.lock() {
            Ok(mut slot) => slot.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        }
    }
}

/// The queue a player reads from, shared with whoever edits it.
///
/// The producer takes one entry at a time under this lock, so appending,
/// removing or moving a pending entry while it plays changes what plays next
/// (T18). The audio callback never touches it.
pub type SharedQueue = Arc<Mutex<Vec<PathBuf>>>;

/// Where the producer is: the queue index and the delivered count at which the
/// current track began. Only the producer writes it, only the UI reads it.
#[derive(Clone, Copy, Debug, Default)]
struct Progress {
    start: usize,
    duration: Option<Duration>,
    /// Whether this is the stream's first turn — the track a seek opened
    /// partway in. A repeat can bring the same index back, so the index alone
    /// cannot say it.
    first: bool,
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
    /// them, applying no `ReplayGain` beyond the built-in default (T11). A mode
    /// resolved from the config goes through [`Player::play_gapless_with_gain`].
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::Io`] when a path cannot be read,
    /// [`AudioError::Unsupported`] when its format is not decodable,
    /// [`AudioError::Device`] when no output device accepts the format, and
    /// [`AudioError::SpecMismatch`] when a track's channel layout cannot be
    /// joined to the output.
    pub fn play_gapless(queue: SharedQueue) -> Result<Self, AudioError> {
        Self::play_gapless_at_with_gain(queue, 0, Duration::ZERO, ReplayGainMode::default())
    }

    /// Like [`Player::play_gapless`], with the `ReplayGain` mode a caller has
    /// resolved from the config (T11, S15): the producer reads each track's
    /// tags and multiplies its samples on the way into the ring, per mode.
    ///
    /// # Errors
    ///
    /// The same errors as [`Player::play_gapless`]
    pub fn play_gapless_with_gain(
        queue: SharedQueue,
        gain_mode: ReplayGainMode,
    ) -> Result<Self, AudioError> {
        Self::play_gapless_at_with_gain(queue, 0, Duration::ZERO, gain_mode)
    }

    /// Plays every path from `track_index` onwards as one continuous stream,
    /// starting the first one `offset` into it, with the built-in default
    /// `ReplayGain` mode (T11). A mode resolved from the config goes through
    /// [`Player::play_gapless_at_with_gain`].
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::Unsupported`] when the list is empty or
    /// `track_index` is past it, and otherwise the same errors as
    /// [`Player::play_gapless`].
    pub fn play_gapless_at(
        queue: SharedQueue,
        track_index: usize,
        offset: Duration,
    ) -> Result<Self, AudioError> {
        Self::play_gapless_at_with_gain(queue, track_index, offset, ReplayGainMode::default())
    }

    /// Like [`Player::play_gapless_at`], with the `ReplayGain` mode a caller has
    /// resolved from the config (T11, S15). A seek lands on a rebuilt player,
    /// so this is also how a seek re-resolves the playing track's own gain.
    ///
    /// # Errors
    ///
    /// The same errors as [`Player::play_gapless_at`].
    pub fn play_gapless_at_with_gain(
        queue: SharedQueue,
        track_index: usize,
        offset: Duration,
        gain_mode: ReplayGainMode,
    ) -> Result<Self, AudioError> {
        Self::play_gapless_at_with(queue, track_index, offset, gain_mode, Repeat::Off)
    }

    /// Like [`Player::play_gapless_at_with_gain`], starting under `repeat`:
    /// the mode is in force from the first track boundary, and
    /// [`Player::set_repeat`] changes it while the stream plays.
    ///
    /// # Errors
    ///
    /// The same errors as [`Player::play_gapless_at`].
    pub fn play_gapless_at_with(
        queue: SharedQueue,
        track_index: usize,
        offset: Duration,
        gain_mode: ReplayGainMode,
        repeat: Repeat,
    ) -> Result<Self, AudioError> {
        Self::play_gapless_at_full(
            queue,
            track_index,
            offset,
            gain_mode,
            repeat,
            /* bit_perfect = */ false,
        )
    }

    /// [`Player::play_gapless_at_with`] under the `bit_perfect` setting
    /// (T17): the samples each decoder produces reach the device unmodified —
    /// no resampling, no `ReplayGain`, and the volume pinned to 100%. A track
    /// whose rate differs from the stream's closes it and reopens at its own
    /// rate (a small gap at that boundary is the accepted cost); consecutive
    /// tracks that share a spec stay one continuous stream, and a channel
    /// layout still maps. When the device has no output at a track's rate, the
    /// player falls back to the converted path and says so once — see
    /// [`Player::take_notices`].
    ///
    /// # Errors
    ///
    /// The same errors as [`Player::play_gapless_at_with`].
    #[expect(
        clippy::too_many_lines,
        reason = "the construction is one story: open the first track, open\n    the device on it, wire the cells and hand the rest to the producer\n    thread; splitting it would scatter that story"
    )]
    pub fn play_gapless_at_full(
        queue: SharedQueue,
        track_index: usize,
        offset: Duration,
        gain_mode: ReplayGainMode,
        repeat: Repeat,
        bit_perfect: bool,
    ) -> Result<Self, AudioError> {
        let (is_empty, len, first) = {
            let queue = match queue.lock() {
                Ok(queue) => queue,
                Err(poisoned) => poisoned.into_inner(),
            };
            (
                queue.is_empty(),
                queue.len(),
                queue.get(track_index).cloned(),
            )
        };
        if is_empty {
            return Err(AudioError::Unsupported("no tracks to play".to_owned()));
        }
        let Some(first) = first else {
            return Err(AudioError::Unsupported(
                "no such track in the queue".to_owned(),
            ));
        };
        let _ = len;

        // Opening once up front means a missing or broken first file fails
        // before a device is opened, and gives us the format to configure.
        let track_spec = TrackSource::open(&first)?.spec();
        let offset_frames = duration_to_frames(offset, track_spec.sample_rate);

        let requested_bit_perfect = bit_perfect;
        let (device, config, spec, opened_bit_perfect) = open_output(track_spec, bit_perfect)?;
        // Bit-perfect survives only when the device took the track's rate; a
        // fallback is the converted path, announced once (T17).
        let bit_perfect = bit_perfect && opened_bit_perfect;
        let notices = Arc::new(Mutex::new(VecDeque::new()));
        if requested_bit_perfect && !opened_bit_perfect {
            push_notice(
                &notices,
                format!(
                    "bit-perfect unavailable — the device has no {} Hz output",
                    track_spec.sample_rate
                ),
            );
        }
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
            start: 0,
            duration: None,
            first: true,
        }));
        let failures = Arc::new(Mutex::new(VecDeque::new()));
        let repeat = Arc::new(RepeatCell::new(repeat));
        let wires = CallbackWires {
            playing: Arc::new(AtomicBool::new(true)),
            producing: Arc::new(AtomicBool::new(true)),
            played: Arc::new(AtomicUsize::new(0)),
            underruns: Arc::new(AtomicUsize::new(0)),
            volume: Arc::new(AtomicU8::new(100)),
        };

        // The stream is built here, before a single sample exists, but not
        // started: the producer starts it once the ring holds the preroll, so
        // the first callback never finds the ring empty. The same rule —
        // build paused, start after the preroll — governs every bit-perfect
        // reopen (T17), which the producer performs itself.
        let (tap, tap_reader) = RingBuffer::<f32>::new(
            usize::try_from(spec.sample_rate).unwrap_or(48_000) / TAP_DIVISOR,
        );
        let stream = build_stream(&device, config, consumer, tap, &wires)?;
        let cell = Arc::new(StreamCell {
            stream: Mutex::new(Some(stream)),
            spec: Mutex::new(spec),
            tap: Mutex::new(tap_reader),
        });

        let producer = {
            let delivered = Arc::clone(&delivered);
            let stop = Arc::clone(&stop);
            let current = Arc::clone(&current);
            let progress = Arc::clone(&progress);
            let failures = Arc::clone(&failures);
            let repeat = Arc::clone(&repeat);
            let producing = Arc::clone(&wires.producing);
            let cell = Arc::clone(&cell);
            let wires = wires.clone();
            let notices = Arc::clone(&notices);
            thread::Builder::new()
                .name("velo-producer".to_owned())
                .spawn(move || {
                    let job = Sequence {
                        queue,
                        start_track: track_index,
                        offset_frames,
                        gain_mode,
                        bit_perfect,
                    };
                    let shared = Shared {
                        stop: &stop,
                        delivered: &delivered,
                        current: &current,
                        progress: &progress,
                        repeat: &repeat,
                    };
                    let mut out = StreamOut {
                        device,
                        cell,
                        wires,
                        ring,
                        delivered: Arc::clone(&delivered),
                        stop: Arc::clone(&stop),
                        failures,
                        notices,
                        preroll_at: spec.interleaved_rate() / 10,
                        started: false,
                    };
                    let result = run_sequence(job, spec, &shared, &mut out);
                    // A sequence shorter than the preroll never reached the
                    // threshold; its samples must still be heard.
                    out.ensure_started(true);
                    producing.store(false, Ordering::Release);
                    result
                })
                .map_err(|error| AudioError::Device(error.to_string()))?
        };

        Ok(Self {
            wires,
            stop,
            delivered,
            current,
            progress,
            failures,
            start_offset_samples: usize::try_from(clock_offset_samples).unwrap_or(usize::MAX),
            producer: Some(producer),
            cell,
            notices,
            bit_perfect,
            repeat,
        })
    }

    /// Changes what follows the current track, from its end on: the producer
    /// reads the mode at the boundary, so the music does not stop for it.
    pub fn set_repeat(&self, mode: Repeat) {
        self.repeat.set(mode);
    }

    /// Moves every mono sample the callback has played since the last call
    /// into `into`, oldest first. A paused player plays nothing and so taps
    /// nothing, which is how the spectrum falls when the music stops.
    pub fn drain_tap(&self, into: &mut Vec<f32>) {
        let mut tap = match self.cell.tap.lock() {
            Ok(tap) => tap,
            Err(poisoned) => poisoned.into_inner(),
        };
        let ready = tap.slots();
        let Ok(chunk) = tap.read_chunk(ready) else {
            return;
        };
        let (first, second) = chunk.as_slices();
        into.extend_from_slice(first);
        into.extend_from_slice(second);
        chunk.commit_all();
    }

    /// The format the device stream is running at right now. Bit-perfect
    /// playback reopens the stream at each track's own rate (T17), so this
    /// follows the track: readers — the clock and the spectrum — must read it
    /// live instead of holding on to an answer.
    #[must_use]
    pub fn spec(&self) -> TrackSpec {
        self.cell.spec()
    }

    /// Blocks until every queued track has been produced and played.
    ///
    /// # Errors
    ///
    /// Propagates the error when the track the player was built from cannot be
    /// opened. Tracks that fail after that are skipped and announced through
    /// [`Player::take_failure`] instead, so they do not fail the wait: a
    /// corrupt file must not kill an album (T19). Returns
    /// [`AudioError::Decode`] when the producer thread stopped unexpectedly.
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

    /// Takes the next skipped track the producer announced, if any. Taking —
    /// not reading — is the point: the interface shows a failure once, and the
    /// next [`Player::take_failure`] only sees a failure that came after it.
    #[must_use]
    pub fn take_failure(&self) -> Option<TrackFailure> {
        let mut queue = match self.failures.lock() {
            Ok(queue) => queue,
            Err(poisoned) => poisoned.into_inner(),
        };
        queue.pop_front()
    }

    /// Takes every skip announced so far, in the order they happened.
    ///
    /// The interface takes them one per tick, so a message stays readable for a
    /// moment; a command that runs to the end has no ticks and wants them all.
    #[must_use]
    pub fn take_failures(&self) -> Vec<TrackFailure> {
        let mut queue = match self.failures.lock() {
            Ok(queue) => queue,
            Err(poisoned) => poisoned.into_inner(),
        };
        queue.drain(..).collect()
    }

    /// Takes the next one-line notice the player published, if any: the
    /// bit-perfect fallback (T17) — a mode the device refused, so velo
    /// switched to the converted path and says so once. Taking — not reading —
    /// is the point, the same rule as [`Player::take_failure`].
    #[must_use]
    pub fn take_notice(&self) -> Option<String> {
        let mut notices = match self.notices.lock() {
            Ok(notices) => notices,
            Err(poisoned) => poisoned.into_inner(),
        };
        notices.pop_front()
    }

    /// Takes every notice published so far, in the order they happened.
    ///
    /// The interface takes them one per tick, so a message stays readable for
    /// a moment; a command that runs to the end has no ticks and wants them
    /// all.
    #[must_use]
    pub fn take_notices(&self) -> Vec<String> {
        let mut notices = match self.notices.lock() {
            Ok(notices) => notices,
            Err(poisoned) => poisoned.into_inner(),
        };
        notices.drain(..).collect()
    }

    /// How long the track that is playing lasts, when the decoder knows.
    #[must_use]
    pub fn current_duration(&self) -> Option<Duration> {
        let progress = match self.progress.lock() {
            Ok(progress) => *progress,
            Err(poisoned) => *poisoned.into_inner(),
        };
        progress.duration
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
    ///
    /// A bit-perfect player refuses: scaling samples is exactly what the mode
    /// exists to prevent (T17), so it stays at 100 whatever is asked, and the
    /// interface explains the pinning.
    pub fn set_volume(&self, percent: u8) {
        if self.bit_perfect {
            return;
        }
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
        let spec = self.cell.spec();
        let samples = elapsed_samples(
            self.wires.played.load(Ordering::Acquire),
            progress.start,
            progress.first,
            self.start_offset_samples,
        );
        #[expect(
            clippy::cast_precision_loss,
            reason = "a track holds fewer frames than f64 represents exactly"
        )]
        let seconds = samples as f64 / spec.interleaved_rate() as f64;
        Duration::from_secs_f64(seconds)
    }

    /// Asks the producer to stop and closes the device.
    pub fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        drop(self.cell.take_stream());
    }
}

impl Drop for Player {
    /// Closes the device stream, wherever the producer has last put it, and
    /// tells the producer to stop. Without this, a player dropped without an
    /// explicit [`Player::stop`] would leave the device playing.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        drop(self.cell.take_stream());
    }
}

/// Where a sequence starts: which track, and how far into it.
struct Sequence {
    queue: SharedQueue,
    start_track: usize,
    offset_frames: u64,
    /// The `ReplayGain` mode resolved from the config when playback started
    /// (T11); the producer resolves each track's own gain from it.
    gain_mode: ReplayGainMode,
    /// Bit-perfect playback (T17): a track whose rate differs from the
    /// stream's reopens the stream at its own rate instead of being converted
    /// into it, and the gain is skipped. Under a real device the reopen is a
    /// stream swap; headless, it only moves the expected spec.
    bit_perfect: bool,
}

/// What a sequence writes into: the ring of a running stream, or a headless
/// collector. The sequence decides *when* — how much to push, when a track
/// failed, when [`needs_stream_reopen`] says the stream must follow a track's
/// rate — and the sink effects it. That is what lets the same loop drive both
/// the producer thread and the headless drains.
trait SequenceSink {
    /// Hands one block of output samples to the stream or the collector.
    fn push_block(&mut self, samples: &[f32]);

    /// Announces a track the sequence had to skip.
    fn failure(&mut self, failure: TrackFailure);

    /// The sequence established that the stream must be reopened at `track`'s
    /// own sample rate (T17). Returns the spec the stream runs at from now on
    /// — the track's rate, or, when the device has no such output, the spec
    /// of the stream that stayed, to which the track is then converted.
    ///
    /// # Errors
    ///
    /// When the stream cannot be reopened and the current one cannot hold the
    /// track either — a device failure, not a format decision.
    fn reopen_stream(&mut self, track: TrackSpec) -> Result<TrackSpec, AudioError>;

    /// The turn ended. A stream that is still waiting for its preroll — its
    /// track was shorter than the preroll — starts anyway: a short track's
    /// samples must be heard (T17).
    fn after_turn(&mut self) {}
}

/// Builds the device stream: the callback that drains the ring. Its whole job
/// is copying samples, handing their mono mix to the spectrum tap and applying
/// the gain ramp — no allocation, no locks, no logging, no decoding.
fn build_stream(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut consumer: rtrb::Consumer<f32>,
    mut tap: Producer<f32>,
    wires: &CallbackWires,
) -> Result<cpal::Stream, AudioError> {
    let channels = usize::from(config.channels);
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
                // Before the gain: the spectrum shows the music, not the
                // volume knob. Wait-free; a full tap drops, never blocks.
                tap_mono(&out[..copied], channels, &mut tap);

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

/// Pushes the mono mix of every whole frame of `samples` into the spectrum
/// tap. Called from the device callback, so it may not wait: a sample that
/// does not fit is dropped, and a trailing partial frame is not a frame.
#[expect(
    clippy::cast_precision_loss,
    reason = "a channel count is a handful, exact in f32"
)]
pub fn tap_mono(samples: &[f32], channels: usize, tap: &mut Producer<f32>) {
    if channels == 0 {
        return;
    }
    let scale = 1.0 / channels as f32;
    for frame in samples.chunks_exact(channels) {
        if tap.push(frame.iter().sum::<f32>() * scale).is_err() {
            return;
        }
    }
}

/// The handles the producer shares with the player and the interface: the
/// stop flag, the delivered counter, the index and progress it publishes, and
/// the repeat mode it reads at every track boundary.
struct Shared<'a> {
    stop: &'a AtomicBool,
    delivered: &'a AtomicUsize,
    current: &'a AtomicUsize,
    progress: &'a Mutex<Progress>,
    repeat: &'a RepeatCell,
}

/// Fills the stream from the file system: the producer's device-side sink.
/// Owns the ring producer and, behind [`StreamCell`], the stream itself, so a
/// bit-perfect rate change can swap them mid-sequence (T17).
struct StreamOut {
    device: cpal::Device,
    cell: Arc<StreamCell>,
    wires: CallbackWires,
    ring: Producer<f32>,
    delivered: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    failures: Arc<Mutex<VecDeque<TrackFailure>>>,
    notices: Arc<Mutex<VecDeque<String>>>,
    /// The delivered count the stream waits for before it starts: the same
    /// head-start rule the first stream follows, re-armed at every reopen.
    preroll_at: usize,
    /// Whether the current stream has been started. A swapped stream is
    /// installed paused and starts once the ring holds its preroll.
    started: bool,
}

impl SequenceSink for StreamOut {
    fn push_block(&mut self, samples: &[f32]) {
        push_all(&mut self.ring, samples, &self.delivered, &self.stop);
        self.ensure_started(false);
    }

    fn failure(&mut self, failure: TrackFailure) {
        match self.failures.lock() {
            Ok(mut queue) => queue.push_back(failure),
            Err(poisoned) => poisoned.into_inner().push_back(failure),
        }
    }

    fn reopen_stream(&mut self, track: TrackSpec) -> Result<TrackSpec, AudioError> {
        // A stop raced in — the player is being dropped; keeping the current
        // stream lets the loop notice the stop at its next check.
        if self.stop.load(Ordering::Relaxed) {
            return Ok(self.cell.spec());
        }
        if let Ok(new_spec) = stream_spec_at(&self.device, track) {
            self.swap_stream(new_spec)?;
            return Ok(new_spec);
        }
        // The device has no output at this rate: the converted path
        // it is, once, and the track still plays (T17).
        push_notice(
            &self.notices,
            format!(
                "bit-perfect unavailable — the device has no {} Hz output",
                track.sample_rate
            ),
        );
        Ok(self.cell.spec())
    }

    fn after_turn(&mut self) {
        self.ensure_started(true);
    }
}

impl StreamOut {
    /// Starts the current stream once the ring holds its preroll, or at once
    /// when `force` says the wait is over (a short track, or the sequence's
    /// end). The stream itself is never rebuilt here — only started.
    fn ensure_started(&mut self, force: bool) {
        if self.started {
            return;
        }
        if !force && self.delivered.load(Ordering::Acquire) < self.preroll_at {
            return;
        }
        let slot = match self.cell.stream.lock() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(stream) = slot.as_ref()
            && let Err(error) = stream.play()
        {
            eprintln!("velo: audio device error: {error}");
        }
        self.started = true;
    }

    /// Closes the current stream and opens a new one at `new_spec`: a new
    /// ring (sized for the new rate), a new spectrum tap and the same
    /// callback wires. The samples still buffered in the old ring belong to
    /// the previous track's tail and are discarded — part of the accepted gap
    /// at a rate change. The new stream is installed paused; the preroll
    /// rule starts it.
    fn swap_stream(&mut self, new_spec: TrackSpec) -> Result<(), AudioError> {
        drop(self.cell.take_stream());
        let (ring, consumer) = RingBuffer::<f32>::new(new_spec.interleaved_rate() * RING_SECONDS);
        let (tap, tap_reader) = RingBuffer::<f32>::new(
            usize::try_from(new_spec.sample_rate).unwrap_or(48_000) / TAP_DIVISOR,
        );
        let config = cpal::StreamConfig {
            channels: new_spec.channels,
            sample_rate: new_spec.sample_rate,
            buffer_size: cpal::BufferSize::Default,
        };
        let stream = build_stream(&self.device, config, consumer, tap, &self.wires)?;
        {
            let mut slot = match self.cell.stream.lock() {
                Ok(slot) => slot,
                Err(poisoned) => poisoned.into_inner(),
            };
            *slot = Some(stream);
        }
        {
            let mut spec_slot = match self.cell.spec.lock() {
                Ok(slot) => slot,
                Err(poisoned) => poisoned.into_inner(),
            };
            *spec_slot = new_spec;
        }
        {
            let mut tap_slot = match self.cell.tap.lock() {
                Ok(slot) => slot,
                Err(poisoned) => poisoned.into_inner(),
            };
            *tap_slot = tap_reader;
        }
        self.ring = ring;
        self.preroll_at = self.delivered.load(Ordering::Acquire) + new_spec.interleaved_rate() / 10;
        self.started = false;
        // The player may have been dropped while this swap ran: then the new
        // stream must die with it, not play on behind a dead player.
        if self.stop.load(Ordering::Relaxed) {
            drop(self.cell.take_stream());
        }
        Ok(())
    }
}

/// Publishes a one-line user notice, once.
fn push_notice(notices: &Mutex<VecDeque<String>>, message: String) {
    match notices.lock() {
        Ok(mut queue) => queue.push_back(message),
        Err(poisoned) => poisoned.into_inner().push_back(message),
    }
}

/// The headless drains' sink: closures into a collector, with a reopen rule
/// supplied by each drain — bit-perfect ones follow the track's rate, the
/// others are never asked.
struct FnSink<'a> {
    block: &'a mut dyn FnMut(&[f32]),
    failure: &'a mut dyn FnMut(TrackFailure),
    reopen: &'a mut dyn FnMut(TrackSpec) -> Result<TrackSpec, AudioError>,
}

impl SequenceSink for FnSink<'_> {
    fn push_block(&mut self, samples: &[f32]) {
        (self.block)(samples);
    }

    fn failure(&mut self, failure: TrackFailure) {
        (self.failure)(failure);
    }

    fn reopen_stream(&mut self, track: TrackSpec) -> Result<TrackSpec, AudioError> {
        (self.reopen)(track)
    }
}

/// Copies one queue entry out with the queue's length, so the lock is held
/// only for the copy.
fn queued_entry(queue: &Mutex<Vec<PathBuf>>, index: usize) -> (Option<PathBuf>, usize) {
    let read = |queue: &Vec<PathBuf>| (queue.get(index).cloned(), queue.len());
    match queue.lock() {
        Ok(queue) => read(&queue),
        Err(poisoned) => read(&poisoned.into_inner()),
    }
}

/// A track failure record for the interface: the path it failed, and why.
fn track_failure(path: &std::path::Path, error: &AudioError) -> TrackFailure {
    TrackFailure {
        path: path.to_path_buf(),
        message: error.to_string(),
    }
}

/// Produces the tracks of a job from `start_track` on, in the order the
/// repeat mode picks, handing each block of output samples to the sink and
/// every track it had to skip with it. The ring, the counters and the failure
/// slot all live behind the sink and the shared handles, which is what lets
/// the headless drains run the same loop without a device.
fn run_sequence(
    job: Sequence,
    expected: TrackSpec,
    shared: &Shared<'_>,
    sink: &mut dyn SequenceSink,
) -> Result<(), AudioError> {
    let Sequence {
        queue,
        start_track,
        offset_frames,
        gain_mode,
        bit_perfect,
    } = job;
    let mut expected = expected;
    let mut turn = TurnJob {
        index: start_track,
        // The first turn is the track the player was built from — opened at
        // selection time, perhaps partway in. A repeat can bring its index
        // back, so the turn, not the index, says which one it is.
        first: true,
        offset_frames,
        gain_mode,
        bit_perfect,
    };
    // Tracks that failed one after another: once that is the whole queue, a
    // repeat would only spin through failures, so the sequence ends instead.
    let mut failed_in_a_row = 0;
    loop {
        if shared.stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        // One entry per turn, under the lock, because the queue is shared: what
        // the interface appended, removed or moved while this runs is what
        // plays next. Past the end is how a sequence finishes; the queue can
        // also shrink.
        let (Some(path), _) = queued_entry(&queue, turn.index) else {
            return Ok(());
        };
        let outcome = play_turn(&path, &turn, &mut expected, shared, sink)?;
        sink.after_turn();

        // The length now, not when the track began: an entry appended while
        // it played is what comes next (T18).
        let len = queued_entry(&queue, turn.index).1;
        let mut repeat = shared.repeat.get();
        match outcome {
            Turn::Stopped => return Ok(()),
            Turn::Played => failed_in_a_row = 0,
            // A turn that delivered no audio counts like a failure: nothing
            // reached the ring to slow the loop down, so repeating it would
            // reopen the file as fast as the disk allows.
            Turn::Failed | Turn::Silent => {
                failed_in_a_row += 1;
                if failed_in_a_row >= len {
                    return Ok(());
                }
                // A track that failed or held nothing is never repeated on
                // its own.
                if repeat == Repeat::One {
                    repeat = Repeat::Off;
                }
            }
        }
        let Some(next) = next_track(turn.index, len, repeat) else {
            return Ok(());
        };
        turn.index = next;
        turn.first = false;
    }
}

/// Which track a turn plays and how it opens it.
struct TurnJob {
    index: usize,
    first: bool,
    offset_frames: u64,
    gain_mode: ReplayGainMode,
    bit_perfect: bool,
}

/// How one turn of the sequence ended.
enum Turn {
    /// The track played to its end.
    Played,
    /// The track opened and ended without a single sample.
    Silent,
    /// The track was skipped and announced.
    Failed,
    /// The stream was asked to stop.
    Stopped,
}

/// Plays one track into the sink. The track the player was built from
/// failing to open is the sequence error the interface already reports;
/// every other failure is announced and costs this one track, so one bad file
/// does not take the rest of the album with it (T19).
fn play_turn(
    path: &std::path::Path,
    turn: &TurnJob,
    expected: &mut TrackSpec,
    shared: &Shared<'_>,
    sink: &mut dyn SequenceSink,
) -> Result<Turn, AudioError> {
    let opened = if turn.first && turn.offset_frames > 0 {
        TrackSource::open_at(path, turn.offset_frames)
    } else {
        TrackSource::open(path)
    };
    let mut source = match opened {
        Ok(source) => source,
        Err(error) if turn.first => return Err(error),
        Err(error) => {
            sink.failure(track_failure(path, &error));
            return Ok(Turn::Failed);
        }
    };

    // Announce which track is being produced once it has opened, so a reader
    // only ever sees a track that is playing or about to — never one that is
    // about to be skipped.
    shared.current.store(turn.index, Ordering::Release);

    // One write publishes every part, so a reader can never see a track
    // without the point it began.
    let record = Progress {
        start: shared.delivered.load(Ordering::Acquire),
        duration: source.duration(),
        first: turn.first,
    };
    match shared.progress.lock() {
        Ok(mut slot) => *slot = record,
        Err(poisoned) => *poisoned.into_inner() = record,
    }

    let spec = source.spec();
    // Bit-perfect (T17): when the track's rate differs from the stream's, the
    // stream closes and reopens at the track's own rate — the sink swaps it,
    // or falls back to the current stream when the device has no such output.
    // A layout difference never reopens: the mapping below still applies.
    if turn.bit_perfect && needs_stream_reopen(*expected, spec, turn.bit_perfect) {
        *expected = sink.reopen_stream(spec)?;
    }
    // A rate difference is converted away, and a layout difference is mapped:
    // mono into stereo is a duplicate, not a remix, and refusing it made every
    // mono file unplayable. A layout that would need real downmix coefficients
    // is announced and skipped, because a device that cannot hold one track
    // must not silence the album (T20).
    let map = match ChannelMap::new(spec.channels, expected.channels) {
        Ok(map) => map,
        Err(error) => {
            sink.failure(track_failure(path, &error));
            return Ok(Turn::Failed);
        }
    };
    let mapped_spec = TrackSpec {
        sample_rate: spec.sample_rate,
        channels: expected.channels,
    };
    let mut converter = RateConverter::new(mapped_spec, *expected)?;

    // ReplayGain belongs to the track: recomputed once per track change, never
    // per queue (T11, T18); multiplied below, before the ring, so the user's
    // volume ramp stays exactly what it was. Bit-perfect skips it entirely
    // (T17): scaling samples is what the mode refuses, even from a tag.
    let gain = if turn.bit_perfect {
        Gain::identity()
    } else {
        Gain::read_from(path, turn.gain_mode)
    };
    let mut heard = false;
    loop {
        if shared.stop.load(Ordering::Relaxed) {
            return Ok(Turn::Stopped);
        }
        let block = match source.read_block() {
            Ok(Some(block)) => block,
            Ok(None) => break,
            // A track that stops decoding costs itself, not the album: what it
            // decoded stays, the rest is skipped, the next one follows.
            Err(error) => {
                sink.failure(track_failure(path, &error));
                return Ok(Turn::Failed);
            }
        };
        let mut resampled = match converter.convert(&map.apply(&block)) {
            Ok(resampled) => resampled,
            Err(error) => {
                sink.failure(track_failure(path, &error));
                return Ok(Turn::Failed);
            }
        };
        // The gain's one multiplication, before the ring: the callback keeps
        // copying and ramping (T11).
        gain.apply(&mut resampled);
        heard |= !resampled.is_empty();
        sink.push_block(&resampled);
    }

    // The resampler holds the track's last frames back until it sees what
    // follows them; without this the tail is heard as a cut-off ending.
    let mut flushed = match converter.flush() {
        Ok(flushed) => flushed,
        Err(error) => {
            sink.failure(track_failure(path, &error));
            return Ok(Turn::Failed);
        }
    };
    gain.apply(&mut flushed);
    heard |= !flushed.is_empty();
    if !flushed.is_empty() {
        sink.push_block(&flushed);
    }
    Ok(if heard { Turn::Played } else { Turn::Silent })
}

/// Drains a queue the way the producer writes it, without a device: every
/// block of output samples goes to `on_block`, every track that had to be
/// skipped goes to `on_failure`, and the queue is a success even when tracks
/// were skipped (T19). Test support for the skip rule, with the built-in
/// default `ReplayGain` mode; [`drain_headless_with_gain`] is the same drain
/// under a resolved mode (T11).
///
/// # Errors
///
/// Returns the error of the track at `start_track` when it cannot be opened —
/// the one failure that is a sequence error, because it is reported at
/// selection time — and [`AudioError::SpecMismatch`] when two tracks in the
/// queue disagree on their channel layout.
pub fn drain_headless(
    queue: SharedQueue,
    start_track: usize,
    offset_frames: u64,
    expected: TrackSpec,
    stop: &AtomicBool,
    on_block: &mut dyn FnMut(&[f32]),
    on_failure: &mut dyn FnMut(TrackFailure),
) -> Result<(), AudioError> {
    drain_headless_with_gain(
        queue,
        start_track,
        offset_frames,
        expected,
        ReplayGainMode::default(),
        stop,
        on_block,
        on_failure,
    )
}

/// [`drain_headless`] under an explicit `ReplayGain` mode (T11, S15): the
/// producer resolves each track's gain from `gain_mode` and multiplies every
/// block on its way into the ring, so the same loop proves where the gain
/// reaches the audio without a device.
///
/// # Errors
///
/// The same errors as [`drain_headless`].
#[expect(
    clippy::too_many_arguments,
    reason = "the job, the mode and the two sinks are the whole contract of\n    the drain; bundling them would only hide what is shared"
)]
pub fn drain_headless_with_gain(
    queue: SharedQueue,
    start_track: usize,
    offset_frames: u64,
    expected: TrackSpec,
    gain_mode: ReplayGainMode,
    stop: &AtomicBool,
    on_block: &mut dyn FnMut(&[f32]),
    on_failure: &mut dyn FnMut(TrackFailure),
) -> Result<(), AudioError> {
    let job = Sequence {
        queue,
        start_track,
        offset_frames,
        gain_mode,
        bit_perfect: false,
    };
    let mut refuse = |_: TrackSpec| -> Result<TrackSpec, AudioError> {
        unreachable!("only bit-perfect sequences reopen the stream")
    };
    drain(
        job,
        expected,
        &RepeatCell::default(),
        stop,
        &mut FnSink {
            block: on_block,
            failure: on_failure,
            reopen: &mut refuse,
        },
    )
}

/// [`drain_headless`] under a repeat mode the caller can change while it
/// runs, the way the interface changes a playing stream's: the loop reads it
/// at every track boundary. Under `All` or `One` the drain only ends when
/// `stop` is set or every track fails in a row.
///
/// # Errors
///
/// The same errors as [`drain_headless`].
#[expect(
    clippy::too_many_arguments,
    reason = "the job, the mode and the two sinks are the whole contract of\n    the drain; bundling them would only hide what is shared"
)]
pub fn drain_headless_repeating(
    queue: SharedQueue,
    start_track: usize,
    offset_frames: u64,
    expected: TrackSpec,
    repeat: &RepeatCell,
    stop: &AtomicBool,
    on_block: &mut dyn FnMut(&[f32]),
    on_failure: &mut dyn FnMut(TrackFailure),
) -> Result<(), AudioError> {
    let job = Sequence {
        queue,
        start_track,
        offset_frames,
        gain_mode: ReplayGainMode::default(),
        bit_perfect: false,
    };
    let mut refuse = |_: TrackSpec| -> Result<TrackSpec, AudioError> {
        unreachable!("only bit-perfect sequences reopen the stream")
    };
    drain(
        job,
        expected,
        repeat,
        stop,
        &mut FnSink {
            block: on_block,
            failure: on_failure,
            reopen: &mut refuse,
        },
    )
}

/// [`drain_headless_with_gain`] under bit-perfect playback (T17): the stream
/// follows each track's own sample rate instead of converting it, and the
/// gain is skipped whatever `gain_mode` says — so the drained samples must be
/// bit-identical to the decoder's own output, which is exactly what the tests
/// assert. No device is opened: the reopen only moves the expected spec.
///
/// # Errors
///
/// The same errors as [`drain_headless`].
#[expect(
    clippy::too_many_arguments,
    reason = "the job, the mode and the two sinks are the whole contract of\n    the drain; bundling them would only hide what is shared"
)]
pub fn drain_headless_bit_perfect(
    queue: SharedQueue,
    start_track: usize,
    offset_frames: u64,
    expected: TrackSpec,
    gain_mode: ReplayGainMode,
    stop: &AtomicBool,
    on_block: &mut dyn FnMut(&[f32]),
    on_failure: &mut dyn FnMut(TrackFailure),
) -> Result<(), AudioError> {
    let job = Sequence {
        queue,
        start_track,
        offset_frames,
        gain_mode,
        bit_perfect: true,
    };
    let channels = expected.channels;
    let mut follow = move |track: TrackSpec| -> Result<TrackSpec, AudioError> {
        // Headless, the reopen only retunes the stream's rate: a layout
        // still maps, exactly as on the device.
        Ok(TrackSpec {
            sample_rate: track.sample_rate,
            channels,
        })
    };
    drain(
        job,
        expected,
        &RepeatCell::default(),
        stop,
        &mut FnSink {
            block: on_block,
            failure: on_failure,
            reopen: &mut follow,
        },
    )
}

/// The headless drains' one body: the producer's loop with local counters.
fn drain(
    job: Sequence,
    expected: TrackSpec,
    repeat: &RepeatCell,
    stop: &AtomicBool,
    sink: &mut dyn SequenceSink,
) -> Result<(), AudioError> {
    let delivered = AtomicUsize::new(0);
    let current = AtomicUsize::new(job.start_track);
    let progress = Mutex::new(Progress {
        start: 0,
        duration: None,
        first: true,
    });
    let shared = Shared {
        stop,
        delivered: &delivered,
        current: &current,
        progress: &progress,
        repeat,
    };
    run_sequence(job, expected, &shared, sink)
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
/// otherwise at the device default, and the stream config to match. Under
/// bit-perfect (T17), the device is opened at the track's own sample rate —
/// and the boolean says whether that worked; when it did not, the caller
/// falls back to the converted path and announces it.
fn open_output(
    track: TrackSpec,
    bit_perfect: bool,
) -> Result<(cpal::Device, cpal::StreamConfig, TrackSpec, bool), AudioError> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| AudioError::Device("no audio output device".to_owned()))?;
    let (spec, exact) = if bit_perfect {
        match stream_spec_at(&device, track) {
            Ok(spec) => (spec, true),
            Err(_) => (output_spec(&device, track)?, false),
        }
    } else {
        (output_spec(&device, track)?, false)
    };
    let config = cpal::StreamConfig {
        channels: spec.channels,
        sample_rate: spec.sample_rate,
        buffer_size: cpal::BufferSize::Default,
    };
    Ok((device, config, spec, exact))
}

/// The spec for a bit-perfect stream: the track's own sample rate, on a
/// device config the device actually offers there (T17). The track's own
/// channel count is preferred; another count still maps, so a mono track
/// keeps playing on every channel. No config at the track's rate is the
/// fallback's cue, not a failure of the track.
fn stream_spec_at(device: &cpal::Device, track: TrackSpec) -> Result<TrackSpec, AudioError> {
    let configs = device
        .supported_output_configs()
        .map_err(|error| AudioError::Device(error.to_string()))?;
    let mut mapped_channels: Option<u16> = None;
    for range in configs {
        if range.sample_format() != cpal::SampleFormat::F32
            || range.min_sample_rate() > track.sample_rate
            || track.sample_rate > range.max_sample_rate()
        {
            continue;
        }
        if range.channels() == track.channels {
            return Ok(track);
        }
        if mapped_channels.is_none() {
            mapped_channels = Some(range.channels());
        }
    }
    match mapped_channels {
        Some(channels) => Ok(TrackSpec {
            sample_rate: track.sample_rate,
            channels,
        }),
        None => Err(AudioError::Device(format!(
            "the device has no {} Hz output",
            track.sample_rate
        ))),
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
    // A layout difference no longer rules the device out: every track is
    // mapped into the device's layout, so its default format is always usable.
    Ok(output)
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
/// added back or the clock restarts mid-song. Every later turn — the tracks
/// after it, and the same track again under a repeat — plays from its
/// beginning and takes no offset.
const fn elapsed_samples(
    played: usize,
    track_start: usize,
    first_turn: bool,
    seek_offset: usize,
) -> usize {
    let within_track = played.saturating_sub(track_start);
    if first_turn {
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
        assert_eq!(elapsed_samples(1_000, 0, true, 0), 1_000);
    }

    #[test]
    fn a_seek_is_added_to_the_clock_instead_of_restarting_it() {
        // Seeks 5 s into the first track at 44.1 kHz, then plays 1 000 frames.
        assert_eq!(elapsed_samples(1_000, 0, true, 220_500), 221_500);
    }

    #[test]
    fn a_later_track_still_counts_from_its_own_beginning() {
        assert_eq!(elapsed_samples(3_000, 2_000, false, 220_500), 1_000);
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
        assert_eq!(elapsed_samples(0, 500, false, 220_500), 0);
    }
}
