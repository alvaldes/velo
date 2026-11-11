//! The realtime half of the audio path: a lock-free ring buffer fed by a
//! producer thread and drained by the device callback.
//!
//! The callback does exactly four things: measure how many samples are ready,
//! copy them, hand their mono mix to the spectrum tap, and fill the rest with
//! silence. No allocation, no locking, no logging, no decoding. Everything
//! expensive happens on the producer thread or in the interface.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rtrb::{Producer, RingBuffer};

use super::replay_gain::Gain;
use super::resample::{ChannelMap, RateConverter};
use super::{AudioError, TrackSource, TrackSpec};
use crate::config::ReplayGainMode;
use crate::radio::stream::{LiveOpener, StreamError};

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

/// The station retry policy (T5c2b, S2): how many attempts a failing
/// station gets and how long the producer waits between them. The policy is
/// data so it travels from the caller — [`Retry::NONE`] is the default and
/// the fast path; [`Retry::ONCE`] is production's explicit ask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retry {
    /// Total attempts per episode: 1 is no retry, 2 is exactly one retry.
    /// The budget belongs to the turn, not to one connection: it is spent by
    /// failures and restored only by audio actually emitted, so a station
    /// that accepts and cuts at once burns the same budget as one that
    /// played — never a loop.
    pub attempts: u32,
    /// How long the producer waits between attempts, on its own thread —
    /// never in the audio callback, and never before the first attempt.
    pub delay: Duration,
}

impl Retry {
    /// No retry: one attempt, no wait. The default, so a caller that
    /// forgets to choose a policy gets the path that never sleeps.
    pub const NONE: Self = Self {
        attempts: 1,
        delay: Duration::ZERO,
    };

    /// Production (S2): exactly one retry after 2 s.
    pub const ONCE: Self = Self {
        attempts: 2,
        delay: Duration::from_secs(2),
    };
}

impl Default for Retry {
    /// [`Retry::NONE`] — the fast path, never a hidden 2 s sleep.
    fn default() -> Self {
        Self::NONE
    }
}

/// A track the producer could not play, published so the interface can say so
/// and the queue can continue without it (T19): a corrupt file must not kill
/// an album, and it must not be silent either.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackFailure {
    /// The failed entry, named exactly as the status line prints it: a
    /// file's file name, a station's own name — never a URL, never a path
    /// that would leak a hash-named cache file (T5c2a).
    pub label: String,
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
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

/// Whether the output folds every frame's channels into one value both
/// channels then carry (issue #8): a downmix to mono, shared with the
/// interface the way the repeat mode is, so `--mono` starts a session in mono
/// and `M` toggles it while playing. The producer reads it at every block, so
/// the toggle reaches the sample path without restarting the stream; the
/// average is taken after `ReplayGain` and before the ring. Never read by the
/// device callback.
#[derive(Debug, Default)]
pub struct MonoCell(AtomicBool);

impl MonoCell {
    /// A cell holding `mono`.
    #[must_use]
    pub const fn new(mono: bool) -> Self {
        Self(AtomicBool::new(mono))
    }

    /// Whether the mix is folded down to mono.
    #[must_use]
    pub fn get(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// Changes the downmix from the next block on.
    pub fn set(&self, mono: bool) {
        self.0.store(mono, Ordering::Relaxed);
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
    /// The queue index the producer is fetching right now, stored index plus
    /// one, zero when none (issue #60): the interface reads it on its tick to
    /// say `⟳ fetching …` on the status line for as long as the download
    /// runs. The same cell the headless drains take from their caller.
    fetching: Arc<AtomicU64>,
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
    /// The name of the device the stream is playing on (issue #44, S1): the
    /// row the device picker marks while this player runs.
    device_name: String,
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
    /// Whether the mono downmix is in force (issue #8): the cell is shared
    /// with the producer, so [`Player::set_mono`] reaches the sample path
    /// without restarting the stream.
    mono: Arc<MonoCell>,
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

/// One entry of the queue: what the producer turns into sound. The queue is
/// typed so an entry can say what it is instead of every consumer guessing
/// from a path alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// A file on disk, possibly a remote one the resolver has yet to cache.
    File(PathBuf),
    /// A live radio station: played as a stream from `url` as its bytes
    /// arrive (S2), never downloaded, never resolved to a path.
    Station {
        /// The stream URL the directory listed (`url_resolved`).
        url: String,
        /// The station's own, user-facing name: what the queue displays
        /// and what a failure message prints — never the URL.
        name: String,
    },
}

impl Entry {
    /// The path the entry stands for: the file's own, for [`Entry::File`].
    /// A station has no path — this is `None`, never an invented or empty
    /// one; its human label goes through [`Entry::label`].
    #[must_use]
    pub fn as_path(&self) -> Option<&Path> {
        match self {
            Self::File(path) => Some(path),
            Self::Station { .. } => None,
        }
    }

    /// The name a user reads for this entry, whichever variant it is: a
    /// file's file name, a station's own name.
    #[must_use]
    pub fn label(&self) -> Cow<'_, str> {
        match self {
            Self::File(path) => path
                .file_name()
                .map_or_else(|| path.to_string_lossy(), std::ffi::OsStr::to_string_lossy),
            Self::Station { name, .. } => Cow::Borrowed(name),
        }
    }
}

impl From<PathBuf> for Entry {
    fn from(path: PathBuf) -> Self {
        Self::File(path)
    }
}

/// The queue a player reads from, shared with whoever edits it.
///
/// The producer takes one entry at a time under this lock, so appending,
/// removing or moving a pending entry while it plays changes what plays next
/// (T18). The audio callback never touches it.
pub type SharedQueue = Arc<Mutex<Vec<Entry>>>;

/// Resolves one queue entry into the file the producer opens, right before
/// the producer reaches it (T12c). A remote row's `subsonic://…` entry
/// becomes the cached file of its original bytes — a hit costs no network, a
/// miss downloads first — and a failure is a skip the producer announces, not
/// a death of the queue (T19). `None` (the default everywhere) means every
/// entry already is a file, which is today's behaviour.
///
/// Called only from the producer thread — never from the audio callback —
/// and as many times as a track starts, so implementations must be cheap on
/// a hit and safe to call again.
pub trait PathResolver: Send + Sync {
    /// The file to open for `path`. A local path is usually returned as it
    /// is; only implementations know which entries are theirs.
    ///
    /// # Errors
    ///
    /// Fails when the entry cannot be turned into a playable file: the
    /// producer announces the failure and continues with the queue.
    fn resolve(&self, path: &Path) -> Result<PathBuf, AudioError>;

    /// Whether `path` is already the file it resolves to, so reaching it
    /// costs no network and no wait. The default answers yes: a resolver
    /// that never downloads has nothing that waits, and a local file is
    /// already on disk. Implementations that download answer from their
    /// cache — never by fetching — which is what lets the interface ask
    /// "will reaching this stall" without re-deriving their rules (issue
    /// #60).
    fn cached(&self, _path: &Path) -> bool {
        true
    }
}

/// One opened live station, as the producer reads it: the same surface a
/// file's `TrackSource` answers, minus everything a stream cannot have — a
/// duration is always `None` and a seek never happens (S3) — plus the title
/// the station announces, which the interface shows (S4).
pub trait StationSource: Send {
    /// The next block of interleaved samples, or `None` at the end of the
    /// stream.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError`] when the body stalls past its read bound or
    /// the decoder fails; the caller decides what follows — here, one
    /// retry, then a failure announcement and the next queue entry (the
    /// retry policy is T5c2b).
    fn read_block(&mut self) -> Result<Option<Vec<f32>>, AudioError>;

    /// The format the station decodes at.
    fn spec(&self) -> TrackSpec;

    /// A live station never declares a duration: this is `None` by contract.
    fn duration(&self) -> Option<Duration>;

    /// The latest `StreamTitle`, or `None` before the first one arrives.
    fn title(&self) -> Option<String>;
}

/// Opens a station URL into a [`StationSource`]. The seam that keeps tests
/// off the network, in the same spirit as [`PathResolver`]: production goes
/// through `radio::stream::open` with the default per-operation timeouts
/// ([`LiveOpener`]); a test injects a fake over fixture bytes. Called once
/// per station open — a retry opens through it again (T5c2b).
pub trait StationOpener: Send + Sync {
    /// # Errors
    ///
    /// Fails when the station cannot be reached or its body does not
    /// decode; the caller retries exactly once after its injected delay,
    /// then announces the failure and moves to the next entry (T5c2b).
    fn open(&self, url: &str) -> Result<Box<dyn StationSource>, StreamError>;
}

/// Where the producer is: the queue index and the delivered count at which the
/// current track began. Only the producer writes it, only the UI reads it.
#[derive(Clone, Debug, Default)]
struct Progress {
    start: usize,
    duration: Option<Duration>,
    /// Whether this is the stream's first turn — the track a seek opened
    /// partway in. A repeat can bring the same index back, so the index alone
    /// cannot say it.
    first: bool,
    /// The title a live station is announcing, or `None` for a file and
    /// before a station's first `StreamTitle` (S4).
    title: Option<String>,
}

/// Which side of a crossfade a stream carries (issue #65): the handover's
/// new source rises, its old source falls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FadeKind {
    /// The new stream: from exact silence to unity across the window.
    In,
    /// The old stream: from unity to exact silence across the window.
    Out,
}

/// The cross-thread arm of a crossfade (issue #65): the interface arms a
/// fade on a player, and the device callback takes the request on its next
/// turn, running the window on its own clock. The arm never touches the
/// callback's samples directly and the callback never waits: the request is
/// a one-shot mailbox, written once and consumed once.
#[derive(Default)]
struct CrossfadeCell {
    /// Nothing armed is [`CrossfadeCell::NONE`]; a [`FadeKind`] otherwise.
    request: AtomicU8,
    /// The window's length in interleaved samples, derived from the
    /// stream's rate when the fade was armed.
    length: AtomicUsize,
    /// Whether a fade-out has fully fallen — exact silence reached, so the
    /// caller may stop the player instead of leaving a fader stuck
    /// half-way.
    out_done: AtomicBool,
}

impl CrossfadeCell {
    const NONE: u8 = 0;

    fn arm(&self, kind: FadeKind, length: usize) {
        self.length.store(length, Ordering::Release);
        self.request.store(
            match kind {
                FadeKind::In => 1,
                FadeKind::Out => 2,
            },
            Ordering::Release,
        );
    }

    /// Takes the armed request, if any, leaving the cell disarmed: one arm,
    /// one consumer.
    fn take(&self) -> Option<(FadeKind, usize)> {
        let request = self.request.swap(Self::NONE, Ordering::Acquire);
        let kind = match request {
            1 => FadeKind::In,
            2 => FadeKind::Out,
            _ => return None,
        };
        Some((kind, self.length.load(Ordering::Acquire)))
    }

    fn finish_out(&self) {
        self.out_done.store(true, Ordering::Release);
    }

    fn out_done(&self) -> bool {
        self.out_done.load(Ordering::Acquire)
    }
}

/// The callback's own side of an armed fade (issue #65): the window runs on
/// the callback's clock, one sample at a time, advancing only while samples
/// actually play — a paused stream holds its fade where it was.
struct FadeEnvelope {
    kind: FadeKind,
    length: usize,
    elapsed: usize,
}

impl FadeEnvelope {
    /// The factor for one sample, and the step forward. Both sides come
    /// from [`crossfade_pair`], so the two streams of a handover — armed on
    /// the same window — sum to unity across it.
    fn next(&mut self) -> f32 {
        let (into, out_of) = crossfade_pair(self.elapsed, self.length);
        self.elapsed += 1;
        match self.kind {
            FadeKind::In => into,
            FadeKind::Out => out_of,
        }
    }

    /// Whether the window has fully run: a fade-in leaves the samples
    /// untouched from here on; a fade-out has reached exact silence, and
    /// [`CrossfadeCell::finish_out`] follows, so the interface stops the
    /// player instead of leaving a fader stuck half-way.
    fn finished(&self) -> bool {
        self.elapsed > self.length
    }
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
    /// The crossfade arm (issue #65): the interface arms a fade on the
    /// player, the callback takes it on its next turn and runs the window on
    /// its own clock. A mailbox, never shared sample state.
    fade: Arc<CrossfadeCell>,
    /// Whether the device stream has started sounding: set when the
    /// producer's preroll rule starts it, read by the handover so the old
    /// stream's fade-out is armed only when there is something to overlap
    /// with (issue #65).
    started: Arc<AtomicBool>,
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
            /* mono = */ false,
            /* bit_perfect = */ false,
            /* device = */ None,
            /* resolver = */ None,
            // Every interface start names its own [`Screen::Interface`] at
            // [`Player::play_gapless_at_full`] — the full call is what the
            // session uses. This wrapper is the headless convenience path,
            // so its default is the no-screen one.
            Screen::Headless,
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
    /// `mono` (issue #8) starts the session with the mono downmix in force:
    /// both channels carry the average of the mix, applied after `ReplayGain`
    /// and before the ring; [`Player::set_mono`] changes it while playing.
    ///
    /// `device` (issue #44, S3) names the output device to open: `None` is
    /// the system's default, a name is looked up among the machine's
    /// outputs. A name that is no longer present falls back to the default
    /// device and says so once through [`Player::take_notice`] — never a
    /// refusal to start. Under bit-perfect (S5) the chosen device is the one
    /// reopened at each track's own rate, and the fallback to converting is
    /// announced exactly as at launch.
    /// `resolver` (T12c) turns queue entries into files just before the
    /// producer opens them: `None` means every entry already is a file, which
    /// is today's behaviour. A remote entry's fetch failure past the first
    /// track is a skip, not a death; the first track failing here is the
    /// selection-time error the caller reports.
    /// `screen` says who owns the screen this player would report across:
    /// [`Screen::Interface`] when the interface is drawing, so a device
    /// error reaches the status line instead of stderr — a stderr write
    /// from the device callback splices into the frame being painted;
    /// [`Screen::Headless`] for `--play` and every other run behind no
    /// interface, where stderr stays the device error's channel, exactly as
    /// it printed before the interface existed to own a frame.
    ///
    /// # Errors
    ///
    /// The same errors as [`Player::play_gapless_at_with`], including the
    /// first entry's resolution failure.
    #[expect(
        clippy::too_many_arguments,
        reason = "the call is one flat list of what a session starts with —\n    the queue, its position, gain, repeat, mono, bit-perfect and the\n    resolver — and every caller names every part it wants; bundling them\n    would hide which knob a call site turns"
    )]
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
        mono: bool,
        bit_perfect: bool,
        device: Option<&str>,
        resolver: Option<Arc<dyn PathResolver>>,
        screen: Screen,
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

        // The first entry is decided here, at selection time, so a bad first
        // entry fails before a device is opened, and the format is known for
        // the device to be built on (T12c). A file resolves through the
        // resolver and opens; a station opens through the station opener —
        // never the resolver, never a path — and the opened stream is handed
        // to the producer, so a station is fetched once at most — twice only
        // under the one-retry policy (T5c2b). The
        // producer decides every entry again when it reaches it — a cache hit
        // costs no network. The dispatch, not a comment, is what keeps the
        // two seams apart.
        let (track_spec, preopened) = match &first {
            Entry::File(path) => {
                let first_path = match resolver.as_ref() {
                    Some(resolver) => resolver.resolve(path)?,
                    None => path.clone(),
                };
                (TrackSource::open(&first_path)?.spec(), None)
            }
            Entry::Station { url, .. } => {
                let source = open_station_retrying(&LiveOpener, url, Retry::ONCE)
                    .map_err(|error| AudioError::Unsupported(error.to_string()))?;
                (source.spec(), Some((url.clone(), source)))
            }
        };
        let offset_frames = duration_to_frames(offset, track_spec.sample_rate);

        let requested_bit_perfect = bit_perfect;
        // The notices live from the first open on (issue #44, S3): a device
        // that was configured and has vanished is announced here, before a
        // single sample exists. Device errors route by `screen`: to stderr
        // when nobody owns one, into these same notices when the interface
        // does — its tick takes them onto the status line, where a stderr
        // write would splice into the frame it is painting.
        let notices = Arc::new(Mutex::new(VecDeque::new()));
        let errors = match screen {
            Screen::Interface => DeviceErrors::Status(Arc::clone(&notices)),
            Screen::Headless => DeviceErrors::Stderr,
        };
        let (device_handle, config, spec, opened_bit_perfect, device_name) =
            open_output(device, track_spec, bit_perfect, &notices)?;
        // Bit-perfect survives only when the device took the track's rate; a
        // fallback is the converted path, announced once (T17).
        let bit_perfect = bit_perfect && opened_bit_perfect;
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
        // The fetching cell (issue #60): the producer stores the entry it is
        // resolving into, the interface reads it on its tick.
        let fetching = Arc::new(AtomicU64::new(0));
        let progress = Arc::new(Mutex::new(Progress {
            start: 0,
            duration: None,
            first: true,
            title: None,
        }));
        let failures = Arc::new(Mutex::new(VecDeque::new()));
        let repeat = Arc::new(RepeatCell::new(repeat));
        let mono = Arc::new(MonoCell::new(mono));
        let wires = CallbackWires {
            playing: Arc::new(AtomicBool::new(true)),
            producing: Arc::new(AtomicBool::new(true)),
            played: Arc::new(AtomicUsize::new(0)),
            underruns: Arc::new(AtomicUsize::new(0)),
            volume: Arc::new(AtomicU8::new(100)),
            fade: Arc::new(CrossfadeCell::default()),
            started: Arc::new(AtomicBool::new(false)),
        };

        // The stream is built here, before a single sample exists, but not
        // started: the producer starts it once the ring holds the preroll, so
        // the first callback never finds the ring empty. The same rule —
        // build paused, start after the preroll — governs every bit-perfect
        // reopen (T17), which the producer performs itself.
        let (tap, tap_reader) = RingBuffer::<f32>::new(
            usize::try_from(spec.sample_rate).unwrap_or(48_000) / TAP_DIVISOR,
        );
        let stream = build_stream(
            &device_handle,
            config,
            consumer,
            tap,
            &wires,
            errors.clone(),
        )?;
        let cell = Arc::new(StreamCell {
            stream: Mutex::new(Some(stream)),
            spec: Mutex::new(spec),
            tap: Mutex::new(tap_reader),
        });

        let producer = {
            let delivered = Arc::clone(&delivered);
            let stop = Arc::clone(&stop);
            let current = Arc::clone(&current);
            let fetching = Arc::clone(&fetching);
            let progress = Arc::clone(&progress);
            let failures = Arc::clone(&failures);
            let repeat = Arc::clone(&repeat);
            let mono = Arc::clone(&mono);
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
                        resolver,
                        opener: None,
                        retry: Retry::ONCE,
                        preopen: preopened,
                    };
                    let shared = Shared {
                        stop: &stop,
                        delivered: &delivered,
                        current: &current,
                        progress: &progress,
                        repeat: &repeat,
                        mono: &mono,
                        fetching: &fetching,
                    };
                    let mut out = StreamOut {
                        device: device_handle,
                        cell,
                        wires,
                        ring,
                        delivered: Arc::clone(&delivered),
                        stop: Arc::clone(&stop),
                        failures,
                        notices,
                        errors,
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
            fetching,
            progress,
            failures,
            start_offset_samples: usize::try_from(clock_offset_samples).unwrap_or(usize::MAX),
            producer: Some(producer),
            cell,
            notices,
            device_name,
            mono,
            bit_perfect,
            repeat,
        })
    }

    /// Changes what follows the current track, from its end on: the producer
    /// reads the mode at the boundary, so the music does not stop for it.
    pub fn set_repeat(&self, mode: Repeat) {
        self.repeat.set(mode);
    }

    /// Folds the mix down to mono (issue #8, S2) — or back out of it — from
    /// the next block on: the producer reads the shared cell per block, so
    /// the stream never restarts for it (S4).
    pub fn set_mono(&self, mono: bool) {
        self.mono.set(mono);
    }

    /// Whether the mix is folded down to mono.
    #[must_use]
    pub fn mono(&self) -> bool {
        self.mono.get()
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

    /// The name of the device the stream is playing on (issue #44, S1): the
    /// one the device picker marks as in use while this player runs.
    #[must_use]
    pub fn device_name(&self) -> &str {
        &self.device_name
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
    /// switched to the converted path and says so once — and, when the
    /// interface owns the screen, the device errors the callback reported,
    /// in [`device_error_notice`]'s words. Taking — not reading — is the
    /// point, the same rule as [`Player::take_failure`], and the take is
    /// [`take_oldest`]'s: an empty queue answers nothing, a message is
    /// answered once and then gone.
    #[must_use]
    pub fn take_notice(&self) -> Option<String> {
        take_oldest(&self.notices)
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
            Ok(progress) => progress.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        progress.duration
    }

    /// The title the live station is announcing, when one plays and the
    /// stream has published one (S4). `None` for a file, and between a
    /// station's titles.
    #[must_use]
    pub fn current_title(&self) -> Option<String> {
        let progress = match self.progress.lock() {
            Ok(progress) => progress.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        progress.title
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

    /// The index, within the list passed to [`Player::play_gapless`], of the
    /// track the producer is fetching right now, when one is: the entry it is
    /// resolving into a file — for a remote miss, the download — between the
    /// moment the queue reaches it and the moment it opens or fails (issue
    /// #60). `None` while no fetch is going on.
    #[must_use]
    pub fn fetching_index(&self) -> Option<usize> {
        let index = self.fetching.load(Ordering::Acquire);
        usize::try_from(index)
            .ok()
            .and_then(|index| index.checked_sub(1))
    }

    /// Silences the device without stopping the stream.
    pub fn set_playing(&self, playing: bool) {
        self.wires.playing.store(playing, Ordering::Relaxed);
    }

    /// Arms this stream's fade-in as the handover's new source (issue #65):
    /// the window runs over the first [`CROSSFADE_WINDOW`] of audio the
    /// stream actually plays, rising from exact silence to unity. A
    /// bit-perfect player refuses — a fade multiplies samples, which is
    /// exactly what bit-perfect exists to prevent — and the interface keeps
    /// it on the plain path.
    pub fn begin_crossfade_in(&self) {
        if self.bit_perfect {
            return;
        }
        self.wires
            .fade
            .arm(FadeKind::In, crossfade_samples(self.cell.spec()));
    }

    /// Arms this stream's fade-out as the handover's old source (issue #65):
    /// the window runs from the moment it is armed — which the interface
    /// does only once the new stream has started sounding — falling to exact
    /// silence across [`CROSSFADE_WINDOW`], then reporting through
    /// [`Player::crossfade_out_finished`]. A bit-perfect player refuses, the
    /// same rule [`Player::begin_crossfade_in`] follows.
    pub fn begin_crossfade_out(&self) {
        if self.bit_perfect {
            return;
        }
        self.wires
            .fade
            .arm(FadeKind::Out, crossfade_samples(self.cell.spec()));
    }

    /// Whether the armed fade-out has fully fallen: exact silence reached,
    /// never a fader stuck half-way (issue #65). The caller stops the player
    /// when this answers yes; before the fade is armed, and for a
    /// bit-perfect player whose fade is refused, this is always no.
    #[must_use]
    pub fn crossfade_out_finished(&self) -> bool {
        self.wires.fade.out_done()
    }

    /// Whether the device stream has started sounding: the preroll rule
    /// starts it once the ring holds its head start. The crossfade handover
    /// arms the old stream's fade-out only then — the overlap must be two
    /// sources sounding, never two fades of silence (issue #65).
    #[must_use]
    pub fn stream_started(&self) -> bool {
        self.wires.started.load(Ordering::Acquire)
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
            Ok(progress) => progress.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
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
    /// Turns queue entries into files just before the producer opens them
    /// (T12c); `None` means every file entry already is a local file.
    resolver: Option<Arc<dyn PathResolver>>,
    /// Opens station entries just before the producer reaches them; `None`
    /// means the production opener — one GET with the default per-operation
    /// timeouts. Never asked about a file entry.
    opener: Option<Arc<dyn StationOpener>>,
    /// The station retry policy (T5c2b): the delay travels from the caller.
    retry: Retry,
    /// The first station, opened at selection time and handed over so it is
    /// fetched once. The producer uses it only while the entry it reaches
    /// still carries the same URL.
    preopen: Option<(String, Box<dyn StationSource>)>,
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

    /// The progress a turn published: the duration when the decoder knows
    /// it, and the title a live station is announcing. The device path
    /// writes the same record where the player reads it; this is the
    /// headless mirror a drain observes.
    fn progress(&mut self, _duration: Option<Duration>, _title: Option<&str>) {}
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
    errors: DeviceErrors,
) -> Result<cpal::Stream, AudioError> {
    let channels = usize::from(config.channels);
    let CallbackWires {
        playing,
        producing,
        played,
        underruns,
        volume,
        fade,
        ..
    } = wires.clone();
    // The gain lives in the callback and is only ever moved by the ramp: no
    // jump at start-up, on pause, on resume or on volume.
    let mut gain = GainRamp::new(0.0);
    // The crossfade's window is the callback's own (issue #65): taken from
    // the arm cell once, advanced one sample at a time, and — for a fade-out
    // — followed by exact silence and the finished flag, never a fader stuck
    // half-way.
    let mut window: Option<FadeEnvelope> = None;
    let mut faded_out = false;

    device
        .build_output_stream(
            config,
            move |out: &mut [f32], _info: &cpal::OutputCallbackInfo| {
                // An armed crossfade is taken once, on the callback's own
                // thread (issue #65): from here the window runs on this
                // stream's clock, over the samples it actually plays.
                if let Some((kind, length)) = fade.take() {
                    window = Some(FadeEnvelope {
                        kind,
                        length,
                        elapsed: 0,
                    });
                }
                // A fade-out that has fully fallen is this stream's end:
                // exact silence until the interface stops the player (issue
                // #65) — a dying stream makes no sound and counts no clock.
                if faded_out {
                    out.fill(0.0);
                    return;
                }
                let target = if playing.load(Ordering::Relaxed) {
                    volume_amplitude(volume.load(Ordering::Relaxed))
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
                // The handover's fade rides on top of the volume ramp (issue
                // #65): the new stream rises from exact silence while the old
                // one falls to it, across one window on each side.
                if let Some(envelope) = window.as_mut() {
                    for slot in out.iter_mut() {
                        *slot *= envelope.next();
                    }
                    if envelope.finished() {
                        if envelope.kind == FadeKind::Out {
                            faded_out = true;
                            fade.finish_out();
                        }
                        window = None;
                    }
                }
                played.fetch_add(copied, Ordering::Release);
            },
            move |error| errors.report_device(&error.to_string()),
            None,
        )
        .map_err(|error| AudioError::Device(error.to_string()))
}

/// The crossfade window (issue #65): 200 ms, the handover the station switch
/// runs on. Both sides of the handover run the same window, so the old
/// source is still sounding while the new one rises — the device is never
/// silent across the switch.
pub const CROSSFADE_WINDOW: Duration = Duration::from_millis(200);

/// The crossfade window's length for a stream at `spec`: [`CROSSFADE_WINDOW`]
/// of interleaved samples, derived from the stream's own rate — never a
/// hardcoded sample count, so a 44.1 kHz stereo stream and a 48 kHz mono one
/// fade over equally long walls of time and different counts of samples.
#[must_use]
pub fn crossfade_samples(spec: TrackSpec) -> usize {
    let millis = usize::try_from(CROSSFADE_WINDOW.as_millis()).unwrap_or(0);
    spec.interleaved_rate().saturating_mul(millis) / 1000
}

/// The crossfade pair `position` samples into a `length`-sample window (issue
/// #65): the new source's fade-in factor and the old source's fade-out
/// factor, one linear pair that sums to unity at every sample of the window.
/// The ends are exact — `position` 0 opens the handover on exact silence from
/// the new source and full level from the old one, and `position` `length`
/// completes it on unity and exact silence — and past the end the old source
/// stays silent, never rising back.
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    reason = "a fade window holds thousands of samples, exact in f32"
)]
pub fn crossfade_pair(position: usize, length: usize) -> (f32, f32) {
    let t = position.min(length) as f32 / length.max(1) as f32;
    (t, 1.0 - t)
}

/// Turns the volume percentage into the amplitude the samples are multiplied
/// by: the cubic perceptual curve `(percent / 100)³` (issue #39). Hearing is
/// roughly logarithmic in amplitude, so the old linear knob squeezed
/// everything audible into its last few points — 100→90 was about a decibel,
/// 10→0 was twenty. The curve it must match, so the next reader does not
/// re-derive it:
///
/// | percent | amplitude | dB      |
/// |--------:|----------:|---------|
/// |     100 |     1.000 |       0 |
/// |      75 |     0.422 |    −7.5 |
/// |      50 |     0.125 |     −18 |
/// |      25 |     0.016 |     −36 |
/// |      10 |     0.001 |     −60 |
/// |       0 |     0     | silence |
///
/// The stored and shown volume stays 0–100; only this mapping is curved.
/// Bit-perfect never scales samples: its volume is pinned to 100, where the
/// amplitude is exactly 1.0.
#[must_use]
pub fn volume_amplitude(percent: u8) -> f32 {
    let percent = f32::from(percent.min(100)) / 100.0;
    percent * percent * percent
}

/// Folds every whole frame of `samples` into one value that every channel of
/// the frame then carries: the mean of the channels — `(left + right) / 2` on
/// a stereo frame, never their sum, so downmixing cannot grow the peak (issue
/// #8, S1). The device keeps its own layout: this is mono content in the
/// stream it already had, not a mono device. A trailing partial frame is not
/// a frame and is left exactly as it came, the same rule the spectrum tap
/// keeps.
#[expect(
    clippy::cast_precision_loss,
    reason = "a channel count is a handful, exact in f32"
)]
pub fn mono_mix(samples: &mut [f32], channels: usize) {
    if channels <= 1 {
        return;
    }
    let scale = 1.0 / channels as f32;
    for frame in samples.chunks_exact_mut(channels) {
        let mix = frame.iter().sum::<f32>() * scale;
        frame.fill(mix);
    }
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
/// stop flag, the delivered counter, the index and progress it publishes, the
/// repeat mode it reads at every track boundary and the mono cell it reads at
/// every block (issue #8).
struct Shared<'a> {
    stop: &'a AtomicBool,
    delivered: &'a AtomicUsize,
    current: &'a AtomicUsize,
    progress: &'a Mutex<Progress>,
    repeat: &'a RepeatCell,
    mono: &'a MonoCell,
    /// Which queue entry the sequence is fetching right now, stored as the
    /// index plus one — zero means none (issue #60). The sequence sets it
    /// around a file entry's resolution, the step a remote miss spends
    /// downloading, and clears it when the entry opens or fails. The
    /// interface reads it on its tick, the way it reads the volume and the
    /// mono cell; the audio callback never touches it.
    fetching: &'a AtomicU64,
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
    /// Where this player's device errors go ([`Screen`], decided at
    /// construction): stderr when nobody owns a screen, the notices above
    /// when the interface does. The producer reports through it too — the
    /// `stream.play()` of a bit-perfect reopen is producer-side work.
    errors: DeviceErrors,
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
            self.errors.report_producer(&error.to_string());
        }
        self.started = true;
        // Published for the crossfade handover (issue #65): the old stream's
        // fade-out is armed only once the new stream has begun sounding.
        self.wires.started.store(true, Ordering::Release);
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
        let stream = build_stream(
            &self.device,
            config,
            consumer,
            tap,
            &self.wires,
            self.errors.clone(),
        )?;
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

/// Pushes a one-line user notice, once.
fn push_notice(notices: &Mutex<VecDeque<String>>, message: String) {
    match notices.lock() {
        Ok(mut queue) => queue.push_back(message),
        Err(poisoned) => poisoned.into_inner().push_back(message),
    }
}

/// Takes the oldest notice from `notices`, if any: taking — not reading — is
/// the point, the same rule as a failure. An empty queue answers `None` and
/// stays empty — taking never invents a message — and a message that was
/// answered is gone, so the same fact is never said twice.
#[must_use]
pub fn take_oldest(notices: &Mutex<VecDeque<String>>) -> Option<String> {
    match notices.lock() {
        Ok(mut queue) => queue.pop_front(),
        Err(poisoned) => poisoned.into_inner().pop_front(),
    }
}

/// A device error in the app's own words (issue: the interface owns the
/// screen a device error used to be spliced into): the same family of fact
/// as "audio device \"X\" is not present — using the default output", set
/// down by the interface on its status line instead of stderr, whose
/// writes land in whatever row the frame it draws happens to be painting.
#[must_use]
pub fn device_error_notice(error: &str) -> String {
    format!("audio device error: {error}")
}

/// Who owns the screen a device error would have to cross: the plain split
/// the player is built with, a constructor argument — never a global. The
/// interface draws its own frame (in the alternate buffer), so a stderr
/// write from inside the player splices into whatever row is being painted;
/// headless runs (`--play`, the drains, the scans) own no screen, so stderr
/// stays theirs, exactly as it always printed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    /// An interface owns the screen: device errors join the player's own
    /// notices, said once on the status line — never stderr.
    Interface,
    /// No screen is owned: device errors print to stderr as always.
    Headless,
}

/// Where a player's device errors go, decided once at construction from the
/// [`Screen`] its caller named.
#[derive(Clone)]
enum DeviceErrors {
    /// The interface's status line: the sentence joins the player's notice
    /// queue, which the interface drains on its tick.
    Status(Arc<Mutex<VecDeque<String>>>),
    /// Headless: stderr, the way device errors have always printed.
    Stderr,
}

impl DeviceErrors {
    /// Reports a device error from the device's callback thread. It must
    /// never wait there: the callback runs on the device's thread and owes
    /// it samples, so the queue is taken with `try_lock` and a message that
    /// arrives while the queue is held elsewhere is dropped — rare, on an
    /// exceptional path, and a device condition that persists will say it
    /// again. (`try_lock` and drop on contention, chosen over an atomic swap
    /// — which cannot carry a sentence — and over a second non-blocking ring,
    /// which would be a whole extra queue to feed for an error that may
    /// never happen.)
    fn report_device(&self, error: &str) {
        match self {
            Self::Stderr => eprintln!("velo: audio device error: {error}"),
            // `try_lock`: never a block on the device's thread.
            Self::Status(notices) => {
                if let Ok(mut notices) = notices.try_lock() {
                    notices.push_back(device_error_notice(error));
                }
            }
        }
    }

    /// Reports a device error from the producer thread, where a lock is
    /// honest — the producer never owes the device samples.
    fn report_producer(&self, error: &str) {
        match self {
            Self::Stderr => eprintln!("velo: audio device error: {error}"),
            Self::Status(notices) => push_notice(notices, device_error_notice(error)),
        }
    }
}

/// The headless drains' sink: closures into a collector, with a reopen rule
/// supplied by each drain — bit-perfect ones follow the track's rate, the
/// others are never asked.
struct FnSink<'a> {
    block: &'a mut dyn FnMut(&[f32]),
    failure: &'a mut dyn FnMut(TrackFailure),
    reopen: &'a mut dyn FnMut(TrackSpec) -> Result<TrackSpec, AudioError>,
    progress: &'a mut dyn FnMut(Option<Duration>, Option<&str>),
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

    fn progress(&mut self, duration: Option<Duration>, title: Option<&str>) {
        (self.progress)(duration, title);
    }
}

/// Copies one queue entry out with the queue's length, so the lock is held
/// only for the copy.
fn queued_entry(queue: &Mutex<Vec<Entry>>, index: usize) -> (Option<Entry>, usize) {
    let read = |queue: &Vec<Entry>| (queue.get(index).cloned(), queue.len());
    match queue.lock() {
        Ok(queue) => read(&queue),
        Err(poisoned) => read(&poisoned.into_inner()),
    }
}

/// A track failure record for the interface: the entry as the listener
/// sees it, and why. The label comes from the queue entry — so a failure
/// that happens *after* resolution, while the cache file is open, still
/// names the entry the user queued, never the hash-named cache file it
/// opened (T12c, true here for every failure, not only resolution's).
fn track_failure(label: &str, message: String) -> TrackFailure {
    TrackFailure {
        label: label.to_owned(),
        message,
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
        resolver,
        opener,
        retry,
        mut preopen,
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
        let (Some(entry), _) = queued_entry(&queue, turn.index) else {
            return Ok(());
        };
        // The entry is decided when the producer reaches it (T12c), by what
        // it is: a file resolves through the resolver and opens from a path;
        // a station opens through the station opener, never the resolver and
        // never a path — the two seams never cross, the match's shape is what
        // makes that so. A failure past the first turn is announced and
        // skipped (T19); the first turn's failure is the sequence error the
        // caller reports at selection time.
        let label = entry.label().into_owned();
        let outcome = match &entry {
            Entry::File(path) => {
                // The resolution is the wait (issue #60): the cell says
                // which entry is being fetched, from the moment the queue
                // reaches it until it opens or fails. A pass-through and a
                // cache hit clear it again within the same step; a miss
                // keeps it up for the download's whole length.
                shared.fetching.store(
                    u64::try_from(turn.index).unwrap_or(u64::MAX) + 1,
                    Ordering::Release,
                );
                let resolved = resolve_entry(resolver.as_ref(), path);
                shared.fetching.store(0, Ordering::Release);
                match resolved {
                    Ok(resolved) => {
                        play_turn(&label, &resolved, &turn, &mut expected, shared, sink)?
                    }
                    Err(error) => {
                        sink.failure(track_failure(&label, error.to_string()));
                        Turn::Failed
                    }
                }
            }
            Entry::Station { url, .. } => {
                match station_source(&mut preopen, opener.as_ref(), url, retry) {
                    Ok(source) => {
                        // The reopen is pure — it opens and nothing else. The
                        // policy's delay is spent by the turn itself, on this
                        // (producer) thread, never in the audio callback.
                        let mut reopen = || station_opener(opener.as_ref()).open(url);
                        play_station_turn(
                            &label,
                            source,
                            &turn,
                            &mut expected,
                            shared,
                            sink,
                            retry,
                            &mut reopen,
                        )
                    }
                    Err(error) if turn.first => {
                        return Err(AudioError::Unsupported(error.to_string()));
                    }
                    Err(error) => {
                        sink.failure(track_failure(&label, error.to_string()));
                        Turn::Failed
                    }
                }
            }
        };
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

/// Resolves one *file* queue entry into the file the producer opens (T12c).
/// Without a resolver every file entry already is a local file, which is
/// today's behaviour. A station never reaches here: the dispatch on the
/// entry, not a promise this comment makes, is what routes it to the station
/// opener instead.
fn resolve_entry(
    resolver: Option<&Arc<dyn PathResolver>>,
    path: &Path,
) -> Result<PathBuf, AudioError> {
    match resolver {
        Some(resolver) => resolver.resolve(path),
        None => Ok(path.to_path_buf()),
    }
}

/// The opener a sequence uses: the injected one, or the production opener —
/// one GET with the default per-operation timeouts.
fn station_opener(opener: Option<&Arc<dyn StationOpener>>) -> &dyn StationOpener {
    static DEFAULT: LiveOpener = LiveOpener;
    match opener {
        Some(opener) => opener.as_ref(),
        None => &DEFAULT,
    }
}

/// The station source for `url`: the one opened at selection time, while the
/// entry the producer reached still is that URL, otherwise a fresh open. A
/// preopen that no longer matches is dropped — the queue may have moved
/// under it. A fresh open carries the retry policy (T5c2b); a preopen was
/// already tried under it at selection time.
fn station_source(
    preopen: &mut Option<(String, Box<dyn StationSource>)>,
    opener: Option<&Arc<dyn StationOpener>>,
    url: &str,
    retry: Retry,
) -> Result<Box<dyn StationSource>, StreamError> {
    match preopen.take() {
        Some((preopen_url, source)) if preopen_url == url => Ok(source),
        _ => open_station_retrying(station_opener(opener), url, retry),
    }
}

/// Opens a station under the retry policy (T5c2b, S2): each failed attempt
/// after the first waits `retry.delay` on the caller's thread — the delay
/// sits between attempts, never before the first — and the last failure is
/// the caller's to announce. With [`Retry::NONE`] this is one attempt and
/// no wait. No stall timer sits beside this: a stall is already an error
/// through the stream source's per-read bound (T5b1), and a second
/// mechanism would only lie about which one fired.
fn open_station_retrying(
    opener: &dyn StationOpener,
    url: &str,
    retry: Retry,
) -> Result<Box<dyn StationSource>, StreamError> {
    let mut last: Option<StreamError> = None;
    for attempt in 0..retry.attempts {
        if attempt > 0 {
            thread::sleep(retry.delay);
        }
        match opener.open(url) {
            Ok(source) => return Ok(source),
            Err(error) => last = Some(error),
        }
    }
    Err(last.expect("attempts is at least 1, so the opener answered"))
}

/// Plays one track into the sink. The track the player was built from
/// failing to open is the sequence error the interface already reports;
/// every other failure is announced and costs this one track, so one bad file
/// does not take the rest of the album with it (T19). Failures are named for
/// the queue entry — including the failures that happen after resolution,
/// while the cache file is open — so a skipped remote track reports the
/// remote row and never the hash-named cached file (T12c); a station is
/// named for itself, the URL never shown (T5c2a). The samples, however,
/// come from the resolved file — the cache — whose tags are the originals'.
fn play_turn(
    label: &str,
    path: &Path,
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
            sink.failure(track_failure(label, error.to_string()));
            return Ok(Turn::Failed);
        }
    };

    // Announce which track is being produced once it has opened, so a reader
    // only ever sees a track that is playing or about to — never one that is
    // about to be skipped.
    shared.current.store(turn.index, Ordering::Release);

    // One write publishes every part, so a reader can never see a track
    // without the point it began. A file has no title: whatever a previous
    // station announced is cleared here.
    let record = Progress {
        start: shared.delivered.load(Ordering::Acquire),
        duration: source.duration(),
        first: turn.first,
        title: None,
    };
    publish_progress(shared, sink, &record);

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
            sink.failure(track_failure(label, error.to_string()));
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
                sink.failure(track_failure(label, error.to_string()));
                return Ok(Turn::Failed);
            }
        };
        let mut resampled = match converter.convert(&map.apply(&block)) {
            Ok(resampled) => resampled,
            Err(error) => {
                sink.failure(track_failure(label, error.to_string()));
                return Ok(Turn::Failed);
            }
        };
        // The gain's one multiplication, before the ring: the callback keeps
        // copying and ramping (T11).
        gain.apply(&mut resampled);
        // The mono downmix (issue #8, S1): after `ReplayGain`, before the
        // ring. The cell is read per block, so `M` lands mid-stream without
        // a restart (S4).
        if shared.mono.get() {
            mono_mix(&mut resampled, usize::from(expected.channels));
        }
        heard |= !resampled.is_empty();
        sink.push_block(&resampled);
    }

    // The resampler holds the track's last frames back until it sees what
    // follows them; without this the tail is heard as a cut-off ending.
    let mut flushed = match converter.flush() {
        Ok(flushed) => flushed,
        Err(error) => {
            sink.failure(track_failure(label, error.to_string()));
            return Ok(Turn::Failed);
        }
    };
    gain.apply(&mut flushed);
    if shared.mono.get() {
        mono_mix(&mut flushed, usize::from(expected.channels));
    }
    heard |= !flushed.is_empty();
    if !flushed.is_empty() {
        sink.push_block(&flushed);
    }
    Ok(if heard { Turn::Played } else { Turn::Silent })
}

/// Plays one live station into the sink. A station is opened through the
/// station opener — never the resolver, never a path — and plays as its
/// bytes arrive. It has no duration, is never seeked (S3: the offset a seek
/// passes is ignored), and skips `ReplayGain` and bit-perfect reopening
/// (S3); its samples convert into the sequence's format exactly as a file's
/// do, so it never pairs with a neighbour as a gapless join (S3). A failure
/// — at open or while reading — gets the caller's retry policy: the
/// producer spends the budget between attempts on its own thread and
/// restores it only when audio actually reaches the ring, so a cut stream
/// cannot loop. A spent budget announces the failure for the station's own
/// name and costs this one entry: the queue moves on (T5c2b).
#[expect(
    clippy::too_many_arguments,
    reason = "the turn is one story — open state, the sequence's shared
    handles, the sink, the policy and the pure reopen — and every one is
    distinct data, not a bundle to hide"
)]
fn play_station_turn(
    label: &str,
    mut source: Box<dyn StationSource>,
    turn: &TurnJob,
    expected: &mut TrackSpec,
    shared: &Shared<'_>,
    sink: &mut dyn SequenceSink,
    retry: Retry,
    reopen: &mut dyn Fn() -> Result<Box<dyn StationSource>, StreamError>,
) -> Turn {
    // Announce which entry is being produced once it has opened, so a
    // reader only ever sees an entry that is playing or about to — never
    // one that is about to be skipped.
    shared.current.store(turn.index, Ordering::Release);

    // A station has no duration and nothing to seek within (S3); the title
    // it announces is the progress the interface reads (S4).
    let spec = source.spec();
    let record = Progress {
        start: shared.delivered.load(Ordering::Acquire),
        duration: source.duration(),
        first: turn.first,
        title: source.title(),
    };
    publish_progress(shared, sink, &record);
    let mut last_title = record.title.clone();

    // A rate difference is converted away and a layout difference is mapped,
    // exactly as for a file; bit-perfect never reopens the stream for a
    // station (S3), so `expected` is left as it is.
    let map = match ChannelMap::new(spec.channels, expected.channels) {
        Ok(map) => map,
        Err(error) => {
            sink.failure(track_failure(label, error.to_string()));
            return Turn::Failed;
        }
    };
    let mapped_spec = TrackSpec {
        sample_rate: spec.sample_rate,
        channels: expected.channels,
    };
    let mut converter = match RateConverter::new(mapped_spec, *expected) {
        Ok(converter) => converter,
        Err(error) => {
            sink.failure(track_failure(label, error.to_string()));
            return Turn::Failed;
        }
    };

    // ReplayGain is skipped for a station (S3): there is no file to read
    // tags from, so the samples go through at unity gain.
    let gain = Gain::identity();
    let mut heard = false;
    // The retry budget belongs to this turn, not to one connection (T5c2b):
    // it is spent by failures and restored only by audio actually emitted,
    // so a stream that accepts and cuts at once burns the same budget as
    // one that played — the turn ends, never loops. The delay is slept here
    // on the producer thread, between attempts, never in the callback.
    let mut failures_without_progress: u32 = 0;
    loop {
        if shared.stop.load(Ordering::Relaxed) {
            return Turn::Stopped;
        }
        let block = match source.read_block() {
            Ok(Some(block)) => block,
            Ok(None) => break,
            Err(error) => {
                if failures_without_progress + 1 >= retry.attempts {
                    sink.failure(track_failure(label, error.to_string()));
                    return Turn::Failed;
                }
                failures_without_progress += 1;
                thread::sleep(retry.delay);
                match reopen() {
                    Ok(fresh) => {
                        source = fresh;
                        continue;
                    }
                    Err(retry_error) => {
                        sink.failure(track_failure(label, retry_error.to_string()));
                        return Turn::Failed;
                    }
                }
            }
        };
        let mut resampled = match converter.convert(&map.apply(&block)) {
            Ok(resampled) => resampled,
            Err(error) => {
                sink.failure(track_failure(label, error.to_string()));
                return Turn::Failed;
            }
        };
        gain.apply(&mut resampled);
        // The same downmix, after the identity gain a station plays at (S1):
        // the toggle is the stream's, whatever the entry is.
        if shared.mono.get() {
            mono_mix(&mut resampled, usize::from(expected.channels));
        }
        let emitted = !resampled.is_empty();
        heard |= emitted;
        if emitted {
            // Real progress: the budget is restored.
            failures_without_progress = 0;
        }
        sink.push_block(&resampled);
        // What the station is announcing now is published when it changes,
        // so a repeated title never repaints (S4). A `None` after a retry
        // means "not announced yet", not a change: the title the listener
        // already saw is held until the reopened station says something new
        // (T5c2b — a retry must not reset what the UI is showing).
        let title = source.title().or_else(|| last_title.clone());
        if title != last_title {
            publish_title(shared, sink, title.as_deref());
            last_title = title;
        }
    }

    // The resampler holds the tail back until it sees what follows, exactly
    // as for a file: without the flush the station's last frames are lost.
    let mut flushed = match converter.flush() {
        Ok(flushed) => flushed,
        Err(error) => {
            sink.failure(track_failure(label, error.to_string()));
            return Turn::Failed;
        }
    };
    gain.apply(&mut flushed);
    if shared.mono.get() {
        mono_mix(&mut flushed, usize::from(expected.channels));
    }
    heard |= !flushed.is_empty();
    if !flushed.is_empty() {
        sink.push_block(&flushed);
    }
    if heard { Turn::Played } else { Turn::Silent }
}

/// Writes one progress record where the player reads it, and mirrors it to
/// the headless sink. One write, so a reader never sees a record half
/// updated.
fn publish_progress(shared: &Shared<'_>, sink: &mut dyn SequenceSink, record: &Progress) {
    match shared.progress.lock() {
        Ok(mut slot) => *slot = record.clone(),
        Err(poisoned) => *poisoned.into_inner() = record.clone(),
    }
    sink.progress(record.duration, record.title.as_deref());
}

/// Updates the published title when it changed, and mirrors it to the
/// headless sink. One write, so a reader never sees a title half updated.
fn publish_title(shared: &Shared<'_>, sink: &mut dyn SequenceSink, title: Option<&str>) {
    let record = {
        let mut slot = match shared.progress.lock() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        if slot.title.as_deref() == title {
            return;
        }
        slot.title = title.map(str::to_owned);
        slot.clone()
    };
    sink.progress(record.duration, record.title.as_deref());
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
        resolver: None,
        opener: None,
        retry: Retry::NONE,
        preopen: None,
    };
    let fetching_cell = AtomicU64::new(0);
    let mut refuse = |_: TrackSpec| -> Result<TrackSpec, AudioError> {
        unreachable!("only bit-perfect sequences reopen the stream")
    };
    drain(
        job,
        expected,
        &RepeatCell::default(),
        stop,
        &fetching_cell,
        &mut FnSink {
            block: on_block,
            failure: on_failure,
            reopen: &mut refuse,
            progress: &mut |_: Option<Duration>, _: Option<&str>| {},
        },
    )
}

/// [`drain_headless_with_gain`] with a resolver (T12c): each queue entry is
/// resolved into a file when the loop reaches it — a remote entry's cache hit
/// opens the cached file, and a failed resolution is announced through
/// `on_failure` and skipped, exactly the way the producer thread does it.
/// This is the headless proof that remote playback never touches the audio
/// path: the drained samples are the cached files', decoded as any local
/// track would be. `fetching` is the cell the loop stores the entry it is
/// fetching into, index plus one, zero when none — the same cell the real
/// player's producer writes and the interface reads (issue #60).
///
/// # Errors
///
/// The same errors as [`drain_headless`].
#[expect(
    clippy::too_many_arguments,
    reason = "the job, the mode, the resolver and the two sinks are the whole\n    contract of the drain; bundling them would only hide what is shared"
)]
pub fn drain_headless_resolving(
    queue: SharedQueue,
    start_track: usize,
    offset_frames: u64,
    expected: TrackSpec,
    gain_mode: ReplayGainMode,
    resolver: Option<Arc<dyn PathResolver>>,
    fetching: &AtomicU64,
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
        resolver,
        opener: None,
        retry: Retry::NONE,
        preopen: None,
    };
    let mut refuse = |_: TrackSpec| -> Result<TrackSpec, AudioError> {
        unreachable!("only bit-perfect sequences reopen the stream")
    };
    drain(
        job,
        expected,
        &RepeatCell::default(),
        stop,
        fetching,
        &mut FnSink {
            block: on_block,
            failure: on_failure,
            reopen: &mut refuse,
            progress: &mut |_: Option<Duration>, _: Option<&str>| {},
        },
    )
}

/// [`drain_headless_resolving`] with the station opener beside the resolver
/// (T5c2a): a station entry opens through `opener` when the loop reaches it
/// and plays as a stream — never through the resolver, never a path — while
/// a file entry resolves exactly as before. Every published progress record
/// — the duration when known, the station's title when announced — goes to
/// `on_progress`, which is how a headless test observes what the player
/// exposes to the interface. `retry` is the station retry policy (T5c2b),
/// passed by the caller — [`Retry::ONCE`] in production, [`Retry::NONE`] or
/// a zero-delay [`Retry::ONCE`] in tests, which is how the retry tests
/// prove the policy without waiting on the wall clock.
///
/// # Errors
///
/// The same errors as [`drain_headless`].
#[expect(
    clippy::too_many_arguments,
    reason = "the job, both seams and the three sinks are the whole contract\n    of the drain; bundling them would only hide what is shared"
)]
pub fn drain_headless_with_opener(
    queue: SharedQueue,
    start_track: usize,
    offset_frames: u64,
    expected: TrackSpec,
    gain_mode: ReplayGainMode,
    resolver: Option<Arc<dyn PathResolver>>,
    opener: Option<Arc<dyn StationOpener>>,
    retry: Retry,
    stop: &AtomicBool,
    on_block: &mut dyn FnMut(&[f32]),
    on_failure: &mut dyn FnMut(TrackFailure),
    on_progress: &mut dyn FnMut(Option<Duration>, Option<&str>),
) -> Result<(), AudioError> {
    let job = Sequence {
        queue,
        start_track,
        offset_frames,
        gain_mode,
        bit_perfect: false,
        resolver,
        opener,
        retry,
        preopen: None,
    };
    let fetching_cell = AtomicU64::new(0);
    let mut refuse = |_: TrackSpec| -> Result<TrackSpec, AudioError> {
        unreachable!("only bit-perfect sequences reopen the stream")
    };
    drain(
        job,
        expected,
        &RepeatCell::default(),
        stop,
        &fetching_cell,
        &mut FnSink {
            block: on_block,
            failure: on_failure,
            reopen: &mut refuse,
            progress: on_progress,
        },
    )
}

/// [`drain_headless_with_gain`] with the mono cell a caller holds (issue #8):
/// the sequence reads it at every block, exactly as the producer thread does,
/// so a test can flip it mid-drain and prove the toggle lands on the sample
/// path without a restart (S4).
///
/// # Errors
///
/// The same errors as [`drain_headless`].
#[expect(
    clippy::too_many_arguments,
    reason = "the job, the mode, the mono cell and the two sinks are the\n    whole contract of the drain; bundling them would only hide what is\n    shared"
)]
pub fn drain_headless_with_mono(
    queue: SharedQueue,
    start_track: usize,
    offset_frames: u64,
    expected: TrackSpec,
    gain_mode: ReplayGainMode,
    mono: &MonoCell,
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
        resolver: None,
        opener: None,
        retry: Retry::NONE,
        preopen: None,
    };
    let fetching_cell = AtomicU64::new(0);
    let mut refuse = |_: TrackSpec| -> Result<TrackSpec, AudioError> {
        unreachable!("only bit-perfect sequences reopen the stream")
    };
    drain_with_mono(
        job,
        expected,
        &RepeatCell::default(),
        mono,
        stop,
        &fetching_cell,
        &mut FnSink {
            block: on_block,
            failure: on_failure,
            reopen: &mut refuse,
            progress: &mut |_: Option<Duration>, _: Option<&str>| {},
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
        resolver: None,
        opener: None,
        retry: Retry::NONE,
        preopen: None,
    };
    let fetching_cell = AtomicU64::new(0);
    let mut refuse = |_: TrackSpec| -> Result<TrackSpec, AudioError> {
        unreachable!("only bit-perfect sequences reopen the stream")
    };
    drain(
        job,
        expected,
        repeat,
        stop,
        &fetching_cell,
        &mut FnSink {
            block: on_block,
            failure: on_failure,
            reopen: &mut refuse,
            progress: &mut |_: Option<Duration>, _: Option<&str>| {},
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
        resolver: None,
        opener: None,
        retry: Retry::NONE,
        preopen: None,
    };
    let channels = expected.channels;
    let fetching_cell = AtomicU64::new(0);
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
        &fetching_cell,
        &mut FnSink {
            block: on_block,
            failure: on_failure,
            reopen: &mut follow,
            progress: &mut |_: Option<Duration>, _: Option<&str>| {},
        },
    )
}

/// The headless drains' one body: the producer's loop with local counters.
/// The mono cell is the drains' own, stereo unless the caller's drain takes
/// one from its caller ([`drain_headless_with_mono`]).
fn drain(
    job: Sequence,
    expected: TrackSpec,
    repeat: &RepeatCell,
    stop: &AtomicBool,
    fetching: &AtomicU64,
    sink: &mut dyn SequenceSink,
) -> Result<(), AudioError> {
    let delivered = AtomicUsize::new(0);
    let current = AtomicUsize::new(job.start_track);
    let mono = MonoCell::default();
    let progress = Mutex::new(Progress {
        start: 0,
        duration: None,
        first: true,
        title: None,
    });
    let shared = Shared {
        stop,
        delivered: &delivered,
        current: &current,
        progress: &progress,
        repeat,
        mono: &mono,
        fetching,
    };
    run_sequence(job, expected, &shared, sink)
}

/// [`drain`] with the mono cell a caller holds (issue #8): the sequence reads
/// it at every block, exactly as the producer thread does, so a test can flip
/// it mid-drain and prove the toggle lands on the sample path without a
/// restart (S4).
fn drain_with_mono(
    job: Sequence,
    expected: TrackSpec,
    repeat: &RepeatCell,
    mono: &MonoCell,
    stop: &AtomicBool,
    fetching: &AtomicU64,
    sink: &mut dyn SequenceSink,
) -> Result<(), AudioError> {
    let delivered = AtomicUsize::new(0);
    let current = AtomicUsize::new(job.start_track);
    let progress = Mutex::new(Progress {
        start: 0,
        duration: None,
        first: true,
        title: None,
    });
    let shared = Shared {
        stop,
        delivered: &delivered,
        current: &current,
        progress: &progress,
        repeat,
        mono,
        fetching,
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

/// The machine's audio outputs (issue #44, S1): the names the device picker
/// offers, in the order cpal lists them, and the default's own name, so the
/// picker can mark the one in use before a player exists.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeviceList {
    /// Every output device's name. Duplicates by name are dropped: two
    /// same-named entries open the same first match, so offering one device
    /// twice would be one choice that is not.
    pub names: Vec<String>,
    /// The default output's name, when the machine has one.
    pub default: Option<String>,
}

/// Lists the default host's audio outputs (issue #44, S1).
#[must_use]
pub fn output_devices() -> DeviceList {
    output_devices_on(&cpal::default_host())
}

/// [`output_devices`] for one host, so the resolution below answers from a
/// list instead of a machine.
fn output_devices_on(host: &cpal::Host) -> DeviceList {
    let mut list = DeviceList {
        default: host
            .default_output_device()
            .map(|device| device_name(&device)),
        ..DeviceList::default()
    };
    if let Ok(devices) = host.output_devices() {
        for device in devices {
            let name = device_name(&device);
            if !list.names.contains(&name) {
                list.names.push(name);
            }
        }
    }
    list
}

/// A device's own name (issue #44, S1): cpal renders it, and that rendering
/// is the name the user knows the device by.
fn device_name(device: &cpal::Device) -> String {
    device.to_string()
}

/// Which device a launch or a switch opens, and what it says once when the
/// requested name is not among the machine's outputs (issue #44, S3): the
/// requested name itself, else the default with a one-line notice — never a
/// refusal to start. The decision is pure over the device list, which is
/// what makes the fallback testable without a machine.
fn open_target<'a>(
    requested: Option<&str>,
    list: &'a DeviceList,
) -> Result<(&'a str, Option<String>), AudioError> {
    match requested {
        // The offered name is returned, not the request's: the answer must
        // borrow from the list, which outlives the request.
        Some(name) if list.names.iter().any(|offered| offered.as_str() == name) => {
            let offered = list
                .names
                .iter()
                .find(|offered| offered.as_str() == name)
                .expect("the guard above checked the name is offered");
            Ok((offered.as_str(), None))
        }
        Some(name) => {
            let default = list.default.as_deref().ok_or_else(|| {
                AudioError::Device(format!(
                    "no audio output device — \"{name}\" is not among the outputs"
                ))
            })?;
            Ok((
                default,
                Some(format!(
                    "audio device \"{name}\" is not present — using the default output"
                )),
            ))
        }
        None => list
            .default
            .as_deref()
            .map(|default| (default, None))
            .ok_or_else(|| AudioError::Device("no audio output device".to_owned())),
    }
}

/// Opens the named output device at `track`'s own format when it accepts it,
/// otherwise at the device default, and the stream config to match. Under
/// bit-perfect (T17), the device is opened at the track's own sample rate —
/// and the boolean says whether that worked; when it did not, the caller
/// falls back to the converted path and announces it. The string is the
/// opened device's own name, what [`Player::device_name`] answers (issue
/// #44, S1). A configured name that is not among the machine's outputs is
/// announced once and the default opens instead (issue #44, S3).
fn open_output(
    requested: Option<&str>,
    track: TrackSpec,
    bit_perfect: bool,
    notices: &Mutex<VecDeque<String>>,
) -> Result<(cpal::Device, cpal::StreamConfig, TrackSpec, bool, String), AudioError> {
    let host = cpal::default_host();
    let list = output_devices_on(&host);
    let (name, fallback) = open_target(requested, &list)?;
    if let Some(notice) = fallback {
        push_notice(notices, notice);
    }
    let device = find_device(&host, name)
        .ok_or_else(|| AudioError::Device(format!("no audio output device named \"{name}\"")))?;
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
    let opened = device_name(&device);
    Ok((device, config, spec, exact, opened))
}

/// The device whose name is `name`, the first match; the list it was chosen
/// from carried no duplicates.
fn find_device(host: &cpal::Host, name: &str) -> Option<cpal::Device> {
    host.output_devices()
        .ok()?
        .find(|device| device_name(device) == name)
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
    use super::{
        DeviceErrors, DeviceList, Retry, device_error_notice, elapsed_samples, open_target,
        take_oldest,
    };
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn two_devices() -> DeviceList {
        DeviceList {
            names: vec!["Speakers".to_owned(), "Headphones".to_owned()],
            default: Some("Speakers".to_owned()),
        }
    }

    #[test]
    fn a_requested_device_that_is_still_there_is_opened_itself() {
        let list = two_devices();
        let (name, notice) = open_target(Some("Headphones"), &list).expect("the request resolves");
        assert_eq!(name, "Headphones");
        assert_eq!(notice, None, "nothing to say about a device that is there");
    }

    #[test]
    fn a_device_that_has_vanished_falls_back_to_the_default_and_says_so_once() {
        let list = two_devices();
        let (name, notice) = open_target(Some("USB DAC"), &list).expect("the launch never refuses");
        assert_eq!(name, "Speakers", "the default is used, never a refusal");
        assert_eq!(
            notice.as_deref(),
            Some("audio device \"USB DAC\" is not present — using the default output"),
            "the fallback says so once"
        );
    }

    #[test]
    fn no_request_opens_the_default_without_a_word() {
        let list = two_devices();
        let (name, notice) = open_target(None, &list).expect("the default resolves");
        assert_eq!(name, "Speakers");
        assert_eq!(notice, None);
    }

    #[test]
    fn a_vanished_device_on_a_machine_without_a_default_is_an_error_that_names_it() {
        let list = DeviceList {
            names: vec![],
            default: None,
        };
        let error = open_target(Some("USB DAC"), &list).expect_err("nothing to fall back to");
        assert!(
            error.to_string().contains("USB DAC"),
            "the error names the device, got: {error}"
        );
        assert_eq!(
            open_target(None, &list)
                .expect_err("no device at all")
                .to_string(),
            "audio device: no audio output device",
            "the error type's own prefix, exactly as the status line prints it"
        );
    }

    #[test]
    fn the_retry_policies_are_exactly_the_two_the_spec_names() {
        assert_eq!(
            Retry::NONE,
            Retry {
                attempts: 1,
                delay: Duration::ZERO
            },
            "no retry: one attempt, no wait"
        );
        assert_eq!(
            Retry::ONCE,
            Retry {
                attempts: 2,
                delay: Duration::from_secs(2)
            },
            "production: one retry after 2 s (S2)"
        );
        assert_eq!(
            Retry::default(),
            Retry::NONE,
            "the default is the fast path, never a hidden sleep"
        );
    }

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

    fn status_slot() -> (Arc<Mutex<VecDeque<String>>>, DeviceErrors) {
        let slot = Arc::new(Mutex::new(VecDeque::new()));
        (Arc::clone(&slot), DeviceErrors::Status(slot))
    }

    /// A device error that arrives with the interface owning the screen
    /// joins the queue the status line drains — the same sentence the
    /// stderr line used to carry, minus the prefix the status line itself
    /// adds.
    #[test]
    fn an_interface_player_s_device_error_lands_on_its_notice_queue() {
        let (slot, errors) = status_slot();
        errors.report_device("Device sample rate changed");
        assert_eq!(
            take_oldest(&slot).as_deref(),
            Some(device_error_notice("Device sample rate changed").as_str()),
            "the sentence reaches the queue the interface takens"
        );
        assert_eq!(take_oldest(&slot), None, "said once");
    }

    /// The callback's own rule: the queue is taken with `try_lock`, and a
    /// message that arrives while the lock is held elsewhere is dropped —
    /// the device thread owes it samples, never a wait. `try_lock` never
    /// blocks, so a queue already held by this very guard proves the call
    /// returns at all.
    #[test]
    fn a_callback_error_a_contended_queue_would_wait_for_is_dropped_instead() {
        let (slot, errors) = status_slot();
        let held = slot.lock().expect("the test lock is held");
        errors.report_device("Device was lost");
        drop(held);
        assert!(
            slot.lock().expect("the queue reads").is_empty(),
            "the contended message was dropped, never waited on"
        );
    }

    /// Headless runs own no screen: their device errors print to stderr, and
    /// nothing may pretend they were said to an interface. The queue — the
    /// interface's own channel — stays empty.
    #[test]
    fn a_headless_player_s_device_error_never_touches_a_notice_queue() {
        let (slot, _unused_interface_only_sink) = status_slot();
        DeviceErrors::Stderr.report_device("Device sample rate changed");
        assert!(
            slot.lock().expect("the queue reads").is_empty(),
            "stderr is the headless error's own channel"
        );
    }
}
