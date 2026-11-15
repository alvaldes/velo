//! `youtube://<id>` -> an audio stream, resolved when it plays (issue #70,
//! S4, as revised in L9).
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
//! more: `yt-dlp -x --audio-format mp3 -o -` streams a sequential, decodable
//! MP3 to stdout, and that pipe is the [`MediaSource`]. Gone with the route,
//! deleted rather than left dead: the format picker and its codec gate
//! (`pick`, `Pick`, `decodes`), the `remote::open`/`RangeTransport` wiring
//! and the injected transport, and the tests that pinned them. What they
//! bought — seeking inside the stream — is a later slice: a seek on a
//! non-seekable source would be a fresh open at zero, a silent restart of
//! the track, so the interface refuses it until then.
//!
//! The resolve is bounded work on the thread the caller chose (S6): the
//! extraction that refuses a live broadcast by name (S3 — its sentence is
//! reviewed and stays) runs under [`tool::RESOLVE`], and the pipe is watched
//! for a stream that never starts by [`tool::spawn`]'s first-byte bound. Both
//! are off
//! the event loop, never a hang.
//!
//! Why the extraction is still here at all, when the pipe answers everything
//! else: one question, `live_status`, and nothing else. It is asked once,
//! before the pipe, so a live broadcast is refused by name before any
//! downloader runs — and the *only* thing it decides (R4): `live_status` is
//! the tool's own word for whether the video is live right now, so a
//! recorded video is never called live, whatever its `formats[]` look like;
//! a recorded video the pipe cannot deliver fails with the pipe's own
//! reason, which names the format that could not be used. Reading anything
//! more out of the extraction would re-decide what the pipe already decides,
//! and the one past reading — a format scan for "playable" — was exactly
//! how a recorded video got called a live broadcast.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};

use symphonia::core::io::MediaSource;

use crate::audio::AudioError;
use crate::audio::output::PathResolver;
use crate::youtube::{is_pseudo_path, tool, video_id};

/// The truth about a live broadcast, said by both refusals — the resolver's
/// (the extraction's `live_status` says `is_live`) and the screen's (a live
/// row chosen anyway). `yt-dlp` answers a live broadcast's streams as HLS
/// manifests, and velo has no HLS demuxer: the honest sentence names what it
/// is and what to do, never the symptom of a failed open.
pub const LIVE_SENTENCE: &str =
    "a live broadcast is HLS and velo cannot play HLS — pick a recorded video";

/// The arguments of the pipe: the tool extracts the audio itself and
/// transcodes it to MP3 on its way to stdout — the one route measured
/// working first try (L9). MP3 on purpose: symphonia decodes it without
/// seeking a container, the same shape the radio's ICY path already proves;
/// the format selector the URL route needed is gone with that route.
/// `--` ends the options before the watch URL, so the id is one argument,
/// never a shell line, and `--no-playlist` keeps a watch URL carrying
/// `&list=` from resolving a whole playlist.
#[must_use]
pub fn arguments(id: &str) -> Vec<OsString> {
    pipe(
        [
            "--no-playlist",
            "--no-warnings",
            "--no-progress",
            "-x",
            "--audio-format",
            "mp3",
            "-o",
            "-",
        ],
        id,
    )
}

/// The arguments of the extraction that refuses a live broadcast by name
/// (S3): the full answer, read for one thing only — its `live_status`, the
/// tool's own word for whether the video is live right now (R4). No URL is
/// taken from it, no format is selected in it, and nothing else about it
/// decides anything: the stream comes from the pipe ([`arguments`]), not
/// from this answer.
#[must_use]
pub fn extraction_arguments(id: &str) -> Vec<OsString> {
    pipe(
        [
            "--no-playlist",
            "--no-warnings",
            "--no-progress",
            "--dump-single-json",
        ],
        id,
    )
}

/// One `yt-dlp` call's shape: the options first, `--` to end them, and the
/// watch URL last — the id is one argument, never a shell line.
fn pipe<const N: usize>(options: [&'static str; N], id: &str) -> Vec<OsString> {
    options
        .into_iter()
        .map(OsString::from)
        .chain(std::iter::once(OsString::from(format!(
            "https://www.youtube.com/watch?v={id}"
        ))))
        .collect()
}

/// The resolver for a machine with `yt-dlp`: a local file passes through
/// untouched, and a `youtube://` row is resolved to the tool's own stream
/// when it plays. The program path is a constructor argument, so a test
/// builds one over a fake binary and no test reaches the network.
pub struct Resolver {
    program: PathBuf,
}

impl Resolver {
    /// A resolver over `program`.
    #[must_use]
    pub fn new(program: PathBuf) -> Self {
        Self { program }
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

    /// How a `youtube://` row plays: the extraction refuses a live broadcast
    /// by name, then the pipe is spawned and its stdout returned as the
    /// [`MediaSource`] — never [`crate::audio::remote`], because nothing
    /// about a `YouTube` stream is fetched or cached any more (S7): nothing
    /// about it is stored. A row that is not ours answers `None`, the
    /// trait's own pass-through.
    ///
    /// # Errors
    ///
    /// When the row is broken (no usable id), the extraction fails, the
    /// answer's `live_status` says the video is live, or the pipe never
    /// starts within [`tool::STARTUP`]: each names its reason, and a broken
    /// row is refused before any process runs.
    fn open_streaming(&self, path: &Path) -> Result<Option<Box<dyn MediaSource>>, AudioError> {
        let Some(id) = video_id(path)? else {
            return Ok(None);
        };
        refuse_live(&self.program, id)?;
        let child = tool::spawn(&self.program, &arguments(id))
            .map_err(|error| AudioError::Source(error.to_string()))?;
        Ok(Some(Box::new(PipedSource { child })))
    }
}

/// The tool's own stdout as the [`MediaSource`] the decoder reads: reads
/// pass through ([`tool::Running`] serves the bytes read before the
/// handover, then the pipe), the stream is never seekable, and it reports
/// no length — a sequential pipe has no position to seek to and no end to
/// know. Dropping it ends the child: the last reader closing is the
/// downloader's end, which is what keeps a playback closed or killed from
/// leaving a downloader running behind it.
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
        // Dropping the source ends the child: the last reader closing is
        // the downloader's end.
        self.child.end();
    }
}

/// The one member the extraction is read for: the tool's own word for
/// whether the video is live right now. Everything else the object carries —
/// descriptions, thumbnails, `formats[]`, URLs — is ignored: no URL is taken
/// from the answer and no format is judged in it (L9, R4), the stream comes
/// from the pipe and the pipe's own failures speak for it.
#[derive(miniserde::Deserialize)]
struct Extraction {
    live_status: Option<String>,
}

/// The extraction that refuses a live broadcast by name, before any pipe is
/// spawned (S3): the sentence is reviewed and stays, and the verdict is the
/// answer's own `live_status` — the one thing the extraction decides (R4).
/// `is_live` is the only value that means live now: `not_live`, `was_live`
/// (a finished broadcast, downloadable like any recording) and
/// `is_upcoming` name a recorded or scheduled video, and a recorded video
/// the pipe cannot use fails with the pipe's own reason, never with a
/// sentence about a broadcast it is not.
///
/// # Errors
///
/// The tool's own failure — not installed, deadline, refused — as the audio
/// path carries it, an answer that reads as no extraction, or the live
/// verdict itself.
fn refuse_live(program: &Path, id: &str) -> Result<(), AudioError> {
    let out = tool::run(program, &extraction_arguments(id), tool::RESOLVE)
        .map_err(|error| AudioError::Source(error.to_string()))?;
    if out.trim().is_empty() {
        return Err(AudioError::Source(
            "yt-dlp answered nothing — there is nothing to play".to_owned(),
        ));
    }
    let extraction: Extraction = miniserde::json::from_str(out.trim()).map_err(|_| {
        AudioError::Source(
            "yt-dlp answered something velo could not read as one extraction".to_owned(),
        )
    })?;
    if extraction.live_status.as_deref() == Some("is_live") {
        return Err(AudioError::Source(LIVE_SENTENCE.to_owned()));
    }
    Ok(())
}
