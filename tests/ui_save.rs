//! Keeping a remote track (issue #45): `ctrl+s` on a remote entry — the one
//! playing, the row under the cursor in the queue, or a row in a list that
//! holds remote tracks — asks for a destination, prefilled
//! `~/Downloads/<the file's own name>`, and copies the cached bytes there
//! off the event loop, a copy out that never touches the library or the
//! cache's own files, and refuses a local file and a live station with
//! their own lines.
//!
//! No test touches the real network or the user's real `~/Downloads`: the
//! transport is a canned fake, the cache and the destination live in
//! throwaway temporary directories, and no environment variable is set
//! (`set_var` is `unsafe` in edition 2024 and this repository forbids
//! unsafe code).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use velo::audio::output::{Entry, PathResolver};
use velo::subsonic::Error as SubsonicError;
use velo::subsonic::cache::{Cache, RemoteResolver, SubsonicResolver, Unconfigured};
use velo::subsonic::client::{Auth, Client, Transport};
use velo::ui::App;
use velo::ui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::from(code)
}

/// A `ctrl`-modified press, the way a terminal reports it.
fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

/// One key press, its action applied.
fn press(app: &mut App<'_>, event: KeyEvent) {
    let action = app.on_key(event);
    app.apply(action);
}

/// Types every character into whatever owns printable keys right now.
fn type_text(app: &mut App<'_>, text: &str) {
    for character in text.chars() {
        app.on_key(key(KeyCode::Char(character)));
    }
}

/// Empties the save prompt's input, the way a user would.
fn clear_input(app: &mut App<'_>) {
    while !app.save_prompt_input().is_empty() {
        app.on_key(key(KeyCode::Backspace));
    }
}

fn temp_dir(name: &str) -> PathBuf {
    let dir =
        Path::new(&std::env::temp_dir()).join(format!("velo-save-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("a throwaway directory");
    dir
}

/// Ticks until the running save reports its end, and returns the final
/// status line. A save that never ends fails the test rather than hanging:
/// the clock is generous, a local copy is fast.
fn run_to_end(app: &mut App<'_>) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            Instant::now() < deadline,
            "the save never reported its end; status: {}",
            app.status()
        );
        if !app.save_running() {
            return app.status().to_owned();
        }
        std::thread::sleep(Duration::from_millis(5));
        app.tick();
    }
}

/// A transport that answers the stream requests with its canned bodies, in
/// order, once each, and counts every call.
struct Counting {
    bodies: Mutex<Vec<Vec<u8>>>,
    calls: AtomicUsize,
}

impl Counting {
    fn with(bodies: &[&[u8]]) -> Self {
        Self {
            bodies: Mutex::new(bodies.iter().map(|body| body.to_vec()).collect()),
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Transport for Counting {
    fn get(&self, _url: &str) -> Result<Vec<u8>, SubsonicError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut bodies = self.bodies.lock().expect("the canned bodies");
        if bodies.is_empty() {
            return Err(SubsonicError::Unreachable {
                detail: "no canned body left".to_owned(),
            });
        }
        Ok(bodies.remove(0))
    }
}

/// The same transport, but its answer takes `delay` to arrive: the stand-in
/// for a slow server, so the test can see the copy's progress line.
struct Slow {
    body: Vec<u8>,
    delay: Duration,
}

impl Transport for Slow {
    fn get(&self, _url: &str) -> Result<Vec<u8>, SubsonicError> {
        std::thread::sleep(self.delay);
        Ok(self.body.clone())
    }
}

/// A Subsonic resolver over the canned transport and a throwaway cache,
/// installed as the app's resolver exactly as the binary installs the real
/// one.
fn resolver_for(transport: Arc<Counting>, cache: PathBuf) -> Arc<dyn PathResolver> {
    let client = Client::new(
        transport,
        "https://music.example.com",
        "demo",
        Auth::Token {
            password: "demo".to_owned(),
        },
    );
    Arc::new(SubsonicResolver::new(
        client,
        "subsonic:music.example.com".to_owned(),
        Cache::new(cache),
    ))
}

/// An app whose queue holds one remote Subsonic song, the queue view open
/// with the cursor on it — the ordinary place `ctrl+s` is pressed.
fn app_with_queued_subsonic_song(resolver: Arc<dyn PathResolver>) -> App<'static> {
    let mut app = App::new(Vec::new());
    app.set_resolver(Some(resolver));
    app.queue_song_for_tests("subsonic://music.example.com/s-1");
    press(&mut app, key(KeyCode::Char('p')));
    app
}

// ---- the copy out ----------------------------------------------------------

#[test]
fn ctrl_s_saves_a_remote_track_from_the_cache_to_the_destination() {
    let cache = temp_dir("copy-cache");
    let downloads = temp_dir("copy-downloads");
    let transport = Arc::new(Counting::with(&[b"canned-audio"]));
    let mut app = app_with_queued_subsonic_song(resolver_for(transport.clone(), cache.clone()));

    press(&mut app, ctrl(KeyCode::Char('s')));
    assert!(
        app.save_prompt_open(),
        "ctrl+s on a remote row asks for a destination"
    );

    clear_input(&mut app);
    let destination = downloads.join("song.mp3");
    type_text(&mut app, &destination.display().to_string());
    press(&mut app, key(KeyCode::Enter));
    assert!(
        !app.save_prompt_open(),
        "an accepted answer closes the prompt"
    );

    let final_line = run_to_end(&mut app);
    assert!(
        final_line.contains("saved to"),
        "one line says where it landed: {final_line}"
    );
    assert!(
        final_line.contains(destination.display().to_string().as_str()),
        "{final_line}"
    );
    assert_eq!(
        fs::read(&destination).expect("the copy is whole"),
        b"canned-audio",
        "the destination holds the cached bytes"
    );
    assert_eq!(
        transport.calls(),
        1,
        "the cache miss fetched the track once, through the resolver"
    );
    let cached: Vec<PathBuf> = fs::read_dir(&cache)
        .expect("the cache directory")
        .flatten()
        .map(|entry| entry.path())
        .collect();
    assert_eq!(
        cached.len(),
        1,
        "the copy is a copy out: the cache keeps its fetched file"
    );
    assert_eq!(
        fs::read(&cached[0]).unwrap(),
        b"canned-audio",
        "the cache file is the same bytes"
    );
}

#[test]
fn a_cache_hit_costs_no_second_fetch() {
    let cache = temp_dir("hit-cache");
    let downloads = temp_dir("hit-downloads");
    let transport = Arc::new(Counting::with(&[b"canned-audio"]));
    let client = Client::new(
        transport.clone(),
        "https://music.example.com",
        "demo",
        Auth::Token {
            password: "demo".to_owned(),
        },
    );
    let resolver: Arc<dyn PathResolver> = Arc::new(SubsonicResolver::new(
        client,
        "subsonic:music.example.com".to_owned(),
        Cache::new(cache),
    ));
    // Warm the cache the way a play would have: the next resolve is a hit.
    resolver
        .resolve(Path::new("subsonic://music.example.com/s-1"))
        .expect("the warm-up fetch");
    assert_eq!(transport.calls(), 1);

    let mut app = app_with_queued_subsonic_song(resolver);
    press(&mut app, ctrl(KeyCode::Char('s')));
    clear_input(&mut app);
    let destination = downloads.join("song.flac");
    type_text(&mut app, &destination.display().to_string());
    press(&mut app, key(KeyCode::Enter));

    run_to_end(&mut app);
    assert_eq!(
        fs::read(&destination).unwrap(),
        b"canned-audio",
        "a hit copies the cached bytes"
    );
    assert_eq!(
        transport.calls(),
        1,
        "the save itself called no network: the cached file was enough"
    );
}

#[test]
fn a_channel_song_saves_the_same_way_and_the_prefill_is_the_files_own_name() {
    let cache = temp_dir("url-cache");
    let downloads = temp_dir("url-downloads");
    let transport = Arc::new(Counting::with(&[b"wrapper-song-bytes"]));
    let resolver: Arc<dyn PathResolver> =
        Arc::new(RemoteResolver::new(transport, Cache::new(cache), None));
    let mut app = App::new(Vec::new());
    app.set_resolver(Some(resolver));
    app.queue_song_for_tests("https://wrap.example/live/a.mp3");
    press(&mut app, key(KeyCode::Char('p')));

    press(&mut app, ctrl(KeyCode::Char('s')));
    assert!(
        app.save_prompt_open(),
        "a channel's song is a remote track a save can copy out"
    );
    assert_eq!(
        app.save_prompt_input(),
        "~/Downloads/a.mp3",
        "prefilled with ~/Downloads and the file's own name"
    );

    clear_input(&mut app);
    let destination = downloads.join("kept.mp3");
    type_text(&mut app, &destination.display().to_string());
    press(&mut app, key(KeyCode::Enter));

    run_to_end(&mut app);
    assert_eq!(
        fs::read(&destination).unwrap(),
        b"wrapper-song-bytes",
        "the song was fetched through the resolver and copied"
    );
}

// ---- the two refusals ------------------------------------------------------

#[test]
fn a_local_file_is_refused_because_it_is_already_on_disk() {
    let mut app = App::new(vec![PathBuf::from("/music/artist/one.mp3")]);
    press(&mut app, ctrl(KeyCode::Char('s')));
    assert!(!app.save_prompt_open(), "no prompt for a local file");
    let status = app.status();
    assert!(status.contains("already a file on disk"), "{status}");
    assert!(status.contains("cp"), "{status}");
}

#[test]
fn a_live_station_is_refused_because_a_stream_does_not_end() {
    let mut app = App::new(Vec::new());
    app.show_playing_for_tests(
        Entry::Station {
            url: "https://radio.example/live".to_owned(),
            name: "Nova".to_owned(),
        },
        Duration::ZERO,
        None,
        None,
    );
    press(&mut app, ctrl(KeyCode::Char('s')));
    assert!(!app.save_prompt_open(), "no prompt for a station");
    let status = app.status();
    assert!(status.contains("live"), "{status}");
    assert!(status.contains("does not end"), "{status}");
}

// ---- S3: an existing destination is never overwritten ---------------------

#[test]
fn an_existing_destination_is_refused_in_the_card_with_its_name_kept() {
    let cache = temp_dir("exists-cache");
    let downloads = temp_dir("exists-downloads");
    let transport = Arc::new(Counting::with(&[b"canned-audio"]));
    let mut app = app_with_queued_subsonic_song(resolver_for(transport.clone(), cache.clone()));

    let destination = downloads.join("song.mp3");
    fs::write(&destination, b"precious").expect("the file that is already there");

    press(&mut app, ctrl(KeyCode::Char('s')));
    clear_input(&mut app);
    type_text(&mut app, &destination.display().to_string());
    press(&mut app, key(KeyCode::Enter));

    assert!(
        app.save_prompt_open(),
        "a refused answer leaves the prompt open"
    );
    let error = app.save_prompt_error().unwrap_or_default();
    assert!(error.contains("already"), "{error}");
    assert_eq!(
        fs::read(&destination).unwrap(),
        b"precious",
        "nothing was overwritten"
    );
    assert!(!app.save_running(), "no copy started");

    // A different name answers the refusal, and the copy lands there.
    clear_input(&mut app);
    let other = downloads.join("song (1).mp3");
    type_text(&mut app, &other.display().to_string());
    press(&mut app, key(KeyCode::Enter));
    let final_line = run_to_end(&mut app);
    assert!(final_line.contains("saved to"), "{final_line}");
    assert_eq!(fs::read(&other).unwrap(), b"canned-audio");
    assert_eq!(
        fs::read(&destination).unwrap(),
        b"precious",
        "the file that was there is still exactly it"
    );
}

// ---- S4: a failure says why and leaves no half file -----------------------

#[test]
fn a_failed_fetch_is_one_line_with_the_reason_and_no_partial_file() {
    let downloads = temp_dir("fail-downloads");
    let resolver: Arc<dyn PathResolver> =
        Arc::new(Unconfigured::new("the server is unreachable".to_owned()));
    let mut app = app_with_queued_subsonic_song(resolver);

    press(&mut app, ctrl(KeyCode::Char('s')));
    clear_input(&mut app);
    let destination = downloads.join("song.mp3");
    type_text(&mut app, &destination.display().to_string());
    press(&mut app, key(KeyCode::Enter));

    let final_line = run_to_end(&mut app);
    assert!(
        final_line.contains("could not be saved"),
        "one line with the reason: {final_line}"
    );
    assert!(final_line.contains("unreachable"), "{final_line}");
    let leftovers: Vec<PathBuf> = fs::read_dir(&downloads)
        .expect("the destination directory")
        .flatten()
        .map(|entry| entry.path())
        .collect();
    assert!(
        leftovers.is_empty(),
        "nothing half-written is left behind: {leftovers:?}"
    );
}

#[test]
fn a_destination_directory_that_does_not_exist_is_a_failure_with_a_reason() {
    let scratch = temp_dir("no-dir");
    let transport = Arc::new(Counting::with(&[b"canned-audio"]));
    let mut app = app_with_queued_subsonic_song(resolver_for(transport, temp_dir("no-dir-cache")));

    press(&mut app, ctrl(KeyCode::Char('s')));
    clear_input(&mut app);
    let destination = scratch.join("no-such-folder").join("song.mp3");
    type_text(&mut app, &destination.display().to_string());
    press(&mut app, key(KeyCode::Enter));

    let final_line = run_to_end(&mut app);
    assert!(
        final_line.contains("could not be saved"),
        "one line with the reason: {final_line}"
    );
    assert!(
        !scratch.join("no-such-folder").exists(),
        "velo does not invent the directory"
    );
}

// ---- the one copy at a time -----------------------------------------------

#[test]
fn a_second_ctrl_s_while_one_runs_is_refused_and_starts_no_second_copy() {
    let cache = temp_dir("twice-cache");
    let downloads = temp_dir("twice-downloads");
    let transport = Arc::new(Counting::with(&[b"canned-audio"]));
    let mut app = app_with_queued_subsonic_song(resolver_for(transport.clone(), cache.clone()));

    press(&mut app, ctrl(KeyCode::Char('s')));
    clear_input(&mut app);
    let destination = downloads.join("song.mp3");
    type_text(&mut app, &destination.display().to_string());
    press(&mut app, key(KeyCode::Enter));
    assert!(app.save_running(), "the first copy is running");

    press(&mut app, ctrl(KeyCode::Char('s')));
    assert!(!app.save_prompt_open(), "no second prompt");
    let status = app.status();
    assert!(status.contains("already saving"), "{status}");

    let final_line = run_to_end(&mut app);
    assert!(final_line.contains("saved to"), "{final_line}");
    assert_eq!(
        transport.calls(),
        1,
        "the refused request fetched nothing and copied nothing"
    );
    let files: Vec<PathBuf> = fs::read_dir(&downloads)
        .expect("the destination directory")
        .flatten()
        .map(|entry| entry.path())
        .collect();
    assert_eq!(files, vec![destination], "exactly one copy landed");
}

// ---- the prompt itself -----------------------------------------------------

#[test]
fn the_prompt_opens_prefilled_edits_and_cancels() {
    let mut app = App::new(Vec::new());
    app.queue_song_for_tests("subsonic://music.example.com/track-9");
    press(&mut app, key(KeyCode::Char('p')));

    press(&mut app, ctrl(KeyCode::Char('s')));
    assert!(app.save_prompt_open());
    assert_eq!(
        app.save_prompt_input(),
        "~/Downloads/track-9",
        "prefilled with ~/Downloads and the file's own name"
    );

    press(&mut app, key(KeyCode::Backspace));
    press(&mut app, key(KeyCode::Backspace));
    assert_eq!(
        app.save_prompt_input(),
        "~/Downloads/track",
        "backspace edits the prefill"
    );

    press(&mut app, key(KeyCode::Esc));
    assert!(!app.save_prompt_open(), "esc cancels");
    assert!(!app.save_running(), "a cancelled prompt copies nothing");
}

#[test]
fn the_progress_and_the_final_line_land_in_the_status() {
    let cache = temp_dir("slow-cache");
    let downloads = temp_dir("slow-downloads");
    let resolver: Arc<dyn PathResolver> = Arc::new(RemoteResolver::new(
        Arc::new(Slow {
            body: b"slow-arriving-bytes".to_vec(),
            delay: Duration::from_millis(400),
        }),
        Cache::new(cache),
        None,
    ));
    let mut app = App::new(Vec::new());
    app.set_resolver(Some(resolver));
    app.queue_song_for_tests("https://wrap.example/live/slow.mp3");
    press(&mut app, key(KeyCode::Char('p')));

    press(&mut app, ctrl(KeyCode::Char('s')));
    clear_input(&mut app);
    let destination = downloads.join("slow.mp3");
    type_text(&mut app, &destination.display().to_string());
    press(&mut app, key(KeyCode::Enter));

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut saw_progress = false;
    let mut final_line = String::new();
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
        app.tick();
        let status = app.status().to_owned();
        if status.starts_with("⟳ saving") {
            saw_progress = true;
            assert!(
                status.contains("slow.mp3"),
                "the progress names what is being saved: {status}"
            );
        }
        if !app.save_running() {
            final_line = status;
            break;
        }
    }
    assert!(
        saw_progress,
        "the status line showed the copy's progress; ended as: {final_line}"
    );
    assert!(final_line.contains("saved to"), "{final_line}");
    assert_eq!(fs::read(&destination).unwrap(), b"slow-arriving-bytes");
}

#[test]
fn the_progress_line_says_what_it_honestly_knows() {
    assert_eq!(
        velo::ui::save_progress_line("a.mp3", 50, Some(100)),
        "⟳ saving a.mp3… 50%",
        "a known total is a percentage"
    );
    assert_eq!(
        velo::ui::save_progress_line("a.mp3", 500, None),
        "⟳ saving a.mp3…",
        "no total, no made-up percentage"
    );
}

// ---- nothing else changes --------------------------------------------------

#[test]
fn the_help_card_names_the_save() {
    let mut app = App::new(Vec::new());
    press(&mut app, key(KeyCode::Char('?')));

    let mut terminal =
        velo::ui::ratatui::Terminal::new(velo::ui::ratatui::backend::TestBackend::new(80, 50))
            .expect("a test terminal");
    terminal
        .draw(|frame| app.draw(frame))
        .expect("the frame draws");
    let screen: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol().to_owned())
        .collect();
    assert!(screen.contains("ctrl+s"), "{screen}");
    assert!(screen.contains("save a remote track"), "{screen}");
}

#[test]
fn ctrl_s_with_nothing_to_save_says_so() {
    let mut app = App::new(Vec::new());
    press(&mut app, ctrl(KeyCode::Char('s')));
    assert!(!app.save_prompt_open(), "no prompt without an entry");
    assert!(app.status().contains("nothing to save"), "{}", app.status());
}
