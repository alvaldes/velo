//! `youtube://<id>` -> an audio stream, resolved when it plays (issue #70,
//! S4 as revised by L9; live broadcasts play, L10).
//!
//! The queue holds the video's identity; the stream is opened at the moment
//! playback reaches the entry, every time, and **the tool holds the stream
//! rather than handing velo a URL**. The URL route this module once carried
//! is deleted on purpose, and the reason is measured, not guessed (L9): over
//! five consecutive extractions of one video, the only format whose URL
//! answers a ranged GET is itag 139 — HE-AAC, which symphonia refuses —
//! while itag 140 (AAC-LC), 251 (opus) and every other decodable format
//! answer `403` both to a bare GET and to a GET carrying the extraction's
//! own `http_headers`. No retry fixes that, so nothing fetches a URL any
//! more.
//!
//! What velo runs instead is a **chain of two children**:
//!
//! ```text
//! yt-dlp -f bestaudio -o - <url> | ffmpeg -hide_banner -loglevel error -i - -f mp3 -
//! ```
//!
//! `yt-dlp` streams the best audio it can get — webm/opus, m4a/AAC, and for
//! a live broadcast raw MPEG-TS, because the tool cannot post-process a live
//! source — and `ffmpeg` decodes whatever that is and writes MP3 to the
//! stdout velo reads. That is the whole point: the tree never needs a
//! Matroska or MPEG-TS demuxer, and the format question disappears. MP3 on
//! purpose: symphonia decodes it without seeking a container, the same shape
//! the radio's ICY path already proves. It is also why a live broadcast
//! plays now (L10): the refusal lived on an extraction that asked the tool
//! whether the video was live, and on a demuxer the tree does not have —
//! the chain needs neither, so the extraction, its live verdict and
//! `LIVE_SENTENCE` are deleted with it. One `yt-dlp` invocation per play,
//! and live is not a special case anywhere. What the chain does not buy —
//! a seek inside the stream — is a later slice: a seek on a non-seekable
//! source would be a fresh open at zero, a silent restart of the track, so
//! the interface refuses it; a live entry's `duration` is `None`, so the
//! existing no-length rule already refuses it with its own sentence.
//!
//! The resolve is bounded work on the thread the caller chose (S6): the
//! chain runs under [`tool::RESOLVE`], and the stream is pre-buffered —
//! [`tool::spawn`] reads up to [`tool::PREBUFFER`] of ffmpeg's output into
//! memory before the handover, so a hiccup in delivery does not empty the
//! device's buffer. Both are off the event loop, never a hang.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};

use symphonia::core::io::MediaSource;

use crate::audio::AudioError;
use crate::audio::output::PathResolver;
use crate::youtube::{is_pseudo_path, tool, video_id};

/// The downloader's half of the chain: the tool streams the best audio it
/// can get to stdout — its own delivery, never a URL this tree would fetch
/// (L9) — and ffmpeg transcodes it ([`ffmpeg_arguments`]). `--` ends the
/// options before the watch URL, so the id is one argument, never a shell
/// line, and `--no-playlist` keeps a watch URL carrying `&list=` from
/// resolving a whole playlist. No `-x`, no `--audio-format`: post-processing
/// is the transcoder's work now, and it is what lets a live broadcast play.
#[must_use]
pub fn arguments(id: &str) -> Vec<OsString> {
    [
        "--no-playlist",
        "--no-warnings",
        "--no-progress",
        "-f",
        "bestaudio",
        "-o",
        "-",
    ]
    .into_iter()
    .map(OsString::from)
    .chain(std::iter::once(OsString::from(format!(
        "https://www.youtube.com/watch?v={id}"
    ))))
    .collect()
}

/// The transcoder's half of the chain: the exact shape measured working
/// against a live broadcast — its MPEG-TS included — and against recorded
/// video alike. ffmpeg decodes whatever arrives on stdin and writes MP3 to
/// stdout; `-loglevel error` keeps the pipe quiet and the stderr free for
/// the one line a failure quotes.
#[must_use]
pub fn ffmpeg_arguments() -> Vec<OsString> {
    [
        "-hide_banner",
        "-loglevel",
        "error",
        "-i",
        "-",
        "-f",
        "mp3",
        "-",
    ]
    .into_iter()
    .map(OsString::from)
    .collect()
}

/// The resolver for a machine with both tools: a local file passes through
/// untouched, and a `youtube://` row is resolved to the chain's stream when
/// it plays. Both program paths are constructor arguments, so a test builds
/// one over fake binaries and no test reaches the network.
pub struct Resolver {
    yt_dlp: PathBuf,
    ffmpeg: PathBuf,
}

impl Resolver {
    /// A resolver over `yt_dlp` — the downloader — and `ffmpeg`, the
    /// transcoder its stream is decoded by.
    #[must_use]
    pub fn new(yt_dlp: PathBuf, ffmpeg: PathBuf) -> Self {
        Self { yt_dlp, ffmpeg }
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

    /// A `YouTube` row is never already reachable: reaching the entry always
    /// waits — which is what sends it to the resolver's worker and what the
    /// interface's fetching line says.
    fn cached(&self, path: &Path) -> bool {
        !is_pseudo_path(path)
    }

    /// How a `youtube://` row plays: the chain is spawned — one `yt-dlp`
    /// invocation, its stream decoded by ffmpeg — and ffmpeg's stdout is
    /// returned as the [`MediaSource`], pre-buffered by [`tool::spawn`]
    /// before the handover. Never [`crate::audio::remote`], because nothing
    /// about a `YouTube` stream is fetched or cached (S7): nothing about it
    /// is stored. A row that is not ours answers `None`, the trait's own
    /// pass-through. A live broadcast is not a special case: the chain
    /// handles whatever the tool streams (L10).
    ///
    /// # Errors
    ///
    /// When the row is broken (no usable id), the chain cannot start
    /// (whichever tool is missing is named), a stage exits before the first
    /// byte (the failing tool's own stderr line), or the pre-buffer never
    /// fills within [`tool::RESOLVE`]: each names its reason, and a broken
    /// row is refused before any process runs.
    fn open_streaming(&self, path: &Path) -> Result<Option<Box<dyn MediaSource>>, AudioError> {
        let Some(id) = video_id(path)? else {
            return Ok(None);
        };
        let child = tool::spawn(
            &self.yt_dlp,
            &arguments(id),
            &self.ffmpeg,
            &ffmpeg_arguments(),
            tool::RESOLVE,
        )
        .map_err(|error| AudioError::Source(error.to_string()))?;
        Ok(Some(Box::new(PipedSource { child })))
    }
}

/// The transcoder's stdout as the [`MediaSource`] the decoder reads: reads
/// pass through ([`tool::Running`] serves the pre-buffer bytes read before
/// the handover, then the pipe), the stream is never seekable, and it
/// reports no length — a sequential pipe has no position to seek to and no
/// end to know. Dropping it ends the chain: the last reader closing is both
/// children's end, which is what keeps a playback closed or killed from
/// leaving a downloader and a transcoder running behind it.
struct PipedSource {
    child: tool::Running,
}

impl Read for PipedSource {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.child.read(buf)
    }
}

/// A pipe has no position to seek to. `MediaSource` requires the trait, and
/// this mirrors symphonia's own [`ReadOnlySource`] — the wrapper the radio's
/// ICY path reads through — bit for bit: the error is the honest answer, and
/// the decoder never asks, because [`MediaSource::is_seekable`] answers
/// `false`.
impl std::io::Seek for PipedSource {
    fn seek(&mut self, _whence: std::io::SeekFrom) -> std::io::Result<u64> {
        Err(std::io::Error::other("a pipe does not support seeking"))
    }
}

impl MediaSource for PipedSource {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        None
    }
}

impl Drop for PipedSource {
    fn drop(&mut self) {
        // Dropping the source ends the chain: the last reader closing is
        // both children's end.
        self.child.end();
    }
}
