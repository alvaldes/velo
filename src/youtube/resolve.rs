//! `youtube://<id>` -> an audio stream, resolved when it plays (issue #70,
//! S4).
//!
//! The queue holds the video's identity; the URL `yt-dlp` extracts expires,
//! so the resolve runs at the moment playback reaches the entry, every time.
//! The URL it answers is opened by the same ranged-stream machinery a radio
//! channel's song uses ([`crate::audio::remote`]), so a result is a finite
//! track: it has a duration and it seeks. A stream that cannot be ranged is
//! refused with its own sentence — falling back to [`PathResolver::resolve`]
//! would be a refusal in disguise, because a `YouTube` entry is never a file.
//!
//! The resolve is one bounded child (`tool::RESOLVE`, S6) on the thread the
//! caller chose — a start's worker, or the producer — never the event loop.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use symphonia::core::io::MediaSource;

use crate::audio::AudioError;
use crate::audio::output::PathResolver;
use crate::audio::remote::{self, RangeTransport, RemoteOpen};
use crate::youtube::{is_pseudo_path, tool, video_id};

/// The format selector of one resolve: the decoder is symphonia with
/// isomp4/aac/mp3 and has no Matroska/WebM demuxer, so `bestaudio`'s default
/// (`webm`/opus) would be a stream velo cannot decode. `ext=m4a` (AAC in
/// MP4) plays through the path that already exists; `mp3` is the first
/// fallback. Everything else fails with the tool's own sentence, which names
/// the format it could not use rather than hanging on a stream nobody can
/// read.
const FORMAT: &str = "bestaudio[ext=m4a]/bestaudio[ext=mp3]/bestaudio";

/// The truth about a live broadcast, said by both refusals — the resolver's
/// (the answer carries no playable format) and the screen's (a live row
/// chosen anyway). `yt-dlp` answers a live broadcast's streams as HLS
/// manifests, and velo has no HLS demuxer: the honest sentence names what it
/// is and what to do, never the symptom of a failed open.
pub const LIVE_SENTENCE: &str =
    "a live broadcast is HLS and velo cannot play HLS — pick a recorded video";

/// Said when the answer carries only audio this build cannot decode: `YouTube`
/// serves HE-AAC and opus as often as AAC-LC, and neither plays here.
pub const UNDECODABLE_SENTENCE: &str = "YouTube offered only audio velo cannot decode — HE-AAC \
     or opus, and this build reads AAC-LC and mp3 — try another result";

/// The arguments of one resolve: the options first, `--` to end them, and
/// the watch URL last — the id is one argument, never a shell line, and
/// `--no-playlist` keeps a watch URL carrying `&list=` from resolving a
/// whole playlist. The full extraction (`--dump-single-json`), not
/// `--get-url`: the bare URL `--get-url` prints is often a signed googlevideo
/// address a plain ranged GET answers 403, while the `url` of the matching
/// `formats[]` entry from the full answer answers 206 — measured on
/// `yt-dlp` 2026.10.26.
#[must_use]
pub fn arguments(id: &str) -> Vec<OsString> {
    [
        "--no-playlist",
        "--no-warnings",
        "--no-progress",
        "--dump-single-json",
        "--format",
        FORMAT,
        "--",
    ]
    .into_iter()
    .map(OsString::from)
    .chain(std::iter::once(OsString::from(format!(
        "https://www.youtube.com/watch?v={id}"
    ))))
    .collect()
}

/// The resolver for a machine with `yt-dlp`: a local file passes through
/// untouched, and a `youtube://` row is resolved to a ranged stream when it
/// plays. The program path is a constructor argument, so a test builds one
/// over a fake binary and no test reaches the network.
pub struct Resolver {
    program: PathBuf,
    /// The range transport the resolved URL is opened through, when a test
    /// injected one. Without one the ask builds the production transport
    /// itself — the Subsonic client's own, the same one the streaming radio
    /// path uses.
    transport: Option<Arc<dyn RangeTransport>>,
}

impl Resolver {
    /// A resolver over `program`.
    #[must_use]
    pub fn new(program: PathBuf) -> Self {
        Self {
            program,
            transport: None,
        }
    }

    /// Injects the range transport the resolved URL is opened through.
    /// Tests only: production builds the transport the ask needs, because
    /// this resolver carries no client whose transport could be erased.
    #[must_use]
    pub fn with_transport(mut self, transport: Arc<dyn RangeTransport>) -> Self {
        self.transport = Some(transport);
        self
    }
}

impl PathResolver for Resolver {
    fn resolve(&self, path: &Path) -> Result<PathBuf, AudioError> {
        if !is_pseudo_path(path) {
            return Ok(path.to_path_buf());
        }
        Err(AudioError::Source(format!(
            "{} is a YouTube entry — velo plays it as a resolved stream, and it is never a file",
            path.to_string_lossy()
        )))
    }

    /// A `YouTube` row is never already reachable: the URL a resolve extracts
    /// expires, so reaching the entry always waits — which is what sends it
    /// to the resolver's worker and what the interface's fetching line says.
    fn cached(&self, path: &Path) -> bool {
        !is_pseudo_path(path)
    }

    /// How a `youtube://` row plays: one bounded resolve for the media URL,
    /// then the radio machinery's ranged open — never
    /// [`crate::audio::remote::open_with_fill`], because a `YouTube` stream
    /// fills no cache (S7): nothing about it is stored. A stream that cannot
    /// be ranged is a refusal, not a fallback question — the entry has no
    /// file to fall back to, and the sentence says why. A row that is not
    /// ours answers `None`, the trait's own pass-through.
    ///
    /// # Errors
    ///
    /// When the row is broken (no usable id), the tool fails or misses its
    /// deadline, or the URL cannot be ranged: each names its reason, and a
    /// broken row is refused before any process runs.
    fn open_streaming(&self, path: &Path) -> Result<Option<Box<dyn MediaSource>>, AudioError> {
        let Some(id) = video_id(path)? else {
            return Ok(None);
        };
        let url = resolved_url(&self.program, id)?;
        let transport = match &self.transport {
            Some(transport) => Arc::clone(transport),
            None => production_transport()?,
        };
        match remote::open(transport, &url) {
            Ok(RemoteOpen::Streams(source)) => Ok(Some(Box::new(source))),
            // Defensive: the selection only hands over plain-HTTP URLs, and
            // those answer ranged GETs — but a server that lies about it is
            // a refusal, never a fallback question (the entry has no file to
            // fall back to).
            Ok(RemoteOpen::NoRanges) => Err(AudioError::Source(
                "yt-dlp returned a stream that cannot be ranged — a stream that cannot be ranged \
                 cannot be seeked, and a YouTube entry has no file to fall back to"
                    .to_owned(),
            )),
            Err(error) => Err(AudioError::Source(error.to_string())),
        }
    }
}

/// The members velo reads out of one extraction. Everything else the object
/// carries — descriptions, thumbnails, the video-level url — is ignored.
#[derive(miniserde::Deserialize)]
struct Extraction {
    formats: Option<Vec<Format>>,
}

/// One entry of the answer's `formats[]`.
///
/// `http_headers` exists on this object — the headers a client is meant to
/// send with the URL. It is not carried yet on purpose: a bare ranged GET of
/// the chosen URLs was measured answering 206, so nothing needs it today.
/// If a format starts refusing bare requests, the headers are the next thing
/// to carry — not a retry, not a guess.
#[derive(miniserde::Deserialize)]
struct Format {
    url: Option<String>,
    vcodec: Option<String>,
    acodec: Option<String>,
    protocol: Option<String>,
    abr: Option<f64>,
}

/// Whether one format is audio-only and plain HTTP — the two things the
/// streaming path can actually play. Audio-only: `vcodec` absent, `null` or
/// `"none"`, and `acodec` present and not `"none"`. Plain HTTP: `protocol`
/// absent or `http`/`https` — anything else (`m3u8`, `m3u8_native`, `dash`,
/// …) is a manifest or a segmented stream this tree cannot demux.
fn playable(format: &Format) -> bool {
    let audio_only = matches!(format.vcodec.as_deref(), None | Some("none"))
        && !matches!(format.acodec.as_deref(), None | Some("none"));
    let plain_http = matches!(format.protocol.as_deref(), None | Some("http" | "https"));
    audio_only && plain_http
}

/// What an answer's `formats[]` allowed.
enum Pick {
    /// One plain-HTTP, audio-only URL the decoder can read.
    Url(String),
    /// Nothing but manifests — the live case.
    ManifestOnly,
    /// Plain-HTTP audio the decoder cannot read at all.
    Undecodable,
}

/// Picks one playable format's URL by what this tree can **decode**, not by
/// what container it is in.
///
/// The distinction is not academic: `YouTube` serves AAC-LC (`mp4a.40.2`) and
/// HE-AAC (`mp4a.40.5`, SBR) both as `m4a`, and it lists the HE-AAC one first.
/// symphonia has `isomp4`/`aac`/`mp3` and no Matroska demuxer, so it reads the
/// first and refuses the second at decode time with "aac too complex" — a
/// codec's name for a profile it does not implement. So the codec decides, and
/// among the streams that decode, the loudest wins.
fn pick(formats: &[Format]) -> Pick {
    let candidates: Vec<&Format> = formats.iter().filter(|format| playable(format)).collect();
    if candidates.is_empty() {
        return Pick::ManifestOnly;
    }
    let best = candidates
        .iter()
        .copied()
        .filter(|format| decodes(format))
        .max_by(|a, b| {
            a.abr
                .partial_cmp(&b.abr)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .and_then(|format| format.url.clone().filter(|url| !url.is_empty()));
    best.map_or(Pick::Undecodable, Pick::Url)
}

/// Whether this tree's decoder reads `format`'s codec: AAC-LC and mp3, and
/// nothing else. HE-AAC is SBR — a profile symphonia does not implement — and
/// opus arrives in webm, whose demuxer this tree does not have.
fn decodes(format: &Format) -> bool {
    match format.acodec.as_deref() {
        Some(codec) => codec == "mp4a.40.2" || codec.starts_with("mp3"),
        None => false,
    }
}

/// One resolve call: the tool runs bounded by [`tool::RESOLVE`], its answer
/// is one extraction, and the chosen format's URL is the media URL.
///
/// # Errors
///
/// The tool's own failure — not installed, deadline, refused — as the audio
/// path carries it, an answer that reads as no extraction, or an answer with
/// no playable format: the live case, refused by its name before any HTTP
/// request is made.
fn resolved_url(program: &Path, id: &str) -> Result<String, AudioError> {
    let out = tool::run(program, &arguments(id), tool::RESOLVE)
        .map_err(|error| AudioError::Source(error.to_string()))?;
    if out.trim().is_empty() {
        return Err(AudioError::Source(
            "yt-dlp answered nothing — there is no URL to play".to_owned(),
        ));
    }
    let extraction: Extraction = miniserde::json::from_str(out.trim()).map_err(|_| {
        AudioError::Source(
            "yt-dlp answered something velo could not read as one extraction".to_owned(),
        )
    })?;
    let formats = extraction.formats.unwrap_or_default();
    match pick(&formats) {
        Pick::Url(url) => Ok(url),
        Pick::ManifestOnly => Err(AudioError::Source(LIVE_SENTENCE.to_owned())),
        Pick::Undecodable => Err(AudioError::Source(UNDECODABLE_SENTENCE.to_owned())),
    }
}

/// The production range transport (remote-streaming T1): the Subsonic
/// client's own HTTP transport, the same one the streamed song path uses.
///
/// # Errors
///
/// `HttpTransport::new` never fails today; the mapping keeps the seam free
/// to grow without a panic.
fn production_transport() -> Result<Arc<dyn RangeTransport>, AudioError> {
    crate::subsonic::client::HttpTransport::new(false)
        .map(|transport| Arc::new(transport) as Arc<dyn RangeTransport>)
        .map_err(|error| AudioError::Source(error.to_string()))
}
