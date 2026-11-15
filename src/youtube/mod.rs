//! `YouTube`, played through `yt-dlp` (issue #70).
//!
//! `yt-dlp` is not in the repository and never will be: it is an optional
//! external tool, found at launch and absent without a launch failure. velo
//! spawns it, reads its output, and never talks to `YouTube` itself.
//!
//! What a queue entry carries is the video's identity — `youtube://<id>` — and
//! never a stream URL: the URL a resolve extracts expires, so it is fetched
//! when the entry plays, not when it was queued (S4).

pub mod search;
pub mod tool;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::audio::AudioError;
use crate::audio::output::PathResolver;

/// The program this module spawns.
pub const PROGRAM: &str = "yt-dlp";

/// What a queue entry's identity starts with, beside `subsonic://`.
pub const SCHEME: &str = "youtube://";

/// The longest video id accepted: `YouTube`'s own are eleven characters, and a
/// row that carries more is refused rather than handed to the tool.
const LONGEST_ID: usize = 64;

/// The `yt-dlp` on this machine's `PATH`, if there is one.
#[must_use]
pub fn detect() -> Option<PathBuf> {
    detect_in(&std::env::var_os("PATH")?)
}

/// The `yt-dlp` on the given `PATH` value, if there is one.
///
/// A file scan, never a spawn: launching velo must not cost a process, and a
/// machine without the tool must not pay for looking.
#[must_use]
pub fn detect_in(paths: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(paths)
        .map(|directory| directory.join(executable()))
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

fn broken(text: &str, why: &str) -> AudioError {
    AudioError::Io(std::io::Error::other(format!(
        "{text} is not a YouTube entry: {why}"
    )))
}

/// The resolver a machine without `yt-dlp` gets (S10): local files pass
/// through untouched, and a `youtube://` row fails with the sentence that says
/// how to get the tool — so the config, the queue and the history stay valid
/// and only `YouTube` is refused.
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
            return Err(AudioError::Io(std::io::Error::other(self.message.clone())));
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

/// The tool's file name on this platform.
#[cfg(unix)]
const fn executable() -> &'static str {
    PROGRAM
}

/// The tool's file name on this platform.
#[cfg(not(unix))]
const fn executable() -> &'static str {
    "yt-dlp.exe"
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
