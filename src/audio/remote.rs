//! A remote URL as a seekable media source: HTTP Range requests behind
//! symphonia's `MediaSource` (remote-streaming T1).
//!
//! This revises the recorded decision L36 of `odd/tasks/velo-player.md`
//! ("Playback is cache-on-play … Streaming with HTTP ranges is a later
//! optimisation"): for a radio channel's songs the wait for the whole body
//! is now felt, so the audio path gains a source that reads the URL as its
//! bytes arrive. What each behaviour serves:
//!
//! - **S1, S2** — [`RemoteSource`] opens with one small range request (the
//!   probe), learns the body's total length from it, and then serves reads
//!   out of chunked windows fetched through the [`RangeTransport`] seam.
//!   `is_seekable()` is `true` and `byte_len()` answers what the probe
//!   learned, so the decoder can seek and report a duration (S2) while the
//!   first sound needs only the first window, not the whole body (S1).
//! - **S3** — a seek to any position re-requests that range on the next
//!   read; it never resolves to the end of the track.
//! - **S5** — a fetch that fails mid-track is retried exactly once, for the
//!   very same window, after the module's 500 ms `RETRY_WAIT` — the station
//!   rule's shape (one attempt more, the wait between the attempts, the
//!   last failure is the error). A second failure is the read's error, so
//!   the track fails with the sentence a failed fetch already uses and the
//!   queue never advances behind the user's back. A fetch that failed
//!   answered no bytes, so a retry cannot append the same bytes to the
//!   cache fill twice, and a seek replaces the window the retry was for.
//! - **D1** (the author's decision) — while the source reads sequentially
//!   from byte 0, the same bytes are appended to the cache's `.part` file
//!   for that URL ([`Cache::begin_fill`]); delivering the whole body renames
//!   that file into place exactly as `Cache::store` does, so a later play is
//!   a cache hit. A seek that skips ahead before the body is complete
//!   abandons the fill: the half file never gets the final name, and the
//!   prune rules of `src/subsonic/cache.rs` keep counting it among the
//!   `.part` files. A backwards seek opens no gap, so it keeps the fill.
//!
//! The window is [`WINDOW_BYTES`] (64 KiB): at the speeds the Subsonic
//! client documents for a channel's song (≈64 KB/s) one request always
//! answers well inside the transport's 30-second stall detector, while a
//! 6 MB track costs about a hundred requests rather than thousands. A
//! server that answers `200` or omits the total answers the typed
//! [`RangeAnswer::NoRanges`] — never a silent whole-body read; the
//! cache-on-play fallback for it is T3's.

use std::io::{self, Read, Seek, SeekFrom};
use std::sync::Arc;
use std::time::Duration;

use symphonia::core::io::MediaSource;

use crate::subsonic::Error;
use crate::subsonic::cache::{Cache, CacheFill};

/// How many bytes one window fetch asks for: the chunked-read granularity of
/// [`RemoteSource`] (see the module note for why this size).
pub const WINDOW_BYTES: u64 = 64 * 1024;

/// How long a failed fetch waits before its one retry (S5): 500 ms, short
/// enough that a mid-track hiccup stays a hiccup the decoder's buffers
/// absorb — the same one-wait-between-attempts shape the station's `Retry`
/// gives an open, at a fraction of its 2 s because a stalled read holds the
/// decoding thread. Production sleeps this; tests never do — they replace
/// it through [`RemoteSource::with_retry_wait`].
const RETRY_WAIT: Duration = Duration::from_millis(500);

/// The answer to one range request of a URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RangeAnswer {
    /// `206 Partial Content` with a parsable `Content-Range` header: the
    /// requested bytes and the body's total length.
    Partial {
        /// The bytes of the requested range, in order.
        bytes: Vec<u8>,
        /// The total length of the body, as the server stated it.
        total: u64,
    },
    /// The server does not answer ranges: it sent `200` over the whole body,
    /// or a partial answer without a parsable total. No bytes ride on this
    /// answer — the fallback fetches the whole body explicitly (T3), never
    /// by silently reading it here.
    NoRanges,
}

/// The seam every range request goes through: one byte range of a URL, `from`
/// inclusive, `to` inclusive when given. The production implementation is
/// [`crate::subsonic::client::HttpTransport`]; tests inject a fixture that
/// serves slices of one canned body and never touch the network.
pub trait RangeTransport: Send + Sync {
    /// Fetches the byte range `from..=to` of `url` (`to` being `None` means
    /// to the end of the body) and answers the bytes plus the body's total
    /// length when the server says it.
    ///
    /// # Errors
    ///
    /// Fails when the request cannot be completed; a server that cannot
    /// answer ranges answers [`RangeAnswer::NoRanges`], not an error.
    fn range(&self, url: &str, from: u64, to: Option<u64>) -> Result<RangeAnswer, Error>;
}

impl<T: RangeTransport + ?Sized> RangeTransport for Arc<T> {
    fn range(&self, url: &str, from: u64, to: Option<u64>) -> Result<RangeAnswer, Error> {
        (**self).range(url, from, to)
    }
}

/// How a URL can play — the probe's answer to the start (remote-streaming
/// T2): the range-backed source the player opens, or the typed verdict that
/// the server cannot answer ranges and the start falls back to cache-on-play
/// (S4). Not an error: a server without ranges is a way of playing, not a
/// failure.
#[derive(Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "the source is the answer's whole point and the verdict\n    carries nothing; boxing it would only move the indirection onto every\n    caller the start path already has"
)]
pub enum RemoteOpen {
    /// The ranged source: opened, it sounds as its bytes arrive (S1) and —
    /// with the cache fill attached — leaves the body behind it (D1).
    Streams(RemoteSource),
    /// The server does not answer ranges: there is no ranged source, and the
    /// start plays the entry the way it always has — the whole body into the
    /// cache, `.part` + rename, then the open.
    NoRanges,
}

/// Asks a URL how it plays (remote-streaming T2): one probe request, the
/// [`RemoteSource`] constructor's own — so the call embeds the probe on
/// whichever thread its caller chose. The start runs it on its resolve
/// worker, never on the event loop.
///
/// # Errors
///
/// Fails with the transport's error when the probe request fails. A server
/// that cannot answer ranges is [`RemoteOpen::NoRanges`], never an error.
pub fn open(transport: impl RangeTransport + 'static, url: &str) -> Result<RemoteOpen, Error> {
    open_shared(Arc::new(transport), url)
}

fn open_shared(transport: Arc<dyn RangeTransport>, url: &str) -> Result<RemoteOpen, Error> {
    let answer = transport.range(url, 0, Some(WINDOW_BYTES - 1))?;
    match answer {
        RangeAnswer::Partial { bytes, total } => Ok(RemoteOpen::Streams(RemoteSource {
            transport,
            url: url.to_owned(),
            total,
            pos: 0,
            window_from: 0,
            window: bytes,
            fill: None,
            retry_wait: Arc::new(std::thread::sleep),
        })),
        RangeAnswer::NoRanges => Ok(RemoteOpen::NoRanges),
    }
}

/// [`open`] with the cache fill attached to the streamed answer (D1): the
/// bytes it reads from byte 0 on are appended to the cache's `.part` file for
/// `url`, renamed into place when the whole body has been delivered — the
/// same rules [`RemoteSource::with_cache_fill`] follows. A `NoRanges` answer
/// attaches no fill; a URL the cache already holds fills nothing.
///
/// # Errors
///
/// As [`open`], plus when the `.part` file cannot be created.
pub fn open_with_fill(
    transport: impl RangeTransport + 'static,
    url: &str,
    cache: &Cache,
) -> Result<RemoteOpen, Error> {
    open_shared_with_fill(Arc::new(transport), url, cache)
}

fn open_shared_with_fill(
    transport: Arc<dyn RangeTransport>,
    url: &str,
    cache: &Cache,
) -> Result<RemoteOpen, Error> {
    let mut source = match open_shared(transport, url)? {
        // The fallback begins no fill: the whole-body download and the fill
        // would race onto one URL's `.part` files (S4).
        RemoteOpen::NoRanges => return Ok(RemoteOpen::NoRanges),
        RemoteOpen::Streams(source) => source,
    };
    if source.total == 0 || cache.path_for(url).is_file() {
        return Ok(RemoteOpen::Streams(source));
    }
    source.fill = Some(FillState {
        fill: cache.begin_fill(url)?,
        next: 0,
    });
    // The probe's window is the fill's first bytes.
    let seeded = std::mem::take(&mut source.window);
    source.apply_fill(&seeded, 0);
    source.window = seeded;
    Ok(RemoteOpen::Streams(source))
}

/// The probe's answer as an error, for the callers that asked for a source
/// outright: the sentence a `NoRanges` verdict carries there. The fallback
/// for it is the start's (T3).
fn no_ranges() -> Error {
    Error::Unreachable {
        detail: "the server does not answer range requests".to_owned(),
    }
}

/// The cache fill running behind the reads: `next` is the first body byte
/// the `.part` file does not hold yet, so an append is only sound when it
/// continues at `next` — otherwise a seek has opened a gap and the fill is
/// abandoned.
#[derive(Debug)]
struct FillState {
    fill: CacheFill,
    next: u64,
}

/// A remote URL as a seekable [`MediaSource`]: the bytes arrive as chunked
/// range requests, a seek re-requests its own range, and — while the reads
/// stay sequential from byte 0 — the cache fills behind them (S1, S2, D1).
pub struct RemoteSource {
    transport: Arc<dyn RangeTransport>,
    url: String,
    /// The body's total length, learned by the probe request.
    total: u64,
    /// The read position: the next byte `read` serves.
    pos: u64,
    /// The current window: the bytes of the body from `window_from` on.
    window: Vec<u8>,
    window_from: u64,
    /// The cache fill running behind the reads, when one was asked for and
    /// has not completed or been abandoned yet.
    fill: Option<FillState>,
    /// The wait between a failed fetch and its one retry (S5): production
    /// sleeps `RETRY_WAIT` on the reading thread; tests replace it with a
    /// recorder so no test waits on the wall clock.
    retry_wait: Arc<dyn Fn(Duration) + Send + Sync>,
}

impl RemoteSource {
    /// Opens the source: one probe request establishes the total length and
    /// seeds the first window, so a read at byte 0 needs no second request.
    ///
    /// # Errors
    ///
    /// Fails with the transport's error when the probe request fails, and
    /// with a sentence naming the missing ranges when the server answers
    /// [`RangeAnswer::NoRanges`] — the fallback for that is T3's.
    pub fn new(transport: impl RangeTransport + 'static, url: &str) -> Result<Self, Error> {
        match open(Arc::new(transport), url)? {
            RemoteOpen::Streams(source) => Ok(source),
            RemoteOpen::NoRanges => Err(no_ranges()),
        }
    }

    /// The same source, filling the cache behind its sequential reads (D1):
    /// the bytes it fetches from byte 0 on are appended to the cache's
    /// `.part` file for `url`, renamed into place when the whole body has
    /// been delivered. A URL the cache already holds fills nothing.
    ///
    /// # Errors
    ///
    /// As [`RemoteSource::new`], plus when the `.part` file cannot be
    /// created.
    pub fn with_cache_fill(
        transport: impl RangeTransport + 'static,
        url: &str,
        cache: &Cache,
    ) -> Result<Self, Error> {
        match open_with_fill(Arc::new(transport), url, cache)? {
            RemoteOpen::Streams(source) => Ok(source),
            RemoteOpen::NoRanges => Err(no_ranges()),
        }
    }

    /// Replaces the wait between a failed fetch and its one retry (S5) —
    /// the tests' seam: no test sleeps the wall clock. Production keeps the
    /// module's own `RETRY_WAIT`.
    #[must_use]
    pub fn with_retry_wait(mut self, wait: Arc<dyn Fn(Duration) + Send + Sync>) -> Self {
        self.retry_wait = wait;
        self
    }

    /// Appends to the fill the part of one fetched range that continues it.
    /// A range that starts beyond the fill's `next` would leave a gap in the
    /// cached file — a seek skipped ahead — and abandons the fill instead:
    /// the half file never gets the final name (D1). A range that starts
    /// below `next` re-covers bytes already appended (a backwards seek);
    /// only its part above `next` is new. A cache that cannot be written is
    /// not a playback problem: the reads go on, only the fill stops.
    fn apply_fill(&mut self, bytes: &[u8], from: u64) {
        let Some(mut state) = self.fill.take() else {
            return;
        };
        if from > state.next {
            return; // dropped: abandoned, the .part file stays behind
        }
        let skip = usize::try_from(state.next - from)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        let fresh = &bytes[skip..];
        if fresh.is_empty() {
            self.fill = Some(state);
            return;
        }
        let appended = u64::try_from(fresh.len()).unwrap_or(u64::MAX);
        if state.fill.append(fresh).is_err() {
            return; // dropped: abandoned
        }
        state.next += appended;
        if state.next >= self.total {
            // The whole body has been delivered: the file goes to its final
            // name exactly as `Cache::store` leaves it, prune included.
            let _ = state.fill.finish();
            return; // complete either way; the reads go on without a fill
        }
        self.fill = Some(state);
    }

    /// Fetches the window starting at the read position. A failed fetch is
    /// retried exactly once, for the very same window, after 500 ms
    /// (`RETRY_WAIT`) — the station rule's shape: one attempt more, the
    /// wait between the attempts and never before the first, and the
    /// second failure is the error the read fails with, exactly as a single
    /// failure is today (S5) — the interface's failure paths keep the
    /// sentence and the queue. A fetch that failed answered no bytes, so
    /// the retry cannot advance the fill's `next` twice for the same
    /// bytes.
    fn fetch_window(&mut self) -> Result<(), Error> {
        let from = self.pos;
        let to = (from + WINDOW_BYTES - 1).min(self.total - 1);
        let answer = match self.transport.range(&self.url, from, Some(to)) {
            Ok(answer) => answer,
            Err(_first) => {
                (self.retry_wait)(RETRY_WAIT);
                self.transport.range(&self.url, from, Some(to))?
            }
        };
        match answer {
            RangeAnswer::Partial { bytes, total } => {
                self.total = total;
                self.apply_fill(&bytes, from);
                self.window_from = from;
                self.window = bytes;
                Ok(())
            }
            RangeAnswer::NoRanges => Err(Error::Unreachable {
                detail: "the server does not answer range requests".to_owned(),
            }),
        }
    }
}

impl std::fmt::Debug for RemoteSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteSource")
            .field("url", &self.url)
            .field("total", &self.total)
            .field("pos", &self.pos)
            .field("window_from", &self.window_from)
            .field("window_len", &self.window.len())
            .field("fill", &self.fill.is_some())
            .finish_non_exhaustive()
    }
}

impl Read for RemoteSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.pos >= self.total {
            return Ok(0);
        }
        let window_end = self.window_from + self.window.len() as u64;
        if self.pos < self.window_from || self.pos >= window_end {
            self.fetch_window()
                .map_err(|error| io::Error::other(error.to_string()))?;
        }
        let offset = usize::try_from(self.pos - self.window_from).unwrap_or(0);
        let n = buf.len().min(self.window.len() - offset);
        buf[..n].copy_from_slice(&self.window[offset..offset + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for RemoteSource {
    fn seek(&mut self, whence: SeekFrom) -> io::Result<u64> {
        let target: i128 = match whence {
            SeekFrom::Start(offset) => i128::from(offset),
            SeekFrom::End(delta) => i128::from(self.total) + i128::from(delta),
            SeekFrom::Current(delta) => i128::from(self.pos) + i128::from(delta),
        };
        // A seek past either end clamps, the way reading a file past its end
        // simply has nothing left instead of failing.
        let target = u64::try_from(target.clamp(0, i128::from(self.total))).unwrap_or(self.total);
        // A seek that would skip ahead past the bytes appended so far stops
        // the fill — a gap in the cached file is worse than no file (D1).
        if self.fill.as_ref().is_some_and(|state| target > state.next) {
            self.fill = None;
        }
        self.pos = target;
        Ok(self.pos)
    }
}

impl MediaSource for RemoteSource {
    fn is_seekable(&self) -> bool {
        // Every position is reachable by a range request (S2, S3).
        true
    }

    fn byte_len(&self) -> Option<u64> {
        // What the probe request learned (S2).
        Some(self.total)
    }
}
