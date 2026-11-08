//! Opening a live station URL as something the player can read (T5b1, S2).
//!
//! `open` performs one HTTP GET with `Icy-MetaData: 1` sent before the body
//! is read — without that header an Icecast server sends no metadata at all
//! — parses `Icy-Metaint` from the response headers case-insensitively, and
//! hands the body to [`IcyReader`] inside a symphonia
//! [`ReadOnlySource`](symphonia::core::io::ReadOnlySource), so
//! [`TrackSource::open_stream`] probes and decodes it as bytes arrive. A
//! stream has no duration, no seek and no disk cache: it is read, not
//! downloaded.
//!
//! ## Wrapper playlists and HLS (T5b2)
//!
//! Radio Browser's `url_resolved` frequently points at a one-entry `.pls`
//! or `.m3u` instead of audio. [`open`] inspects at most [`WRAPPER_CAP`]
//! bytes of the body and lets [`lists::unwrap_station`] sniff the format —
//! no sniffing is re-implemented here. A wrapper's first playable entry is
//! fetched **once** more with the same timeouts and headers; a wrapper of a
//! wrapper is an error naming that, never another hop. An HLS manifest
//! fails as [`StreamError::Hls`]: its `.ts` segments are unplayable here
//! and no byte of a segment is ever fetched. The inspected prefix is
//! chained back in front of the body when the body is audio, so the cap
//! costs nothing.
//!
//! ## Timeouts are per operation, never a deadline
//!
//! The ureq 3 agent here sets exactly three knobs and deliberately not
//! `timeout_global` (nor `timeout_per_call` / `timeout_recv_body`): those
//! are *total* budgets, and a live body never ends, so a total budget would
//! kill a healthy station mid-song. The three knobs that bound without
//! ending are:
//!
//! - [`Timeouts::connect`] — `timeout_connect`: one-shot connection setup
//!   (TCP + TLS). Nothing about it repeats, so it is a real deadline.
//! - [`Timeouts::response`] — `timeout_recv_response`: the wait for the
//!   response *headers*, once per open. Also a real deadline: headers are
//!   finite.
//! - [`Timeouts::read`] — `timeout_per_read`: ureq restarts this bound on
//!   *every* read of the body (see its `timings` module). An endless stream
//!   that keeps delivering bytes trips it never; a socket that stalls —
//!   accept-and-send-nothing servers, congested relays — errors within one
//!   bound. That is the only way to bound a body that never finishes.
//!
//! ## Demux state for the UI
//!
//! [`IcyReader`] sits inside the decoder's source chain, so its
//! `title()`/`title_seq()` are not directly reachable once `open` returns.
//! [`Observed`] wraps the demux, mirrors its sequence counter after every
//! read, and publishes changes into an shared [`Arc<Mutex<DemuxState>>`]
//! that [`StationStream`] exposes. Nothing re-parses metadata: the demux
//! remains the only authority on titles.

use std::fmt;
use std::io::{self, Cursor, Read};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use symphonia::core::io::ReadOnlySource;

use super::icy::IcyReader;
use super::lists::{self, ListsError};
use crate::audio::{AudioError, TrackSource};
use crate::radio::client::USER_AGENT;

/// The largest `Icy-Metaint` accepted: 64 KiB of audio per metadata block.
/// Real stations use 8 KiB–64 KiB; anything larger is a corrupt header.
const MAX_METINT: usize = 64 * 1024;

/// The most body bytes inspected while deciding whether a station URL is a
/// wrapper playlist. A real `.pls`/`.m3u` is a few KiB — Radio Browser
/// entries are a handful of lines — so 16 KiB is four times anything seen
/// in the wild, and a hostile server cannot stream forever into the
/// inspection buffer. The inspected bytes are never thrown away: when the
/// body turns out to be audio, the prefix is chained back in front of the
/// remaining stream, so no audio is lost and the cap is not a filter.
const WRAPPER_CAP: u64 = 16 * 1024;

/// The per-operation timeouts of one live stream. Every field bounds one
/// kind of wait; none of them bounds the *body as a whole*, which never
/// ends.
#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    /// Connection setup (TCP + TLS), once per open.
    pub connect: Duration,
    /// Waiting for the response headers, once per open.
    pub response: Duration,
    /// The stall bound for a single read of the body; restarts on every
    /// read, so a flowing stream is never cut and a stalled one errors.
    pub read: Duration,
}

impl Default for Timeouts {
    /// The production values: fast enough that a dead station is reported
    /// within seconds, loose enough for real network jitter on every read.
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(10),
            response: Duration::from_secs(10),
            read: Duration::from_secs(15),
        }
    }
}

/// Everything opening or reading a live station can fail with. Every
/// variant names what happened; none of them is a panic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamError {
    /// The station could not be reached: refused connection, DNS failure,
    /// TLS trouble, or a connect/response timeout.
    Unreachable {
        /// What the transport observed.
        detail: String,
    },
    /// The server answered with a status other than 200.
    BadStatus {
        /// The status the server sent.
        status: u16,
    },
    /// `Icy-Metaint` is present but not a usable interval: unparsable,
    /// zero, or beyond [`MAX_METINT`]. The value is named, because a
    /// corrupt interval is the one metadata failure that cannot be
    /// validated below this layer.
    BadMetaint {
        /// The header value exactly as the server sent it.
        value: String,
    },
    /// The body is not audio this player can decode, and not a wrapper
    /// playlist either. A wrapper that yields nothing playable (`NoStream`)
    /// or one pointing at another wrapper also lands here, with the reason
    /// in `detail`.
    NotAudio {
        /// Why the probe or decoder refused the body.
        detail: String,
    },
    /// The body is an HLS manifest: its entries are `.ts` media segments,
    /// and no decoder compiled here plays those. Named on its own because
    /// "unsupported format" from deep inside the decoder is exactly the
    /// message that starts a debugging session.
    Hls,
}

impl fmt::Display for StreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreachable { detail } => {
                write!(formatter, "the station could not be reached: {detail}")
            }
            Self::BadStatus { status } => write!(formatter, "the station answered HTTP {status}"),
            Self::BadMetaint { value } => {
                write!(formatter, "the station sent an unusable Icy-Metaint: {value}")
            }
            Self::NotAudio { detail } => write!(formatter, "the station body is not audio: {detail}"),
            Self::Hls => write!(
                formatter,
                "the station body is an HLS manifest: its .ts segments cannot be decoded"
            ),
        }
    }
}

impl std::error::Error for StreamError {}

/// The latest demux state, published by [`Observed`] and read by the UI.
#[derive(Default)]
struct DemuxState {
    title: Option<String>,
    seq: u64,
}

/// One opened station: the decoded track plus the demux state the UI polls.
///
/// `read_block` drives the demux; each read publishes any `StreamTitle`
/// change into the shared state that [`title`](Self::title) reads back.
pub struct StationStream {
    track: TrackSource,
    state: Arc<Mutex<DemuxState>>,
}

impl fmt::Debug for StationStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StationStream")
            .field("title", &self.title())
            .field("title_seq", &self.title_seq())
            .finish_non_exhaustive()
    }
}

impl StationStream {
    /// The next block of interleaved `f32` samples, or `None` at the end of
    /// the stream.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError`] when the body stalls past the injected read
    /// bound or the decoder fails; the caller decides whether to retry or
    /// advance the queue (that policy is T5c).
    pub fn read_block(&mut self) -> Result<Option<Vec<f32>>, AudioError> {
        self.track.read_block()
    }

    /// The sample format the station decodes at.
    #[must_use]
    pub fn spec(&self) -> crate::audio::TrackSpec {
        self.track.spec()
    }

    /// A live station never declares a duration.
    #[must_use]
    pub fn duration(&self) -> Option<Duration> {
        self.track.duration()
    }

    /// The latest `StreamTitle`, or `None` before the first one arrives.
    #[must_use]
    pub fn title(&self) -> Option<String> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .title
            .clone()
    }

    /// Changes only when [`title`](Self::title) changes, so a station that
    /// repeats the same title never repaints the UI.
    #[must_use]
    pub fn title_seq(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .seq
    }
}

/// The reader handed to symphonia: the ICY demux plus publication of its
/// title changes into the shared [`DemuxState`]. The demux itself stays the
/// only parser of metadata.
struct Observed<R: Read> {
    demux: IcyReader<R>,
    last_seq: u64,
    state: Arc<Mutex<DemuxState>>,
}

impl<R: Read> Read for Observed<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.demux.read(buf)?;
        let seq = self.demux.title_seq();
        if seq != self.last_seq {
            self.last_seq = seq;
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.title = self.demux.title().map(str::to_owned);
            state.seq = seq;
        }
        Ok(n)
    }
}

/// Performs one GET with `Icy-MetaData: 1` sent before the body, checks the
/// status, parses `Icy-Metaint` case-insensitively, and returns the owned
/// body reader plus the interval. Used for the station URL and, at most
/// once more, for a wrapper's resolved entry.
fn fetch(
    agent: &ureq::Agent,
    url: &str,
) -> Result<(impl Read + use<>, Option<usize>), StreamError> {
    let response = agent
        .get(url)
        .header("Icy-MetaData", "1")
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|error| match error {
            ureq::Error::StatusCode(status) => StreamError::BadStatus { status },
            other => StreamError::Unreachable {
                detail: other.to_string(),
            },
        })?;

    let status = response.status();
    if status != 200 {
        return Err(StreamError::BadStatus { status: status.as_u16() });
    }

    // Icecast sends the header lowercase; the header map lookup is
    // case-insensitive, so any spelling arrives here. No header at all is
    // a plain passthrough, never an error.
    let metaint = match response.headers().get("Icy-Metaint") {
        None => None,
        Some(raw) => {
            let value = raw
                .to_str()
                .map_err(|_| StreamError::BadMetaint {
                    value: String::from_utf8_lossy(raw.as_bytes()).into_owned(),
                })?
                .trim()
                .to_owned();
            let parsed: usize = value.parse().map_err(|_| StreamError::BadMetaint {
                value: value.clone(),
            })?;
            if parsed == 0 || parsed > MAX_METINT {
                return Err(StreamError::BadMetaint { value });
            }
            Some(parsed)
        }
    };

    // `into_reader` is the owned, `'static` body reader: it keeps reading
    // the live connection after `response` is dropped here.
    let (_, body) = response.into_parts();
    Ok((body.into_reader(), metaint))
}

/// The first [`WRAPPER_CAP`] bytes of `body`, with `body` itself handed back
/// positioned after them, plus whether the inspection finished cleanly.
///
/// A clean finish means the body ended (or the cap was reached) inside the
/// read bound — the bytes are complete enough to sniff. An error means the
/// body did not deliver its bytes within the read bound, which is an
/// audio-shaped property, not a playlist one: the caller skips sniffing and
/// hands the partial prefix to the decoder, where the stall surfaces through
/// the normal read path.
fn bounded_prefix<R: Read>(body: R) -> (Vec<u8>, R, bool) {
    let mut prefix = Vec::new();
    let mut taken = body.take(WRAPPER_CAP);
    let complete = taken.read_to_end(&mut prefix).is_ok();
    (prefix, taken.into_inner(), complete)
}

/// One list-sniffing decision shared by the resolved level. A body that is
/// neither a wrapper nor HLS goes on to the decoder; a known wrapper with no
/// playable entry is [`StreamError::NotAudio`] naming why; a nested wrapper
/// is the bounded end of resolution.
fn sniff<R: Read>(
    prefix: &[u8],
    body: R,
    metaint: Option<usize>,
) -> Result<(Vec<u8>, R, Option<usize>), StreamError> {
    match lists::unwrap_station(prefix) {
        Err(ListsError::NotAPlaylist) => Ok((prefix.to_vec(), body, metaint)),
        Err(lists_error) => Err(map_lists(&lists_error)),
        Ok(_) => Err(StreamError::NotAudio {
            detail: String::from(
                "the wrapper playlist points at another wrapper playlist; \
                 only one level is resolved",
            ),
        }),
    }
}

/// A wrapper failure that is final at whichever level it appeared, as a
/// [`StreamError`]: HLS is its own variant, a wrapper with no playable entry
/// keeps the reason [`lists`] gave.
fn map_lists(error: &ListsError) -> StreamError {
    match error {
        ListsError::Hls => StreamError::Hls,
        ListsError::NoStream | ListsError::NotAPlaylist => {
            StreamError::NotAudio { detail: error.to_string() }
        }
    }
}

/// Opens a live station: one GET with `Icy-MetaData: 1`, `Icy-Metaint`
/// validation, and the body wired into a non-seekable
/// [`TrackSource::open_stream`]. The agent uses per-operation timeouts
/// only — see the module documentation for why no total budget exists.
///
/// When the body sniffs as a wrapper playlist ([`lists::unwrap_station`]
/// knows the formats; no sniffing is re-implemented here), its first
/// playable entry is fetched **once more** with the same timeouts and the
/// same headers, and whatever that body is, is final: a wrapper pointing at
/// another wrapper is an error naming that, never another hop. An HLS
/// manifest fails as [`StreamError::Hls`]; a body that is neither audio nor
/// a playlist keeps failing as [`StreamError::NotAudio`].
///
/// # Errors
///
/// Returns [`StreamError::Unreachable`] when a station or a resolved
/// wrapper entry cannot be reached in time, [`StreamError::BadStatus`] for
/// a non-200 answer, [`StreamError::BadMetaint`] for an unusable
/// `Icy-Metaint`, [`StreamError::Hls`] for an HLS manifest, and
/// [`StreamError::NotAudio`] when the body does not decode and no wrapper
/// explains it.
pub fn open(url: &str, timeouts: Timeouts) -> Result<StationStream, StreamError> {
    let agent = ureq::Agent::config_builder()
        .timeout_connect(Some(timeouts.connect))
        .timeout_recv_response(Some(timeouts.response))
        .timeout_per_read(Some(timeouts.read))
        .build()
        .new_agent();

    let (body, metaint) = fetch(&agent, url)?;
    let (prefix, body, complete) = bounded_prefix(body);
    let (prefix, body, metaint) = if complete {
        match lists::unwrap_station(&prefix) {
            Err(ListsError::NotAPlaylist) => (prefix, body, metaint),
            Err(lists_error) => return Err(map_lists(&lists_error)),
            Ok(entry) => {
                // One level only: fetch the resolved entry once more, with
                // the same timeouts and the same headers, and whatever the
                // body turns out to be is final. A wrapper here is an
                // error naming that, never another hop.
                let (body, metaint) = fetch(&agent, &entry)?;
                let (prefix, body, complete) = bounded_prefix(body);
                if complete {
                    sniff(&prefix, body, metaint)?
                } else {
                    (prefix, body, metaint)
                }
            }
        }
    } else {
        // The inspection was cut off by the read bound: an audio-shaped
        // stall, not a playlist. Decode from what arrived; the stall
        // surfaces through the normal read path.
        (prefix, body, metaint)
    };

    let state = Arc::new(Mutex::new(DemuxState::default()));
    let observed = Observed {
        demux: IcyReader::new(Cursor::new(prefix).chain(body), metaint),
        last_seq: 0,
        state: Arc::clone(&state),
    };
    let track =
        TrackSource::open_stream(Box::new(ReadOnlySource::new(observed))).map_err(|error| {
            StreamError::NotAudio {
                detail: match error {
                    AudioError::Unsupported(detail) | AudioError::Decode(detail) => detail,
                    other => other.to_string(),
                },
            }
        })?;

    Ok(StationStream { track, state })
}
