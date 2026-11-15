//! `YouTube`, played through `yt-dlp` (issue #70).
//!
//! `yt-dlp` is not in the repository and never will be: it is an optional
//! external tool, found at launch and absent without a launch failure. velo
//! spawns it, reads its output, and never talks to `YouTube` itself.
//!
//! What a queue entry carries is the video's identity — `youtube://<id>` — and
//! never a stream URL: the URL a resolve extracts expires, so it is fetched
//! when the entry plays, not when it was queued (S4).

pub mod resolve;
pub mod search;
pub mod tool;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::audio::AudioError;
use crate::audio::output::PathResolver;

/// The program this module spawns: the downloader that starts the chain.
pub const PROGRAM: &str = "yt-dlp";

/// The transcoder the chain runs beside the downloader: whatever
/// [`PROGRAM`] streams, `ffmpeg` decodes it and writes MP3 to stdout — the
/// reason a live broadcast plays (L10) and the tree never needed a Matroska
/// or MPEG-TS demuxer.
pub const FFMPEG: &str = "ffmpeg";

/// What a queue entry's identity starts with, beside `subsonic://`.
pub const SCHEME: &str = "youtube://";

/// The longest video id accepted: `YouTube`'s own are eleven characters, and a
/// row that carries more is refused rather than handed to the tool.
const LONGEST_ID: usize = 64;

/// The exact length of a video id: `YouTube`'s own are eleven characters, and
/// an input of another length is not one — never a guess.
const ID_LENGTH: usize = 11;

/// The `yt-dlp` on this machine's `PATH`, if there is one.
#[must_use]
pub fn detect() -> Option<PathBuf> {
    detect_in(&std::env::var_os("PATH")?)
}

/// The `ffmpeg` on this machine's `PATH`, if there is one: the chain's second
/// stage, found the same way [`detect`] finds the downloader — a file scan,
/// never a spawn. A machine can have the downloader and not the transcoder,
/// and the missing one is the one the sentence names.
#[must_use]
pub fn detect_ffmpeg() -> Option<PathBuf> {
    detect_ffmpeg_in(&std::env::var_os("PATH")?)
}

/// The `yt-dlp` on the given `PATH` value, if there is one.
///
/// A file scan, never a spawn: launching velo must not cost a process, and a
/// machine without the tool must not pay for looking.
#[must_use]
pub fn detect_in(paths: &OsStr) -> Option<PathBuf> {
    on_paths(paths, executable())
}

/// The `ffmpeg` on the given `PATH` value, if there is one — the same scan,
/// the same rules, the transcoder's own name.
#[must_use]
pub fn detect_ffmpeg_in(paths: &OsStr) -> Option<PathBuf> {
    on_paths(paths, ffmpeg_executable())
}

/// The first executable `program` on the given `PATH` value, if there is one.
fn on_paths(paths: &OsStr, program: &str) -> Option<PathBuf> {
    std::env::split_paths(paths)
        .map(|directory| directory.join(program))
        .find(|candidate| is_executable(candidate))
}

/// Whether `path` is a queue entry from a search result rather than a file.
/// Beside `subsonic::cache::is_pseudo_path`, for the same reason: the art and
/// the scrobble paths must tell one from the other.
#[must_use]
pub fn is_pseudo_path(path: &Path) -> bool {
    path.to_str().is_some_and(|text| text.starts_with(SCHEME))
}

/// The video id inside a `youtube://` entry: `None` when the path is not one,
/// `Err` when it says it is and carries no usable id.
///
/// # Errors
///
/// When the path starts with [`SCHEME`] and what follows is empty, too long, or
/// is not a video id — a broken row is refused, never opened as a file and never
/// handed to the tool.
pub fn video_id(path: &Path) -> Result<Option<&str>, AudioError> {
    let Some(text) = path.to_str() else {
        return Ok(None);
    };
    let Some(id) = text.strip_prefix(SCHEME) else {
        return Ok(None);
    };
    if id.is_empty() {
        return Err(broken(text, "it carries no video id"));
    }
    if id.len() > LONGEST_ID {
        return Err(broken(text, "its video id is too long"));
    }
    if !id
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '-' || character == '_')
    {
        return Err(broken(
            text,
            "its video id holds something a video id cannot",
        ));
    }
    Ok(Some(id))
}

/// The identity a search result carries into the queue, the history and the
/// favourites: `youtube://<id>`.
#[must_use]
pub fn pseudo_path(id: &str) -> PathBuf {
    PathBuf::from(format!("{SCHEME}{id}"))
}

/// The video id a pasted `YouTube` URL, the app's own identity, or a bare id
/// names: `None` for anything else — never a guess.
///
/// What a person actually pastes is accepted:
/// `https://www.youtube.com/watch?v=<id>` (with any extra query parameters,
/// `&list=` and `&t=` among them), `https://youtu.be/<id>`,
/// `https://www.youtube.com/shorts/<id>`, `/embed/<id>`, `/live/<id>`,
/// `youtube://<id>` (the app's own identity, pasted back), a scheme-less
/// `youtube.com/…` paste, and a bare eleven-character video id. The scheme
/// and the host fold case — `HTTPS://` and `YouTube.com` are what a person
/// pastes — while the path and the id stay case-sensitive, because an id
/// is. Anything else — a search phrase, another site, a playlist link, an
/// id of another length — is `None`.
#[must_use]
pub fn input_video_id(text: &str) -> Option<String> {
    let text = text.trim();
    // The app's own identity pasted back, or a bare id.
    if let Some(id) = text.strip_prefix(SCHEME) {
        return is_video_id(id).then(|| id.to_owned());
    }
    if is_video_id(text) {
        return Some(String::from(text));
    }
    // A pasted URL: scheme (optional — a search-field paste drops it),
    // host, path segments, query. Scheme and host fold case (R3-003):
    // `HTTPS://youtu.be/…` and `YouTube.com/watch?v=…` are what a person
    // pastes, and a URL is not matched against what a browser would
    // lowercase for them. The path and the id do not fold: an id is.
    let rest = match text.split_once("://") {
        Some((scheme, rest))
            if scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("http") =>
        {
            rest
        }
        Some(_) => return None,
        None => text,
    };
    let (authority, tail) = rest.split_once('/')?;
    let (path, query) = tail.split_once('?').unwrap_or((tail, ""));
    let host = authority.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let mut segments = path.split('/');
    let id = match (host, segments.next(), segments.next()) {
        ("youtu.be", Some(id), None) => id,
        ("youtube.com", Some("watch"), None) => query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .find_map(|(name, value)| (name == "v").then_some(value))?,
        ("youtube.com", Some("shorts" | "embed" | "live"), Some(id))
            if segments.next().is_none() =>
        {
            id
        }
        _ => return None,
    };
    is_video_id(id).then(|| id.to_owned())
}

/// Whether `text` is exactly one video id: eleven characters, the letters,
/// digits, `-` and `_` a video id holds, and nothing more.
fn is_video_id(text: &str) -> bool {
    text.len() == ID_LENGTH
        && text
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

fn broken(text: &str, why: &str) -> AudioError {
    AudioError::Source(format!("{text} is not a YouTube entry: {why}"))
}

/// The resolver a machine without the tools gets (S10): local files pass
/// through untouched, and a `youtube://` row fails with the sentence that
/// says how to get whichever one is missing — so the config, the queue and
/// the history stay valid and only `YouTube` is refused.
#[derive(Debug)]
pub struct Unconfigured {
    message: String,
}

impl Unconfigured {
    /// The resolver for a machine whose tool is missing.
    #[must_use]
    pub fn new(program: &Path) -> Self {
        Self {
            message: tool::ToolError::NotInstalled(program.to_path_buf()).to_string(),
        }
    }

    /// The sentence every `youtube://` row fails with.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl PathResolver for Unconfigured {
    fn resolve(&self, path: &Path) -> Result<PathBuf, AudioError> {
        if is_pseudo_path(path) {
            // Source, not Io: the sentence is a refusal, not a file read —
            // "cannot read the file" would name something that never happened.
            return Err(AudioError::Source(self.message.clone()));
        }
        Ok(path.to_path_buf())
    }

    /// A `YouTube` row is never already reachable: it has to be resolved to be
    /// played, which is what sends it to the resolver's worker and what makes
    /// the missing tool a status line instead of a failed file open.
    fn cached(&self, path: &Path) -> bool {
        !is_pseudo_path(path)
    }
}

/// The downloader's file name on this platform.
#[cfg(unix)]
const fn executable() -> &'static str {
    PROGRAM
}

/// The downloader's file name on this platform.
#[cfg(not(unix))]
const fn executable() -> &'static str {
    "yt-dlp.exe"
}

/// The transcoder's file name on this platform.
#[cfg(unix)]
const fn ffmpeg_executable() -> &'static str {
    FFMPEG
}

/// The transcoder's file name on this platform.
#[cfg(not(unix))]
const fn ffmpeg_executable() -> &'static str {
    "ffmpeg.exe"
}

/// Whether `path` is a program this machine would run: a file, and executable.
#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.metadata()
        .is_ok_and(|data| data.is_file() && data.permissions().mode() & 0o111 != 0)
}

/// Whether `path` is a program this machine would run: a file.
#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}
