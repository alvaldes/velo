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
//! The two caches are bounded. The track cache holds at most
//! [`Cache::TRACK_CACHE_CAP`] (2 GiB) and the covers cache
//! [`Cache::COVERS_CACHE_CAP`] (256 MiB). The cap is enforced **after** a
//! successful store, never before: the whole directory is walked (one flat
//! `read_dir`, no recursion — the caches are flat by construction), and when
//! the total is over the cap, whole files are evicted least-recently-used by
//! modification time until it is under again. The one file that may never be
//! evicted is the one just stored — eviction that removed it would re-download
//! it on the next play, a loop rather than a policy — so a single file larger
//! than the cap is kept anyway. `velo --cache-clean` empties both caches on
//! demand through [`cache::clean`].

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

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
    /// The byte cap enforced after every store, when there is one: over it,
    /// the oldest files (by modification time) are evicted, never the file
    /// just stored. `None` leaves the cache unbounded — the shape tests use;
    /// the binary bounds both caches.
    cap: Option<u64>,
}

impl Cache {
    /// The track cache's cap: 2 GiB of original bytes.
    pub const TRACK_CACHE_CAP: u64 = 2 * 1024 * 1024 * 1024;
    /// The covers cache's cap: 256 MiB of images.
    pub const COVERS_CACHE_CAP: u64 = 256 * 1024 * 1024;

    /// A cache rooted at `dir` — [`cache_dir`] for the real one, a
    /// throwaway directory for a test. Unbounded until [`Cache::with_cap`]
    /// bounds it.
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self { dir, cap: None }
    }

    /// Bounds the cache: after every successful store, when the directory
    /// holds more than `cap` bytes, the oldest files are evicted until it
    /// does not — except the file just stored, which survives even when it
    /// alone is over the cap.
    #[must_use]
    pub fn with_cap(mut self, cap: u64) -> Self {
        self.cap = Some(cap);
        self
    }

    /// The directory this cache stores into.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
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
    /// temporary sibling, rename it over the final name. The cap, when the
    /// cache has one, is enforced after the rename — never on a failed
    /// store, and never by evicting the file just written.
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
        })?;
        self.prune(path);
        Ok(())
    }

    /// Enforces the cap after a successful store: one flat walk of the
    /// directory (no recursion — the caches are flat by construction), and
    /// when the total is over the cap, whole files are removed oldest-first
    /// by modification time until it is under again. The file just stored is
    /// never a candidate: it is what the next play needs, and removing it
    /// would turn eviction into a re-download loop. A single file larger
    /// than the cap is therefore kept. Files whose metadata cannot be read
    /// are skipped, not guessed at. A walk or an eviction that fails is
    /// silent: the cache is a cache, and an unpruned cache is still correct.
    fn prune(&self, keep: &Path) {
        let Some(cap) = self.cap else {
            return;
        };
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        let mut files: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
        let mut total: u64 = 0;
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            total = total.saturating_add(metadata.len());
            files.push((
                entry.path(),
                metadata.len(),
                metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            ));
        }
        if total <= cap {
            return;
        }
        // Oldest first; the path breaks mtime ties so the order is stable.
        files.sort_by(|a, b| (a.2, &a.0).cmp(&(b.2, &b.0)));
        for (path, size, _) in files {
            if total <= cap {
                break;
            }
            if path == keep {
                continue;
            }
            if fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(size);
            }
        }
    }

    /// The cache of cover art that sits beside this one: `$XDG_CACHE_HOME/
    /// velo/covers` (else `~/.cache/velo/covers`) when this is the track
    /// cache under `…/velo/subsonic`. Same rule, same stable-hash names, its
    /// own directory — a cover never lands among the track bytes (T12d) —
    /// and its own, smaller cap ([`Cache::COVERS_CACHE_CAP`]).
    #[must_use]
    pub fn covers_cache(&self) -> Self {
        Self {
            dir: self.dir.with_file_name("covers"),
            cap: Some(Self::COVERS_CACHE_CAP),
        }
    }
}

/// Removes every file directly inside `dir` — the engine behind
/// `velo --cache-clean`, called once per cache. The directory itself is
/// created when it does not exist and never removed, files inside
/// subdirectories are not touched, and a file that cannot be removed is
/// skipped rather than reported as freed. Nothing outside `dir` is ever
/// read or written. Answers how many files went and how many bytes they
/// held.
#[must_use]
pub fn clean(dir: &Path) -> (usize, u64) {
    if fs::create_dir_all(dir).is_err() {
        return (0, 0);
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return (0, 0);
    };
    let mut files = 0;
    let mut bytes = 0;
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        if fs::remove_file(entry.path()).is_ok() {
            files += 1;
            bytes += metadata.len();
        }
    }
    (files, bytes)
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

/// Whether `path` is an absolute `http(s)://` URL: a remote song row from a
/// configured radio channel (cliamp S2) — neither a local file nor a
/// `subsonic://` row.
#[must_use]
pub fn is_remote_url(path: &Path) -> bool {
    path.to_str().is_some_and(|text| {
        let lower = text.to_ascii_lowercase();
        lower.starts_with("http://") || lower.starts_with("https://")
    })
}

/// A transport handed to the remote-song resolver by value: `Arc<T>` keeps
/// the test's counting handle beside the resolver, the way the `&T` impl
/// lets the Subsonic client's tests borrow theirs.
impl<T: Transport + ?Sized> Transport for Arc<T> {
    fn get(&self, url: &str) -> Result<Vec<u8>, Error> {
        (**self).get(url)
    }
}

/// The player's resolver for remote song URLs (cliamp S2, S3 — the human's
/// decision: cache like Subsonic, reusing the resolver). An `http(s)://`
/// queue entry — a configured radio channel's song — downloads into the same
/// track cache a Subsonic server's songs use (a hit costs no network, a miss
/// downloads once) and resolves to that file, so seek, gapless and
/// `ReplayGain` behave as they do for any local file, and `--cache-clean`
/// cleans what it writes. Every other entry follows the wrapped resolver —
/// the configured server's own, whatever its disposition — or, without one,
/// passes through untouched.
pub struct RemoteResolver<T> {
    transport: T,
    cache: Cache,
    /// Where every non-remote path goes: the server's own resolver when one
    /// is configured, or a plain pass-through.
    fallback: Option<Arc<dyn PathResolver>>,
}

impl<T: Transport> RemoteResolver<T> {
    /// A resolver over one transport and the shared track cache.
    #[must_use]
    pub fn new(transport: T, cache: Cache, fallback: Option<Arc<dyn PathResolver>>) -> Self {
        Self {
            transport,
            cache,
            fallback,
        }
    }

    /// The cached file of `path` when it is a remote URL, `None` for any
    /// other entry.
    ///
    /// # Errors
    ///
    /// When a remote URL cannot be fetched or stored: the producer announces
    /// the failure and the queue keeps going (T19).
    pub fn resolve_remote(&self, path: &Path) -> Result<Option<PathBuf>, AudioError> {
        if !is_remote_url(path) {
            return Ok(None);
        }
        // `is_remote_url` saw the text, so it is there and it is a URL.
        let url = path.to_str().unwrap_or_default();
        self.cache
            .ensure(url, || self.transport.get(url))
            .map(Some)
            .map_err(|error| as_audio_error(&error))
    }
}

impl<T: Transport + Send + Sync> PathResolver for RemoteResolver<T> {
    fn resolve(&self, path: &Path) -> Result<PathBuf, AudioError> {
        if let Some(resolved) = self.resolve_remote(path)? {
            return Ok(resolved);
        }
        match &self.fallback {
            Some(resolver) => resolver.resolve(path),
            None => Ok(path.to_path_buf()),
        }
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
