//! The on-disk cache of remote tracks (T12c).
//!
//! Playback is cache-on-play: the original bytes are fetched once with
//! `stream?id=<id>&format=raw` — no transcoding, so what lands on disk is
//! what the server holds — and then played from that file. Seek, gapless,
//! bit-perfect and `ReplayGain`-from-tags keep working with no change to the
//! audio path, because the producer opens a file either way.
//!
//! Three things live here:
//!
//! - the directory: [`cache_dir`] resolves `$XDG_CACHE_HOME/velo/subsonic`,
//!   else `~/.cache/velo/subsonic`, from the environment values **passed as
//!   arguments** (this repo forbids `set_var` in tests);
//! - the filename: [`Cache::path_for`] hashes the stable stream URL — the
//!   URL without any credentials — so the same track always lands on the
//!   same file and two different tracks never share one;
//! - the fetch: [`Cache::ensure`] returns a hit without calling the
//!   transport and downloads a miss into place atomically.
//!
//! The same rule serves the covers cache (T12d): [`Cache::covers_cache`]
//! derives the sibling `…/velo/covers` directory, and [`CoverArt`] (on
//! [`SubsonicResolver`]) fetches `getCoverArt` into it — one image file per
//! cover id, a hit never calling the transport.
//!
//! [`SubsonicResolver`] ties the three together behind the player's
//! [`PathResolver`] seam: it parses a queue entry `subsonic://<host>/<id>`,
//! passes every other path through untouched (a queue can mix sources,
//! T12b), and downloads on a miss. A queue entry is resolved **when the
//! producer reaches it**, so a cache hit costs no network and a failed
//! download is a skip the producer announces — the queue continues (T19).
//!
//! The cache is never pruned: every track ever played stays until the user
//! deletes the directory. Nothing in velo grows without bound on purpose;
//! eviction is deliberately out of scope for T12c.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use md5::{Digest as _, Md5};

use crate::audio::AudioError;
use crate::audio::output::PathResolver;

use super::Error;
use super::client::{Client, Transport};

/// Resolves the cache directory from the two environment values the binary
/// owns: `$XDG_CACHE_HOME/velo/subsonic` when `xdg_cache_home` is set and
/// not empty, else `$HOME/.cache/velo/subsonic`. `None` when neither value
/// is usable — with no home there is no place a cache belongs.
#[must_use]
pub fn cache_dir(xdg_cache_home: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    let from_xdg = xdg_cache_home
        .filter(|value| !value.is_empty())
        .map(|value| Path::new(value).join("velo").join("subsonic"));
    from_xdg.or_else(|| {
        home.filter(|value| !value.is_empty()).map(|value| {
            Path::new(value)
                .join(".cache")
                .join("velo")
                .join("subsonic")
        })
    })
}

/// Whether `path` is a queue entry from a synced remote row
/// (`subsonic://<host>/<id>`, T12b) rather than a file. Shared with the
/// interface, whose art and scrobble paths must tell a remote row from a
/// file for exactly the same reason this module does.
#[must_use]
pub fn is_pseudo_path(path: &Path) -> bool {
    path.to_str()
        .is_some_and(|text| text.starts_with("subsonic://"))
}

/// Splits `subsonic://<host>/<id>` into its host and song id. `None` when the
/// path is not a pseudo path; [`Err`] when it looks like one but does not
/// parse — a broken row is refused, never opened as a file.
fn parse_pseudo_path(path: &Path) -> Result<Option<(String, String)>, AudioError> {
    let Some(text) = path.to_str() else {
        return Ok(None);
    };
    let Some(rest) = text.strip_prefix("subsonic://") else {
        return Ok(None);
    };
    let Some((host, id)) = rest.split_once('/') else {
        return Err(AudioError::Io(std::io::Error::other(format!(
            "{text} is not a subsonic track: it carries no song id"
        ))));
    };
    if host.is_empty() || id.is_empty() {
        return Err(AudioError::Io(std::io::Error::other(format!(
            "{text} is not a subsonic track: host and song id must both be there"
        ))));
    }
    Ok(Some((host.to_owned(), id.to_owned())))
}

/// The stable, credential-free stream URL one song id caches under: the
/// request velo will make, minus the parameters that change per request.
/// Hashing this keeps the same track on the same file across sessions.
fn stream_url(base_url: &str, id: &str) -> String {
    format!(
        "{}/rest/stream?id={id}&format=raw",
        base_url.trim_end_matches('/')
    )
}

/// Maps a Subsonic failure onto the audio path's own error type, the only
/// error the producer's skip path carries. The server's or the store's
/// sentence rides after the audio prefix, which is what the interface
/// announces.
fn as_audio_error(error: &Error) -> AudioError {
    AudioError::Io(std::io::Error::other(error.to_string()))
}

/// The directory, the filename rule and the fetch, in one value.
#[derive(Debug, Clone)]
pub struct Cache {
    dir: PathBuf,
}

impl Cache {
    /// A cache rooted at `dir` — [`cache_dir`] for the real one, a
    /// throwaway directory for a test.
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// The file a stream URL caches into: the md5 of the stable URL, hex.
    /// The same URL yields the same name twice; two different URLs never
    /// collide. No extension is needed — decoding probes the bytes, not the
    /// name — and none is given, because the server's own naming says
    /// nothing trustworthy about the format anyway.
    #[must_use]
    pub fn path_for(&self, url: &str) -> PathBuf {
        let mut hasher = Md5::new();
        hasher.update(url.as_bytes());
        self.dir.join(format!("{:x}", hasher.finalize()))
    }

    /// The cached file for `url`, downloaded with `fetch` when it is not
    /// there yet. A hit calls nothing: the bytes are already on disk. The
    /// download lands through a temporary file renamed into place, so a
    /// failed or interrupted fetch never leaves a half-written track behind.
    ///
    /// # Errors
    ///
    /// The fetch's own [`Error`] when the download fails, and one honest
    /// sentence when the file cannot be written.
    pub fn ensure(
        &self,
        url: &str,
        fetch: impl FnOnce() -> Result<Vec<u8>, Error>,
    ) -> Result<PathBuf, Error> {
        let path = self.path_for(url);
        if path.is_file() {
            return Ok(path);
        }
        let bytes = fetch()?;
        self.store(&path, &bytes)?;
        Ok(path)
    }

    /// Writes the fetched bytes into place: create the directory, write a
    /// temporary sibling, rename it over the final name.
    fn store(&self, path: &Path, bytes: &[u8]) -> Result<(), Error> {
        fs::create_dir_all(&self.dir).map_err(|error| {
            Error::Config(format!(
                "cannot create the cache directory {}: {error}",
                self.dir.display()
            ))
        })?;
        let temp = self
            .dir
            .join(format!(".part-{}-{}", std::process::id(), temp_counter()));
        let written = fs::write(&temp, bytes).and_then(|()| fs::rename(&temp, path));
        if written.is_err() {
            let _ = fs::remove_file(&temp);
        }
        written.map_err(|error| {
            Error::Config(format!(
                "cannot write the cached file {}: {error}",
                path.display()
            ))
        })
    }

    /// The cache of cover art that sits beside this one: `$XDG_CACHE_HOME/
    /// velo/covers` (else `~/.cache/velo/covers`) when this is the track
    /// cache under `…/velo/subsonic`. Same rule, same stable-hash names, its
    /// own directory — a cover never lands among the track bytes (T12d).
    #[must_use]
    pub fn covers_cache(&self) -> Self {
        Self {
            dir: self.dir.with_file_name("covers"),
        }
    }
}

/// One more unique temporary name per call, so two downloads never race onto
/// the same `.part-…` file.
fn temp_counter() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// The player's resolver for one configured server: a queue entry
/// `subsonic://<host>/<id>` becomes the cached file of its original bytes;
/// every other entry passes through untouched. Built once by the binary and
/// handed to the interface, which hands it to the player.
#[derive(Debug)]
pub struct SubsonicResolver<T> {
    client: Client<T>,
    /// The index source this resolver serves, `subsonic:<host>` — the same
    /// string the sync (T12b) marks its rows with, so a host comparison is
    /// exactly the row-to-server match.
    source: String,
    cache: Cache,
    /// The cover-art cache beside `cache` (T12d): one image file per cover
    /// id, fetched once.
    covers: Cache,
}

impl<T: Transport> SubsonicResolver<T> {
    /// A resolver for one server. `source` is what [`super::super`]
    /// `library::source_for` derives from the same base URL, so the
    /// pseudo-path host and this source always agree for synced rows.
    #[must_use]
    pub fn new(client: Client<T>, source: String, cache: Cache) -> Self {
        Self {
            client,
            source,
            covers: cache.covers_cache(),
            cache,
        }
    }
}

impl<T: Transport + Send + Sync> PathResolver for SubsonicResolver<T> {
    fn resolve(&self, path: &Path) -> Result<PathBuf, AudioError> {
        let Some((host, id)) = parse_pseudo_path(path)? else {
            // A local file — the queue can mix sources (T12b) — is already
            // the file to open, and costs no request.
            return Ok(path.to_path_buf());
        };
        if format!("subsonic:{host}") != self.source {
            return Err(AudioError::Io(std::io::Error::other(format!(
                "{host} is not the configured subsonic server ({})",
                self.source
            ))));
        }
        let url = stream_url(self.client.base_url(), &id);
        self.cache
            .ensure(&url, || self.client.stream_bytes(&id))
            .map_err(|error| as_audio_error(&error))
    }
}

/// The cover-art seam the interface draws through (T12d): one `cover_id` in,
/// the cached image file out. `None` on any failure — a cover is decoration,
/// and a missing or slow one means no cover, never an error the user sees.
/// The interface calls this off its drawing path, on a background thread.
pub trait CoverArt: Send + Sync {
    /// The cached file of one cover id, fetched once from the server.
    fn cover_file(&self, cover_id: &str) -> Option<PathBuf>;
}

impl<T: Transport + Send + Sync> CoverArt for SubsonicResolver<T> {
    fn cover_file(&self, cover_id: &str) -> Option<PathBuf> {
        // The covers cache keys on the cover id itself, so the same id is
        // always the same file and a hit never calls the transport.
        self.covers
            .ensure(cover_id, || self.client.cover_art_bytes(cover_id))
            .ok()
    }
}

/// The resolver for a server whose credentials could not be resolved (T12a):
/// every remote track is skipped with the credential sentence and no request
/// is ever sent — the queue keeps going, and local files still play. What a
/// half-configured `[subsonic]` table gets, instead of an interface that
/// refuses to start.
#[derive(Debug, Clone)]
pub struct Unconfigured {
    message: String,
}

impl Unconfigured {
    /// A resolver whose remote tracks all fail with `message`.
    #[must_use]
    pub fn new(message: String) -> Self {
        Self { message }
    }
}

impl PathResolver for Unconfigured {
    fn resolve(&self, path: &Path) -> Result<PathBuf, AudioError> {
        if is_pseudo_path(path) {
            return Err(AudioError::Io(std::io::Error::other(self.message.clone())));
        }
        Ok(path.to_path_buf())
    }
}
