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
use crate::audio::output::{StationOpener, StationSource};
use crate::audio::{AudioError, TrackSource, TrackSpec};
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
                write!(
                    formatter,
                    "the station sent an unusable Icy-Metaint: {value}"
                )
            }
            Self::NotAudio { detail } => {
                write!(formatter, "the station body is not audio: {detail}")
            }
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

    /// The codec the station decodes, when it has a name — what the one-shot
    /// probe prints (issue #13).
    #[must_use]
    pub fn codec_name(&self) -> Option<&'static str> {
        self.track.codec_name()
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

/// The one-shot agent [`probe`] and [`expand`] share: the three
/// per-operation timeouts of [`Timeouts`], and the redirect history kept
/// (`save_redirect_history(true)`), which only records URLs.
fn station_agent(timeouts: Timeouts) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(timeouts.connect))
        .timeout_recv_response(Some(timeouts.response))
        .timeout_per_read(Some(timeouts.read))
        .save_redirect_history(true)
        .build()
        .new_agent()
}

/// Performs one GET with `Icy-MetaData: 1` sent before the body, checks the
/// status, parses `Icy-Metaint` case-insensitively, and returns the owned
/// body reader plus the response facts the one-shot probe reports. Used for
/// the station URL and, at most once more, for a wrapper's resolved entry.
struct Fetched<R> {
    body: R,
    metaint: Option<usize>,
    /// The `Content-Type` the response declared, when it did.
    content_type: Option<String>,
    /// The URLs this fetch was redirected to, in order: the request URL
    /// itself is never listed, only the hops after it.
    redirects: Vec<String>,
}

fn fetch(agent: &ureq::Agent, url: &str) -> Result<Fetched<impl Read + use<>>, StreamError> {
    use ureq::ResponseExt as _;

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
        return Err(StreamError::BadStatus {
            status: status.as_u16(),
        });
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
    let content_type = response
        .headers()
        .get("Content-Type")
        .and_then(|value| value.to_str().ok().map(str::to_owned));
    let redirects = response
        .get_redirect_history()
        .map_or_else(Vec::new, |history| {
            history
                .iter()
                .skip(1)
                .map(std::string::ToString::to_string)
                .collect()
        });
    let (_, body) = response.into_parts();
    Ok(Fetched {
        body: body.into_reader(),
        metaint,
        content_type,
        redirects,
    })
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
        ListsError::NoStream | ListsError::NotAPlaylist => StreamError::NotAudio {
            detail: error.to_string(),
        },
    }
}

/// The facts gathered on the way through one open, for the one-shot probe
/// (issue #13). Nothing here changes what the player gets: [`open`] computes
/// the same things and throws this record away.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// The URL the station really points at after one wrapper level: `None`
    /// when the body was audio from the start.
    pub resolved_url: Option<String>,
    /// Every URL the station was redirected to, in order, across both
    /// fetches when a wrapper was resolved; empty when there were none.
    pub redirects: Vec<String>,
    /// The `Content-Type` the final response declared, when it did.
    pub content_type: Option<String>,
    /// The `Icy-Metaint` the final response declared, when it did.
    pub metaint: Option<usize>,
}

/// Opens a live station and reports what happened on the way: the same open
/// the player takes — one GET with `Icy-MetaData: 1`, one wrapper level
/// resolved, the body wired into [`TrackSource::open_stream`] — plus the
/// [`Probe`] record of the response facts. The redirect history is kept
/// (`save_redirect_history(true)`), which only records URLs: nothing the
/// player reads behaves differently.
///
/// # Errors
///
/// The same errors as [`open`].
pub fn probe(url: &str, timeouts: Timeouts) -> Result<(StationStream, Probe), StreamError> {
    let agent = station_agent(timeouts);

    let fetched = fetch(&agent, url)?;
    let mut redirects = fetched.redirects;
    let (prefix, body, complete) = bounded_prefix(fetched.body);
    let (prefix, body, metaint, content_type, resolved_url) = if complete {
        match lists::unwrap_station(&prefix) {
            Err(ListsError::NotAPlaylist) => {
                (prefix, body, fetched.metaint, fetched.content_type, None)
            }
            Err(lists_error) => return Err(map_lists(&lists_error)),
            Ok(entry) => {
                // One level only: fetch the resolved entry once more, with
                // the same timeouts and the same headers, and whatever the
                // body turns out to be is final. A wrapper here is an
                // error naming that, never another hop.
                let resolved = fetch(&agent, &entry)?;
                redirects.extend(resolved.redirects);
                let (prefix, body, complete) = bounded_prefix(resolved.body);
                let (prefix, body, metaint) = if complete {
                    sniff(&prefix, body, resolved.metaint)?
                } else {
                    (prefix, body, resolved.metaint)
                };
                (prefix, body, metaint, resolved.content_type, Some(entry))
            }
        }
    } else {
        // The inspection was cut off by the read bound: an audio-shaped
        // stall, not a playlist. Decode from what arrived; the stall
        // surfaces through the normal read path.
        (prefix, body, fetched.metaint, fetched.content_type, None)
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

    Ok((
        StationStream { track, state },
        Probe {
            resolved_url,
            redirects,
            content_type,
            metaint,
        },
    ))
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
    probe(url, timeouts).map(|(stream, _)| stream)
}

/// The queue's view of an opened station (T5c2a): the producer reads it
/// exactly as it reads a file's `TrackSource`, with a `None` duration and
/// no seek by contract (S3) and the demux's own title beside it (S4).
impl StationSource for StationStream {
    fn read_block(&mut self) -> Result<Option<Vec<f32>>, AudioError> {
        self.track.read_block()
    }

    fn spec(&self) -> TrackSpec {
        self.track.spec()
    }

    fn duration(&self) -> Option<Duration> {
        self.track.duration()
    }

    fn title(&self) -> Option<String> {
        StationStream::title(self)
    }
}

/// The production opener for queue station entries (T5c2a): one GET through
/// [`open`] with the default per-operation timeouts — see
/// [`Timeouts::default`]. Tests inject their own [`StationOpener`] instead;
/// that is the seam's point.
#[derive(Debug, Clone, Copy, Default)]
pub struct LiveOpener;

impl StationOpener for LiveOpener {
    fn open(&self, url: &str) -> Result<Box<dyn StationSource>, StreamError> {
        open(url, Timeouts::default()).map(|stream| Box::new(stream) as Box<dyn StationSource>)
    }
}

/// The most bytes read past the inspection cap once the body sniffs as a
/// wrapper. A wrapper is a finite text file, but a hostile server could
/// stream forever, so the read is bounded: 4 MiB is roughly 40,000 entries,
/// far past any real channel's song list.
const WRAPPER_REST_CAP: u64 = 4 * 1024 * 1024;

/// One song a wrapper lists (issue #34): the resolved absolute `http(s)`
/// URL — the song's identity, cliamp S3, and the only thing the cache and
/// the history ever see — beside the title the wrapper gave the entry (an
/// `.m3u`'s `#EXTINF` text, a `.pls`'s `Title<n>`), when it named one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Song {
    /// The song's `https://…` URL, resolved against the wrapper's own URL.
    pub url: String,
    /// The title the wrapper declared, when it did; never invented.
    pub title: Option<String>,
}

/// What a station URL resolved to, before anything is queued (cliamp S1):
/// the shell asks this once for a station the user configured in
/// `radio.toml`, and a multi-entry wrapper opens as the songs it lists — one
/// row per entry, in the wrapper's order, each playable and seekable as a
/// track. A wrapper with one entry, or a body that is audio outright, plays
/// live exactly as before (S6). Nothing here knows whose service the
/// wrapper is: the rule keys on the body alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expansion {
    /// The songs of a channel: one [`Song`] per entry, in the wrapper's
    /// order, each with its URL and the title the wrapper gave it (issue
    /// #34). Two or more — a wrapper with one entry plays live.
    Songs(Vec<Song>),
    /// Not a multi-entry wrapper: play live.
    Live,
}

/// Resolves one wrapper entry against the wrapper's own URL (cliamp S2):
/// an absolute `http(s)` entry stays as written, a scheme-relative
/// `//host/path` entry takes the wrapper's scheme, an absolute `/path`
/// entry joins the wrapper's origin, and a directory-relative entry joins
/// the wrapper's directory, with `./` and `../` resolved the way a browser
/// resolves a playlist entry. `None` for anything that cannot become a
/// fetchable URL — another scheme, an empty line.
#[must_use]
fn resolve_entry(base: &str, entry: &str) -> Option<String> {
    let entry = entry.trim();
    if entry.is_empty() {
        return None;
    }
    let lower = entry.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Some(entry.to_owned());
    }
    let (scheme, rest) = base.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.is_empty() {
        return None;
    }
    if let Some(path) = entry.strip_prefix("//") {
        return Some(format!("{scheme}://{path}"));
    }
    if let Some(path) = entry.strip_prefix('/') {
        return Some(format!("{scheme}://{authority}/{path}"));
    }
    let path = &rest[authority_end..];
    let directory = match path.rfind('/') {
        Some(index) => &path[..=index],
        None => "/",
    };
    let mut segments: Vec<&str> = directory
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .collect();
    let relative = entry.strip_prefix("./").unwrap_or(entry);
    for segment in relative.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    Some(format!("{scheme}://{authority}/{}", segments.join("/")))
}

/// The network-free half of [`expand`]: the expansion decision for one
/// fetched body against the URL it was fetched from. Two or more resolvable
/// entries are [`Expansion::Songs`]; one entry plays live (S1); none is the
/// refusal of cliamp S5. An HLS manifest is refused on its own: its `.ts`
/// segments are not songs. Not a wrapper at all — audio outright — is live.
fn expansion_for(base: &str, body: &[u8]) -> Result<Expansion, StreamError> {
    let entries = match lists::entries_with_titles(body) {
        Ok(entries) => entries,
        // Not a wrapper, or a known format with no playable entry — for the
        // expansion both mean the same thing: this is not a song list, and
        // the live path reports it as it always has.
        Err(ListsError::NotAPlaylist | ListsError::NoStream) => return Ok(Expansion::Live),
        Err(ListsError::Hls) => return Err(StreamError::Hls),
    };
    let songs: Vec<Song> = entries
        .into_iter()
        .filter_map(|(entry, title)| resolve_entry(base, &entry).map(|url| Song { url, title }))
        .collect();
    match songs.len() {
        0 => Err(StreamError::NotAudio {
            detail: String::from("the wrapper playlist carries no song entry"),
        }),
        1 => Ok(Expansion::Live),
        _ => Ok(Expansion::Songs(songs)),
    }
}

/// Resolves `url` once and decides between a finite song list and live
/// playback (cliamp S1): one GET with the same headers and per-operation
/// timeouts as [`open`]. A body that sniffs as a wrapper playlist is read
/// whole — a wrapper is a finite text file, and no entry may be cut in half
/// by the inspection cap — and expanded; anything else is
/// [`Expansion::Live`], and the live path re-fetches and reports what it
/// finds, exactly as before (S6).
///
/// A wrapper that carries no resolvable song entry, an unreachable URL and a
/// non-200 answer are refusals: the caller says one line naming the station
/// and the reason (cliamp S5) and queues nothing — a station already known
/// to be unopenable is never queued to fail again. A body that stalls
/// before its first bytes are read is *not* a verdict: the live path
/// re-fetches and reports the stall through its own error, the way it always
/// has.
///
/// # Errors
///
/// [`StreamError::Unreachable`] when the URL cannot be fetched,
/// [`StreamError::BadStatus`] for a non-200 answer, [`StreamError::Hls`]
/// for an HLS manifest, and [`StreamError::NotAudio`] when a wrapper
/// carries no song entry.
pub fn expand(url: &str, timeouts: Timeouts) -> Result<Expansion, StreamError> {
    let agent = station_agent(timeouts);
    let fetched = fetch(&agent, url)?;
    let (prefix, body, complete) = bounded_prefix(fetched.body);
    if !complete {
        // The inspection was cut off by the read bound: an audio-shaped
        // stall, not a playlist verdict. The live path re-fetches and
        // reports the stall through its own error.
        return Ok(Expansion::Live);
    }
    if matches!(
        lists::unwrap_station(&prefix),
        Err(ListsError::NotAPlaylist)
    ) {
        return Ok(Expansion::Live);
    }
    // A wrapper: read the rest of the body, so no entry is cut in half by
    // the inspection cap.
    let mut whole = prefix;
    let mut rest = Vec::new();
    body.take(WRAPPER_REST_CAP)
        .read_to_end(&mut rest)
        .map_err(|error| StreamError::Unreachable {
            detail: error.to_string(),
        })?;
    whole.extend_from_slice(&rest);
    expansion_for(url, &whole)
}
