//! The update check (issue #69, slice 1): know that a newer velo exists.
//!
//! On launch, honouring `[update] check` (`never`, `daily` or `weekly`,
//! `daily` the default), one background thread asks GitHub for the newest
//! release tag and compares it with the running version. The comparison and
//! the interval decision are pure functions over injected values — a clock,
//! a stored timestamp, the running version, the fetched tag — so they are
//! testable without a network or a sleep.
//!
//! Everything is silent on failure: no retry storm, no error line. The
//! worker does the asking only; the event-loop side, which also owns every
//! other config write, records `[update] last_check` — the unix timestamp
//! of the last check — when it consumes the answer. Whatever the answer
//! was, the timestamp is recorded, so a failed check waits a whole interval
//! like a successful one. Nothing is
//! ever downloaded, verified or executed: replacing velo's own binary is
//! slice 2's work, out of scope here.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, SystemTime};

use miniserde::Deserialize;

use crate::radio::client::Transport;

/// The endpoint the check asks (issue #69): GitHub's "latest release" for
/// velo, whose `tag_name` is the newest published version.
pub const RELEASES_URL: &str = "https://api.github.com/repos/alvaldes/velo/releases/latest";

/// The interval a `daily` check waits between checks.
pub const DAILY_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// The interval a `weekly` check waits between checks.
pub const WEEKLY_INTERVAL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// The version of velo that is running (issue #69): what the fetched tag is
/// compared against, and what the settings card's `version` row names.
pub const RUNNING_VERSION: &str = env!("CARGO_PKG_VERSION");

/// How often velo may ask whether a newer one is out (issue #69): `daily`
/// is the default, `never` means no request at all. The way an unknown
/// value is handled follows the file's convention for an enum-valued key —
/// the same strictness `replay_gain` is read with: serde's own
/// unknown-variant error, which names the value it refused and every
/// allowed one, and fails the load with the file and the line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckMode {
    /// Ask at most once a day (the default).
    #[default]
    Daily,
    /// Ask at most once a week.
    Weekly,
    /// Never ask; no request at all.
    Never,
}

impl CheckMode {
    /// The interval the mode waits between checks; `never` waits forever.
    #[must_use]
    pub const fn interval(self) -> Option<Duration> {
        match self {
            Self::Daily => Some(DAILY_INTERVAL),
            Self::Weekly => Some(WEEKLY_INTERVAL),
            Self::Never => None,
        }
    }
}

/// Whether a check is due now (issue #69): pure over the injected values.
/// `never` is never due; a mode with no stored timestamp is due at once —
/// a fresh install is asked nothing, but its first launch does check;
/// otherwise the check is due when the stored timestamp is a whole
/// interval old. A clock that appears to have moved backwards is not due:
/// a surprise request buys nothing.
#[must_use]
pub fn check_due(mode: CheckMode, now: SystemTime, last_check: Option<SystemTime>) -> bool {
    let Some(interval) = mode.interval() else {
        return false;
    };
    let Some(last) = last_check else {
        return true;
    };
    match now.duration_since(last) {
        Ok(elapsed) => elapsed >= interval,
        Err(_) => false,
    }
}

/// The version triple of `text`, with or without a leading `v`: up to three
/// dot-separated numbers — a missing part is zero, a part that is not a
/// number, a fourth part or no numbers at all is no version.
#[must_use]
pub fn version_triple(text: &str) -> Option<(u64, u64, u64)> {
    let trimmed = text.trim();
    let digits = trimmed.strip_prefix('v').unwrap_or(trimmed);
    if digits.is_empty() {
        return None;
    }
    let mut parts = [0_u64; 3];
    for (index, part) in digits.split('.').enumerate() {
        if index >= 3 {
            return None;
        }
        parts[index] = part.parse().ok()?;
    }
    Some((parts[0], parts[1], parts[2]))
}

/// Whether `fetched` is a newer version than `running` (issue #69): both
/// read as a semver triple, a leading `v` allowed, and only a strictly
/// newer fetched version answers yes — never a downgrade, never an equal
/// version, and a tag that is not a version answers no without panicking.
#[must_use]
pub fn newer_version(running: &str, fetched: &str) -> bool {
    match (version_triple(running), version_triple(fetched)) {
        (Some(running), Some(fetched)) => fetched > running,
        _ => false,
    }
}

/// The display form of a release tag: the leading `v` off, so the status
/// line and the settings row name versions the way the running version is
/// named.
#[must_use]
pub fn display_version(tag: &str) -> &str {
    let trimmed = tag.trim();
    trimmed.strip_prefix('v').unwrap_or(trimmed)
}

/// The one field of the releases answer velo reads; every other field of
/// the JSON is ignored, never an error.
#[derive(Deserialize)]
struct Release {
    tag_name: String,
}

/// Asks `transport` for the newest release tag (issue #69). `None` on any
/// failure — a refused request, a status, a body that is not the JSON, a
/// tag that is not there — because the caller is silent about them all.
#[must_use]
pub fn fetch_latest_tag(transport: &dyn Transport) -> Option<String> {
    let body = transport.get(RELEASES_URL).ok()?;
    let release: Release = miniserde::json::from_str(&body).ok()?;
    let tag = release.tag_name;
    (!tag.trim().is_empty()).then_some(tag)
}

/// What a finished check says (issue #69): the tag it fetched, or nothing —
/// a failure is not an error line, it is the check having said nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    /// The check could not ask, or could not read the answer: silent.
    Failed,
    /// The newest tag on the releases page, `v` prefix and all.
    Tag(String),
}

/// The interface's handle on the one-shot check (issue #69): its thread
/// runs off the event loop and [`Self::poll`] never blocks.
#[derive(Debug)]
pub struct UpdateCheck {
    outcome: Receiver<CheckOutcome>,
}

impl UpdateCheck {
    /// Starts the check on its own thread (issue #69). The thread does the
    /// network only: it asks through `transport`, decides the answer, and
    /// sends it; it never touches the config file. The timestamp of the
    /// check is the consumer's to record — the event-loop side records it
    /// when it takes the answer out of the channel, so the config file has
    /// one writer, the same one every user setting has.
    ///
    /// # Errors
    ///
    /// Fails when the thread cannot be spawned. The caller is silent about
    /// it (issue #69): a launch without the check is a launch all the same.
    pub fn spawn(transport: Arc<dyn Transport + Send + Sync>) -> Result<Self, String> {
        let (sender, receiver): (Sender<CheckOutcome>, Receiver<CheckOutcome>) = mpsc::channel();
        let worker = move || {
            let outcome = match fetch_latest_tag(transport.as_ref()) {
                Some(tag) => CheckOutcome::Tag(tag),
                None => CheckOutcome::Failed,
            };
            let _ = sender.send(outcome);
        };
        std::thread::Builder::new()
            .name("velo-update-check".to_owned())
            .spawn(worker)
            .map(|_| Self { outcome: receiver })
            .map_err(|error| format!("cannot start the update-check thread: {error}"))
    }

    /// What the check said, once it has said anything. Never blocks; `None`
    /// while the thread is still running. The answer is read once: the
    /// check is one-shot, and the caller drops the handle after it.
    pub fn poll(&mut self) -> Option<CheckOutcome> {
        self.outcome.try_recv().ok()
    }

    /// A pair wired to a plain channel instead of a thread: the tests
    /// inject the outcome and observe what the tick does with it, without
    /// a network. Production never calls this, which is why it is hidden
    /// from the docs.
    #[doc(hidden)]
    #[must_use]
    pub fn loopback() -> (Self, Sender<CheckOutcome>) {
        let (sender, receiver) = mpsc::channel();
        (Self { outcome: receiver }, sender)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    fn unix(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    // ---- the interval decision (issue #69) -----------------------------

    #[test]
    fn never_is_never_due_even_without_a_timestamp() {
        let now = unix(1_000_000);
        assert!(!check_due(CheckMode::Never, now, None));
        assert!(!check_due(CheckMode::Never, now, Some(unix(0))));
    }

    #[test]
    fn a_daily_check_is_due_only_after_a_whole_day() {
        let now = unix(100_000);
        // No stored timestamp: the first check is due at once.
        assert!(check_due(CheckMode::Daily, now, None));
        // Not yet a day: not due.
        assert!(!check_due(
            CheckMode::Daily,
            unix(100_000 + 24 * 60 * 60 - 1),
            Some(now)
        ));
        // A whole day old: due.
        assert!(check_due(
            CheckMode::Daily,
            unix(100_000 + 24 * 60 * 60),
            Some(now)
        ));
        // Long past it: due.
        assert!(check_due(
            CheckMode::Daily,
            unix(100_000 + 9 * 24 * 60 * 60),
            Some(now)
        ));
    }

    #[test]
    fn a_weekly_check_is_due_only_after_a_whole_week() {
        let now = unix(100_000);
        assert!(check_due(CheckMode::Weekly, now, None));
        assert!(!check_due(
            CheckMode::Weekly,
            unix(100_000 + 7 * 24 * 60 * 60 - 1),
            Some(now)
        ));
        assert!(check_due(
            CheckMode::Weekly,
            unix(100_000 + 7 * 24 * 60 * 60),
            Some(now)
        ));
    }

    #[test]
    fn a_clock_that_moved_backwards_is_not_due() {
        let last = unix(200_000);
        assert!(!check_due(CheckMode::Daily, unix(100_000), Some(last)));
    }

    // ---- the version comparison (issue #69) ----------------------------

    #[test]
    fn only_a_strictly_newer_version_answers_yes() {
        let running = "0.42.3";
        assert!(newer_version(running, "0.43.0"));
        assert!(newer_version(running, "1.0.0"));
        assert!(newer_version(running, "0.42.4"));
        assert!(!newer_version(running, "0.42.3"), "equal is not newer");
        assert!(!newer_version(running, "0.42.2"), "never a downgrade");
        assert!(!newer_version(running, "0.41.9"), "never a downgrade");
        assert!(!newer_version(running, "0.42"), "a missing part is zero");
        assert!(!newer_version(running, "0.42.3.1"), "not a semver triple");
    }

    #[test]
    fn a_leading_v_on_either_side_changes_nothing() {
        assert!(newer_version("v0.42.3", "v0.43.0"));
        assert!(newer_version("0.42.3", "v0.43.0"));
        assert!(newer_version("v0.42.3", "0.43.0"));
        assert!(!newer_version("v0.42.3", "v0.42.3"));
    }

    #[test]
    fn a_tag_that_is_not_a_version_answers_no_without_panicking() {
        let running = "0.42.3";
        assert!(!newer_version(running, ""));
        assert!(!newer_version(running, "latest"));
        assert!(!newer_version(running, "nightly-2026-01-01"));
        assert!(!newer_version(running, "1.x"));
        assert!(!newer_version(running, "v"));
        assert!(!newer_version("not-a-version", "0.43.0"));
    }

    #[test]
    fn the_display_form_drops_the_leading_v() {
        assert_eq!(display_version("v0.43.0"), "0.43.0");
        assert_eq!(display_version("0.43.0"), "0.43.0");
    }
}
