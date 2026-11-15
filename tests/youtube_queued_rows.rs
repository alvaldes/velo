//! A `youtube://` row that reaches the queue as a `File` entry — from the
//! history, a favourite, a playlist, or `velo --play youtube://<id>` — plays
//! exactly the way its own [`Entry::Youtube`] kind does: the path asks the
//! streaming question first, one bounded resolve, and the tool's own pipe as
//! the stream — while the carried source of a start's worker is never
//! resolved a second time (issue #70, S4 as revised by L9).
//!
//! Every case here runs a fake program written into a scratch directory; the
//! streamed cases stream a real fixture through the pipe, and the
//! carried-source control serves its body through a transport the same way
//! `tests/remote_stream.rs`'s fixture does. No test reaches the network, and
//! none needs `yt-dlp` installed.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use velo::audio::TrackSource;
use velo::audio::output::{
    Entry, PathResolver, SharedQueue, TrackFailure, drain_headless_resolving,
    drain_headless_with_preopen, open_first_entry,
};
use velo::audio::remote::{RangeAnswer, RangeTransport, RemoteOpen};
use velo::config::ReplayGainMode;
use velo::subsonic::Error;
use velo::youtube::pseudo_path;
use velo::youtube::resolve::Resolver;

/// The URL the carried-source control's transport serves; nothing in the
/// streamed route carries a URL any more.
const STREAM_URL: &str = "https://stream.example/audio.m4a";

/// This case's scratch directory, recreated empty so a failed earlier run can
/// never satisfy a later assertion.
fn scratch(case: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("velo-youtube-rows-{case}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("the scratch directory creates");
    dir
}

/// The MP3 fixture the fake chain streams: the same shape the real chain has
/// — a sequential, decodable MP3 on the transcoder's stdout.
fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("silence-then-tone.mp3")
}

/// A fake chain shaped like the real one: the downloader counts its own runs
/// in a file beside it and streams the fixture; the transcoder passes its
/// stdin through — the chain itself is exercised, stdin wired to the
/// downloader's stdout — so a test can prove the tool ran, or that it never
/// did.
fn fake(dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;

    let yt = dir.join("yt-dlp");
    let runs = dir.join("runs");
    let body = format!(
        "#!/bin/sh\necho run >> {runs}\ncat {fixture}\n",
        runs = runs.display(),
        fixture = fixture().display(),
    );
    fs::write(&yt, body).expect("the fake downloader is written");
    fs::set_permissions(&yt, fs::Permissions::from_mode(0o755))
        .expect("the fake downloader turns executable");
    let ff = dir.join("ffmpeg");
    fs::write(&ff, "#!/bin/sh\ncat\n").expect("the fake transcoder is written");
    fs::set_permissions(&ff, fs::Permissions::from_mode(0o755))
        .expect("the fake transcoder turns executable");
    (yt, ff, runs)
}

/// How many times the fake downloader ran: a resolve the play did not need
/// is a second ask the queue never owes.
fn tool_runs(runs: &Path) -> usize {
    fs::read_to_string(runs).map_or(0, |content| content.lines().count())
}

/// A transport that records every URL it was asked about and serves slices of
/// one canned body — the carried-source control only, the way
/// `tests/remote_stream.rs`'s fixture does — never a network.
struct Serves {
    body: Vec<u8>,
    urls: Mutex<Vec<String>>,
}

impl RangeTransport for Serves {
    fn range(&self, url: &str, from: u64, to: Option<u64>) -> Result<RangeAnswer, Error> {
        self.urls
            .lock()
            .expect("the record unlocks")
            .push(url.to_owned());
        let total = self.body.len() as u64;
        let end = to.map_or(total, |to| (to + 1).min(total));
        let from = usize::try_from(from).expect("fixture bodies are small");
        let end = usize::try_from(end).expect("fixture bodies are small");
        Ok(RangeAnswer::Partial {
            bytes: self.body[from..end].to_vec(),
            total,
        })
    }
}

impl Serves {
    /// Every URL the transport was asked for, in order.
    fn asked(&self) -> Vec<String> {
        self.urls.lock().expect("the record unlocks").clone()
    }
}

/// The MP3 fixture's samples decoded as a local file — the reference the
/// streamed pipe is held to.
///
/// Each call writes its own directory, numbered: the tests run in parallel, so
/// a shared name here is one test wiping another's reference file while it is
/// being read.
fn reference() -> (velo::audio::TrackSpec, Vec<f32>) {
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    let dir = scratch(&format!(
        "reference-{}",
        CALLS.fetch_add(1, Ordering::Relaxed)
    ));
    let path = dir.join("reference.mp3");
    let bytes = fs::read(fixture()).expect("the fixture reads");
    fs::write(&path, bytes).expect("the reference writes");

    let mut source = TrackSource::open(&path).expect("the reference opens");
    let spec = source.spec();
    let mut samples = Vec::new();
    while let Some(block) = source.read_block().expect("the reference decodes") {
        samples.extend_from_slice(&block);
    }

    let _ = fs::remove_dir_all(&dir);
    (spec, samples)
}

/// A resolver over the fake chain: the whole streamed seam, with nothing
/// reaching the network.
fn resolver(yt_dlp: PathBuf, ffmpeg: PathBuf) -> Arc<dyn PathResolver> {
    Arc::new(Resolver::new(yt_dlp, ffmpeg))
}

/// B1: a `youtube://` row that arrived as a `File` entry — the history, a
/// favourite, a playlist, `--play` — is never a file: the producer asks the
/// streaming question first and the track plays from the tool's own pipe,
/// instead of being skipped with `cannot read the file`.
#[test]
fn a_youtube_row_that_arrives_as_a_file_entry_plays_the_stream_the_tool_resolves() {
    let dir = scratch("file-entry");
    let (program, ff, runs) = fake(&dir);
    let (spec, expected_samples) = reference();
    let queue: SharedQueue = Arc::new(std::sync::Mutex::new(vec![Entry::File(pseudo_path("abc"))]));
    let heard: Mutex<Vec<f32>> = Mutex::new(Vec::new());
    let failures: Mutex<Vec<TrackFailure>> = Mutex::new(Vec::new());

    drain_headless_resolving(
        queue,
        0,
        0,
        spec,
        ReplayGainMode::default(),
        Some(resolver(program, ff)),
        &AtomicU64::new(0),
        &AtomicBool::new(false),
        &mut |block| heard.lock().expect("the samples").extend_from_slice(block),
        &mut |failure| failures.lock().expect("the failures").push(failure),
    )
    .expect("the sequence drains");

    assert!(
        failures.lock().expect("the failures").is_empty(),
        "a youtube:// row that arrived as a file entry is never skipped: {:?}",
        failures.lock().expect("the failures")
    );
    assert_eq!(
        *heard.lock().expect("the samples"),
        expected_samples,
        "the tool's pipe decodes to exactly the fixture's own samples"
    );
    assert_eq!(tool_runs(&runs), 1, "exactly one resolve, the play's own");
}

/// The control: a result queued from the screen — its own `Entry::Youtube`
/// kind — plays the same way, and the fix for the `File` shape changed
/// nothing about it.
#[test]
fn a_result_queued_from_the_screen_still_plays_from_its_own_entry() {
    let dir = scratch("screen-entry");
    let (program, ff, runs) = fake(&dir);
    let (spec, expected_samples) = reference();
    let queue: SharedQueue = Arc::new(std::sync::Mutex::new(vec![Entry::Youtube {
        path: pseudo_path("abc"),
        title: String::from("Dawn"),
        uploader: Some(String::from("Chill Channel")),
        duration: Some(212),
    }]));
    let heard: Mutex<Vec<f32>> = Mutex::new(Vec::new());
    let failures: Mutex<Vec<TrackFailure>> = Mutex::new(Vec::new());

    drain_headless_resolving(
        queue,
        0,
        0,
        spec,
        ReplayGainMode::default(),
        Some(resolver(program, ff)),
        &AtomicU64::new(0),
        &AtomicBool::new(false),
        &mut |block| heard.lock().expect("the samples").extend_from_slice(block),
        &mut |failure| failures.lock().expect("the failures").push(failure),
    )
    .expect("the sequence drains");

    assert!(
        failures.lock().expect("the failures").is_empty(),
        "the screen's own entry is never skipped: {:?}",
        failures.lock().expect("the failures")
    );
    assert_eq!(*heard.lock().expect("the samples"), expected_samples);
    assert_eq!(tool_runs(&runs), 1, "exactly one resolve, the play's own");
}

/// The carried source of a start's worker (`preopened_file`) is the track
/// already: the first turn opens nothing and resolves nothing — one
/// selection-time ask, never a second one.
#[test]
fn a_carried_source_plays_without_a_second_resolve() {
    let dir = scratch("carried");
    let (_program, _ff, runs) = fake(&dir);
    let (spec, expected_samples) = reference();
    let transport = Arc::new(Serves {
        body: fs::read(fixture()).expect("the fixture reads"),
        urls: Mutex::new(Vec::new()),
    });
    let RemoteOpen::Streams(source) =
        velo::audio::remote::open(Arc::clone(&transport), STREAM_URL).expect("the probe answers")
    else {
        panic!("a ranged server streams");
    };
    let preopened = TrackSource::open_remote(Box::new(source)).expect("the track opens");
    let queue: SharedQueue = Arc::new(std::sync::Mutex::new(vec![Entry::File(pseudo_path("abc"))]));
    let heard: Mutex<Vec<f32>> = Mutex::new(Vec::new());
    let failures: Mutex<Vec<TrackFailure>> = Mutex::new(Vec::new());

    drain_headless_with_preopen(
        queue,
        0,
        0,
        spec,
        ReplayGainMode::default(),
        preopened,
        &AtomicBool::new(false),
        &mut |block| heard.lock().expect("the samples").extend_from_slice(block),
        &mut |failure| failures.lock().expect("the failures").push(failure),
    )
    .expect("the sequence drains");

    assert!(
        failures.lock().expect("the failures").is_empty(),
        "the carried turn is never skipped: {:?}",
        failures.lock().expect("the failures")
    );
    assert_eq!(*heard.lock().expect("the samples"), expected_samples);
    assert_eq!(
        tool_runs(&runs),
        0,
        "the tool never ran again: the carried source was the track already"
    );
    assert_eq!(
        transport.asked(),
        vec![STREAM_URL.to_owned()],
        "only the carried open's own probe reached the transport"
    );
}

/// B1, at the seam every start shares: a `youtube://` row that arrived as a
/// `File` entry — what `velo --play youtube://<id>` queues, and what the
/// history, a favourite and a playlist rebuild — opens the tool's own stream
/// at selection time, the way its own entry kind does, and the source is
/// carried to the first turn. `PathResolver::resolve`'s refusal for a
/// `youtube://` row is never reached.
#[test]
fn a_file_row_that_is_a_youtube_row_opens_its_stream_at_selection_time() {
    let dir = scratch("selection");
    let (program, ff, runs) = fake(&dir);
    let (spec, _) = reference();

    let (opened_spec, preopen, carried) = open_first_entry(
        &Entry::File(pseudo_path("abc")),
        None,
        Some(&resolver(program, ff)),
    )
    .expect("the row opens as its resolved stream");

    assert_eq!(
        opened_spec, spec,
        "the stream's spec is the device's, exactly as its own entry kind gives it"
    );
    assert!(
        preopen.is_none(),
        "nothing was opened as a station: the streaming answer carried no preopen"
    );
    let (carried_path, _carried_source) =
        carried.expect("the opened source is carried to the first turn");
    assert_eq!(
        carried_path,
        pseudo_path("abc"),
        "the carried source is keyed by the path the producer matches against"
    );
    assert_eq!(
        tool_runs(&runs),
        1,
        "exactly one resolve, the selection's own"
    );
}

/// The carried source of a start's worker rides through the same seam
/// untouched: the selection opens nothing and resolves nothing — one ask,
/// the worker's already made.
#[test]
fn a_carried_first_source_opens_nothing_and_resolves_nothing() {
    let dir = scratch("carried-selection");
    let (_program, _ff, runs) = fake(&dir);
    let transport = Arc::new(Serves {
        body: fs::read(fixture()).expect("the fixture reads"),
        urls: Mutex::new(Vec::new()),
    });
    let RemoteOpen::Streams(source) =
        velo::audio::remote::open(Arc::clone(&transport), STREAM_URL).expect("the probe answers")
    else {
        panic!("a ranged server streams");
    };

    let before = transport.asked().len();

    let (_, preopen, carried) = open_first_entry(
        &Entry::File(pseudo_path("abc")),
        Some(Box::new(source)),
        Some(&resolver(
            PathBuf::from("/nowhere/yt-dlp"),
            PathBuf::from("/nowhere/ffmpeg"),
        )),
    )
    .expect("the carried source is the track already");

    assert!(preopen.is_none());
    assert!(
        carried.is_some(),
        "the carried source rides to the first turn, keyed by the entry"
    );
    assert_eq!(
        transport.asked().len(),
        before,
        "the selection asked the transport nothing more: the carried source was used as it is"
    );
    assert_eq!(
        tool_runs(&runs),
        0,
        "the tool never ran again: the worker's ask was not made twice"
    );
}
