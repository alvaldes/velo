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

/// The arguments of one resolve: the options first, `--` to end them, and
/// the watch URL last — the id is one argument, never a shell line, and
/// `--no-playlist` keeps a watch URL carrying `&list=` from resolving a
/// whole playlist.
#[must_use]
pub fn arguments(id: &str) -> Vec<OsString> {
    [
        "--no-playlist",
        "--no-warnings",
        "--no-progress",
        "--get-url",
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
        Err(AudioError::Io(std::io::Error::other(format!(
            "{} is a YouTube entry — velo plays it as a resolved stream, and it is never a file",
            path.to_string_lossy()
        ))))
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
            Ok(RemoteOpen::NoRanges) => Err(AudioError::Io(std::io::Error::other(
                "yt-dlp returned a stream that cannot be ranged — a stream that cannot be ranged \
                 cannot be seeked, and a YouTube entry has no file to fall back to",
            ))),
            Err(error) => Err(AudioError::Io(std::io::Error::other(error.to_string()))),
        }
    }
}

/// One resolve call: the tool runs bounded by [`tool::RESOLVE`], and its
/// first non-empty stdout line is the media URL.
///
/// # Errors
///
/// The tool's own failure — not installed, deadline, refused — as the audio
/// path carries it, or an empty answer where a URL should be.
fn resolved_url(program: &Path, id: &str) -> Result<String, AudioError> {
    let out = tool::run(program, &arguments(id), tool::RESOLVE)
        .map_err(|error| AudioError::Io(std::io::Error::other(error.to_string())))?;
    out.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            AudioError::Io(std::io::Error::other(
                "yt-dlp answered nothing — there is no URL to play",
            ))
        })
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
        .map_err(|error| AudioError::Io(std::io::Error::other(error.to_string())))
}
