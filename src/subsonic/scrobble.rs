//! Scrobbling (T12d): telling the server what is playing, and what counted.
//!
//! Two pieces live here:
//!
//! - [`Scrobbler`], the [`Scrobbles`] sink over one [`Client`]: every call
//!   is one `scrobble` request, fired from a background thread by the
//!   interface. A failure is swallowed — a scrobble is a courtesy to the
//!   server's play history, never something playback waits on or the user
//!   is told about.
//! - [`ScrobbleTracker`], the decision the interface feeds once per tick:
//!   a track change on a remote row announces it as now playing
//!   (`submission=false`), and once it has played long enough to count, one
//!   `submission=true` goes out.
//!
//! The counting rule is the one Subsonic-compatible servers themselves
//! apply: a track counts as played after **half its duration or four
//! minutes, whichever comes first**. A track whose duration is unknown
//! counts at four minutes. Local tracks are never scrobbled.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use super::client::{Client, Transport};

/// The scrobble sink the interface holds behind an [`Arc`]: the two calls
/// the Subsonic play history is made of, already fire-and-forget — an
/// implementation must not panic and must not surface a failure.
pub trait Scrobbles: Send + Sync {
    /// `scrobble?id=<id>&submission=false`: the track is now playing.
    fn now_playing(&self, remote_id: &str);
    /// `scrobble?id=<id>&submission=true`: the track counted as played.
    fn submit(&self, remote_id: &str);
}

/// The [`Scrobbles`] over one configured server. Every call is one request;
/// every failure vanishes.
pub struct Scrobbler<T> {
    client: Client<T>,
}

impl<T: Transport> Scrobbler<T> {
    /// A scrobbler for one server, sharing the transport the resolver's
    /// client was built over.
    #[must_use]
    pub fn new(client: Client<T>) -> Self {
        Self { client }
    }
}

impl<T: Transport + Send + Sync> Scrobbles for Scrobbler<T> {
    fn now_playing(&self, remote_id: &str) {
        // Fire and forget: a failing scrobble is never shown and never
        // logged loud — playback must not care.
        let _ = self.client.scrobble(remote_id, false);
    }

    fn submit(&self, remote_id: &str) {
        let _ = self.client.scrobble(remote_id, true);
    }
}

/// The four-minute cap of the counting rule, in seconds.
const CAP_SECS: u64 = 240;

/// The rule a track counts as played after: half its duration or four
/// minutes, whichever comes first — the rule Subsonic-compatible servers
/// themselves apply. A duration the server did not say (or said as zero)
/// counts at the cap.
#[must_use]
pub fn threshold(duration: Option<Duration>) -> Duration {
    let cap = Duration::from_secs(CAP_SECS);
    match duration {
        Some(duration) if duration > Duration::ZERO => duration / 2,
        _ => cap,
    }
    .min(cap)
}

/// The song id of one remote queue path, `subsonic://<host>/<id>`; `None`
/// for a local file or a broken row.
fn remote_id(path: &Path) -> Option<&str> {
    let rest = path.to_str()?.strip_prefix("subsonic://")?;
    let (_, id) = rest.split_once('/')?;
    (!id.is_empty()).then_some(id)
}

/// The decision half of scrobbling, fed once per tick with the playing
/// path, the position and the duration. It remembers which track was last
/// seen so a track change is seen with one comparison, and whether the
/// playing track has already counted.
pub struct ScrobbleTracker {
    sink: Arc<dyn Scrobbles>,
    path: Option<PathBuf>,
    submitted: bool,
}

impl ScrobbleTracker {
    /// A tracker feeding `sink`.
    #[must_use]
    pub fn new(sink: Arc<dyn Scrobbles>) -> Self {
        Self {
            sink,
            path: None,
            submitted: false,
        }
    }

    /// One tick of the playing state. A path that changed announces the new
    /// remote track as now playing and re-arms the threshold; a position
    /// past [`threshold`] submits the playing remote track, once. A local
    /// path sends nothing, and a stopped player resets the memory so the
    /// same track announced again later is announced again.
    pub fn on_tick(&mut self, path: Option<&Path>, position: Duration, duration: Option<Duration>) {
        let Some(path) = path else {
            if self.path.take().is_some() {
                self.submitted = false;
            }
            return;
        };
        let Some(id) = remote_id(path) else {
            // A local file — or a broken remote row, which scrobbles nothing
            // rather than scrobbing nonsense.
            self.path = Some(path.to_path_buf());
            self.submitted = false;
            return;
        };
        if self.path.as_deref() != Some(path) {
            self.path = Some(path.to_path_buf());
            self.submitted = false;
            self.sink.now_playing(id);
        }
        if !self.submitted && position >= threshold(duration) {
            self.sink.submit(id);
            self.submitted = true;
        }
    }
}
