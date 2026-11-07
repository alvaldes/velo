//! Scrobbling (T12d): the interface tells the server what is playing, and
//! once a remote track has played long enough to count, that it counted.
//!
//! The decision is one tracker fed once per tick with the playing path, the
//! position and the duration; the sink is the [`Scrobbles`] seam, which the
//! tests record instead of a network. No test here touches the network.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use velo::subsonic::Error;
use velo::subsonic::client::{Auth, Client, Transport};
use velo::subsonic::scrobble::{ScrobbleTracker, Scrobbler, Scrobbles, threshold};
use velo::ui::App;

// ------------------------------------------------------------- infrastructure

/// A sink that records every call as `(id, what)`.
#[derive(Default, Clone)]
struct RecordingSink {
    calls: Arc<Mutex<Vec<(String, &'static str)>>>,
}

impl Scrobbles for RecordingSink {
    fn now_playing(&self, remote_id: &str) {
        self.calls
            .lock()
            .unwrap()
            .push((remote_id.to_owned(), "now-playing"));
    }

    fn submit(&self, remote_id: &str) {
        self.calls
            .lock()
            .unwrap()
            .push((remote_id.to_owned(), "submit"));
    }
}

impl RecordingSink {
    fn calls(&self) -> Vec<(String, &'static str)> {
        self.calls.lock().unwrap().clone()
    }
}

fn tracker(sink: &RecordingSink) -> ScrobbleTracker {
    ScrobbleTracker::new(Arc::new(sink.clone()))
}

/// The queue path of one remote row, as the sync writes it (T12b).
fn remote(id: &str) -> PathBuf {
    PathBuf::from(format!("subsonic://music.example.com/{id}"))
}

/// A local file, as a scanned row's path looks.
fn local() -> PathBuf {
    PathBuf::from("/music/Library/Tide/Harbor/01 - Dawn.wav")
}

// ---------------------------------------------------------------- the rule

#[test]
fn the_threshold_is_half_the_track_or_four_minutes_whichever_comes_first() {
    // A five-minute track counts at half of it.
    assert_eq!(
        threshold(Some(Duration::from_secs(300))),
        Duration::from_secs(150)
    );
    // A twenty-minute track counts at the four-minute cap.
    assert_eq!(
        threshold(Some(Duration::from_secs(1200))),
        Duration::from_secs(240)
    );
    // Exactly four minutes: half of it is shorter, so half it is.
    assert_eq!(
        threshold(Some(Duration::from_secs(240))),
        Duration::from_secs(120)
    );
    // A duration the server did not say: the cap applies.
    assert_eq!(threshold(None), Duration::from_secs(240));
    assert_eq!(threshold(Some(Duration::ZERO)), Duration::from_secs(240));
}

// ------------------------------------------------------------- the tracker

#[test]
fn starting_a_remote_track_tells_the_server_now_playing() {
    let sink = RecordingSink::default();
    let mut tracker = tracker(&sink);

    tracker.on_tick(
        Some(&remote("s-1")),
        Duration::ZERO,
        Some(Duration::from_secs(200)),
    );

    assert_eq!(sink.calls(), [("s-1".to_owned(), "now-playing")]);
}

#[test]
fn crossing_the_threshold_submits_exactly_once() {
    let sink = RecordingSink::default();
    let mut tracker = tracker(&sink);
    // A 200 s track counts at 100 s.
    tracker.on_tick(
        Some(&remote("s-1")),
        Duration::ZERO,
        Some(Duration::from_secs(200)),
    );

    tracker.on_tick(
        Some(&remote("s-1")),
        Duration::from_secs(99),
        Some(Duration::from_secs(200)),
    );
    assert_eq!(
        sink.calls(),
        [("s-1".to_owned(), "now-playing")],
        "not yet: one second to go"
    );

    tracker.on_tick(
        Some(&remote("s-1")),
        Duration::from_secs(100),
        Some(Duration::from_secs(200)),
    );
    assert_eq!(
        sink.calls(),
        [
            ("s-1".to_owned(), "now-playing"),
            ("s-1".to_owned(), "submit"),
        ],
        "the threshold is reached: the track counted"
    );

    // Ticks beyond the threshold never submit a second time.
    for position in [110_u64, 150, 199] {
        tracker.on_tick(
            Some(&remote("s-1")),
            Duration::from_secs(position),
            Some(Duration::from_secs(200)),
        );
    }
    assert_eq!(sink.calls().len(), 2, "exactly one submission");
}

#[test]
fn the_four_minute_cap_submits_at_the_cap() {
    let sink = RecordingSink::default();
    let mut tracker = tracker(&sink);
    tracker.on_tick(
        Some(&remote("s-1")),
        Duration::ZERO,
        Some(Duration::from_secs(1200)),
    );

    tracker.on_tick(
        Some(&remote("s-1")),
        Duration::from_secs(239),
        Some(Duration::from_secs(1200)),
    );
    tracker.on_tick(
        Some(&remote("s-1")),
        Duration::from_secs(240),
        Some(Duration::from_secs(1200)),
    );

    assert_eq!(
        sink.calls(),
        [
            ("s-1".to_owned(), "now-playing"),
            ("s-1".to_owned(), "submit"),
        ]
    );
}

#[test]
fn an_unknown_duration_counts_at_four_minutes() {
    let sink = RecordingSink::default();
    let mut tracker = tracker(&sink);
    tracker.on_tick(Some(&remote("s-1")), Duration::ZERO, None);

    tracker.on_tick(Some(&remote("s-1")), Duration::from_secs(240), None);

    assert_eq!(
        sink.calls(),
        [
            ("s-1".to_owned(), "now-playing"),
            ("s-1".to_owned(), "submit"),
        ]
    );
}

#[test]
fn a_local_track_sends_nothing() {
    let sink = RecordingSink::default();
    let mut tracker = tracker(&sink);

    for position in [0_u64, 60, 120, 240, 600] {
        tracker.on_tick(
            Some(&local()),
            Duration::from_secs(position),
            Some(Duration::from_secs(3600)),
        );
    }

    assert!(sink.calls().is_empty(), "local tracks are never scrobbled");
}

#[test]
fn a_track_change_before_the_threshold_submits_nothing_for_the_old_track() {
    let sink = RecordingSink::default();
    let mut tracker = tracker(&sink);

    tracker.on_tick(
        Some(&remote("s-1")),
        Duration::ZERO,
        Some(Duration::from_secs(200)),
    );
    tracker.on_tick(
        Some(&remote("s-1")),
        Duration::from_secs(10),
        Some(Duration::from_secs(200)),
    );
    tracker.on_tick(
        Some(&remote("s-2")),
        Duration::ZERO,
        Some(Duration::from_secs(200)),
    );

    assert_eq!(
        sink.calls(),
        [
            ("s-1".to_owned(), "now-playing"),
            ("s-2".to_owned(), "now-playing"),
        ]
    );
}

#[test]
fn stopping_and_replaying_the_same_track_announces_it_again() {
    let sink = RecordingSink::default();
    let mut tracker = tracker(&sink);

    tracker.on_tick(
        Some(&remote("s-1")),
        Duration::ZERO,
        Some(Duration::from_secs(200)),
    );
    tracker.on_tick(None, Duration::ZERO, None);
    tracker.on_tick(
        Some(&remote("s-1")),
        Duration::ZERO,
        Some(Duration::from_secs(200)),
    );

    assert_eq!(
        sink.calls(),
        [
            ("s-1".to_owned(), "now-playing"),
            ("s-1".to_owned(), "now-playing"),
        ]
    );
}

// ------------------------------------------------------------ the sink (wire)

/// A transport that records every URL and answers `ok`.
struct Recording {
    urls: Mutex<Vec<String>>,
}

impl Transport for Recording {
    fn get(&self, url: &str) -> Result<Vec<u8>, Error> {
        self.urls.lock().unwrap().push(url.to_owned());
        Ok(br#"{"subsonic-response":{"status":"ok"}}"#.to_vec())
    }
}

/// A transport that always fails, for the swallow test.
struct Broken;

impl Transport for Broken {
    fn get(&self, _url: &str) -> Result<Vec<u8>, Error> {
        Err(Error::Unreachable {
            detail: "connection refused".to_owned(),
        })
    }
}

fn query_pairs(url: &str) -> Vec<String> {
    let query = url.split_once('?').map_or("", |(_, query)| query);
    query.split('&').map(str::to_owned).collect()
}

#[test]
fn now_playing_goes_over_the_wire_as_submission_false() {
    let transport = Recording {
        urls: Mutex::new(Vec::new()),
    };
    let scrobbler = Scrobbler::new(Client::new(
        &transport,
        "https://music.example.com",
        "demo",
        Auth::Token {
            password: "demo".to_owned(),
        },
    ));

    scrobbler.now_playing("s-1");

    let urls = transport.urls.lock().unwrap();
    assert_eq!(urls.len(), 1);
    assert!(
        urls[0].starts_with("https://music.example.com/rest/scrobble?"),
        "{}",
        urls[0]
    );
    let pairs = query_pairs(&urls[0]);
    for piece in ["id=s-1", "submission=false"] {
        assert!(
            pairs.contains(&piece.to_owned()),
            "{piece} missing: {}",
            urls[0]
        );
    }
}

#[test]
fn submit_goes_over_the_wire_as_submission_true() {
    let transport = Recording {
        urls: Mutex::new(Vec::new()),
    };
    let scrobbler = Scrobbler::new(Client::new(
        &transport,
        "https://music.example.com",
        "demo",
        Auth::Token {
            password: "demo".to_owned(),
        },
    ));

    scrobbler.submit("s-1");

    let pairs = query_pairs(&transport.urls.lock().unwrap()[0]);
    assert!(pairs.contains(&"id=s-1".to_owned()));
    assert!(pairs.contains(&"submission=true".to_owned()));
}

// ------------------------------------------------- the interface's wiring

#[test]
fn the_interface_scrobbles_remote_track_changes_and_shows_nothing() {
    let sink = RecordingSink::default();
    let mut app = App::browse(None);
    app.set_scrobbler(Some(Arc::new(sink.clone())));

    // A remote track starts.
    app.scrobble_tick(
        Some(remote("s-1")),
        Duration::ZERO,
        Some(Duration::from_secs(200)),
    );
    assert_eq!(sink.calls(), [("s-1".to_owned(), "now-playing")]);
    assert_eq!(
        app.status(),
        "",
        "a scrobble is never a message the user sees"
    );

    // It counts.
    app.scrobble_tick(
        Some(remote("s-1")),
        Duration::from_secs(100),
        Some(Duration::from_secs(200)),
    );
    assert_eq!(sink.calls().len(), 2);
    assert_eq!(app.status(), "");

    // A local track plays: nothing is sent.
    app.scrobble_tick(
        Some(local()),
        Duration::from_secs(600),
        Some(Duration::from_secs(3600)),
    );
    assert_eq!(sink.calls().len(), 2);
}

#[test]
fn the_interface_without_a_scrobbler_never_scrobbles() {
    let mut app = App::browse(None);
    // No set_scrobbler call: a local-only or unconfigured launch.
    app.scrobble_tick(
        Some(remote("s-1")),
        Duration::from_secs(100),
        Some(Duration::from_secs(200)),
    );
    // Nothing to assert but that nothing panicked: the calls live in the sink
    // that was never installed.
}

#[test]
fn a_failing_scrobble_changes_nothing_the_user_can_see() {
    let mut app = App::browse(None);
    // A sink backed by a broken transport: the swallow lives in the
    // `Scrobbles` impl, so the interface stays untouched by it.
    app.set_scrobbler(Some(Arc::new(Scrobbler::new(Client::new(
        Broken,
        "https://music.example.com",
        "demo",
        Auth::Token {
            password: "demo".to_owned(),
        },
    )))));

    app.scrobble_tick(
        Some(remote("s-1")),
        Duration::ZERO,
        Some(Duration::from_secs(200)),
    );
    app.scrobble_tick(
        Some(remote("s-1")),
        Duration::from_secs(100),
        Some(Duration::from_secs(200)),
    );

    assert_eq!(
        app.status(),
        "",
        "a failing scrobble is logged nowhere loud"
    );
}

#[test]
fn a_failing_sink_never_panics() {
    let scrobbler = Scrobbler::new(Client::new(
        Broken,
        "https://music.example.com",
        "demo",
        Auth::Token {
            password: "demo".to_owned(),
        },
    ));
    scrobbler.now_playing("s-1");
    scrobbler.submit("s-1");
}
