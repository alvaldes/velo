//! The `YouTube` search screen (issue #70, T5), tested without a terminal
//! and without a network: the row and its fact, the tool-less refusal, the
//! search and its answers through the worker, the queueing and the playing,
//! and that nothing about a search is stored.
//!
//! Every case here runs a fake program written into a scratch directory: no
//! test in this file reaches the network, and none of them needs `yt-dlp`
//! installed.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use velo::audio::output::Entry;
use velo::ui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use velo::ui::{Action, App, View};

/// How long a test waits for the worker's answer before it calls the answer
/// not arrived: the fake program finishes in milliseconds, so this is
/// generous and never the tool's own deadline.
const ARRIVAL_BOUND: Duration = Duration::from_secs(5);

/// This case's scratch directory, recreated empty so a failed earlier run
/// can never satisfy a later assertion.
fn scratch(case: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("velo-ui-youtube-{case}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the scratch directory creates");
    dir
}

/// A fake program: executable, and small enough to read at the call site.
fn fake(dir: &Path, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = dir.join("yt-dlp");
    std::fs::write(&path, body).expect("the fake program is written");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("the fake program turns executable");
    path
}

fn app() -> App<'static> {
    App::browse(None)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::from(code)
}

/// A `ctrl`-modified press, the way a terminal reports it.
fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

/// One key press, its action applied.
fn press(app: &mut App<'_>, code: KeyCode) -> Action {
    let action = app.on_key(key(code));
    app.apply(action);
    action
}

/// Ticks until the predicate holds or the wait runs out. The wait is a
/// wall-clock bound, generous against a worker thread's own scheduling: a
/// fake program answers in milliseconds.
fn until(app: &mut App<'_>, satisfies: impl Fn(&App<'_>) -> bool) -> bool {
    let started = std::time::Instant::now();
    while started.elapsed() < ARRIVAL_BOUND {
        app.tick();
        if satisfies(app) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    false
}

/// The whole interface as one screen, the way the user sees it.
fn screen_of(app: &App) -> String {
    use velo::ui::ratatui::Terminal;
    use velo::ui::ratatui::backend::TestBackend;
    use velo::ui::ratatui::buffer::{Buffer, Cell};

    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("a test terminal");
    terminal
        .draw(|frame| app.draw(frame))
        .expect("the frame draws");
    let buffer: &Buffer = terminal.backend().buffer();
    let screen: String = buffer.content().iter().map(Cell::symbol).collect();
    let characters: Vec<char> = screen.chars().collect();
    characters
        .chunks(80)
        .map(|line| line.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The line of the sources screen a row lives on.
fn row_line(screen: &str, name: &str) -> String {
    screen
        .lines()
        .find(|line| line.contains(name))
        .unwrap_or_default()
        .to_owned()
}

/// One search answer: two results, one of them named through `channel`
/// instead of `uploader`, one of them a live broadcast (marked so by its
/// `live_status`) with no duration, and one line of garbage between them the
/// parser is expected to skip.
const SEARCH_ANSWER: &str = concat!(
    r#"{"id": "abc123", "title": "Dawn", "uploader": "Chill Channel", "duration": 212.0}"#,
    "\n",
    "```this line is garbage nobody reads```\n",
    r#"{"id": "def456", "title": "Nightfall", "channel": "Evening", "duration": null, "live_status": "is_live"}"#,
    "\n"
);

/// An app standing on the `YouTube` row with the tool found.
fn app_on_the_row(dir: &Path, body: &str) -> (App<'static>, PathBuf) {
    let program = fake(dir, body);
    let mut app = app();
    app.set_youtube_program(Some(program.clone()));
    app.open_sources();
    press(&mut app, KeyCode::Char('j')); // onto YouTube
    (app, program)
}

/// S1: the row exists, sits directly after `Radio`, and its fact says
/// whether the launch found the tool — `yt-dlp` or `yt-dlp missing` — from
/// the injected capability alone. No process is spawned to draw the screen.
#[test]
fn the_row_sits_after_radio_and_its_fact_says_whether_the_tool_was_found() {
    let mut app = app();
    app.open_sources();
    let screen = screen_of(&app);

    assert!(
        screen.contains("YouTube"),
        "the row is on screen, got:\n{screen}"
    );
    assert!(
        screen.contains("yt-dlp missing"),
        "without the tool the fact says so, got:\n{screen}"
    );
    let (radio, youtube) = (
        screen.find("Radio").expect("the Radio row is on screen"),
        screen
            .find("YouTube")
            .expect("the YouTube row is on screen"),
    );
    assert!(
        radio < youtube,
        "YouTube sits directly after Radio, got:\n{screen}"
    );

    let dir = scratch("fact-found");
    let program = fake(&dir, "#!/bin/sh\ntrue\n");
    app.set_youtube_program(Some(program));
    let screen = screen_of(&app);
    let line = row_line(&screen, "YouTube");
    assert!(
        line.contains("yt-dlp") && !line.contains("missing"),
        "with the tool the fact names it, got:\n{screen}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// S1, S10: opening the row without the tool opens no screen — one
/// sentence says how to install it and the cursor stays where it was.
#[test]
fn opening_the_row_without_the_tool_says_how_to_install_it_and_stays() {
    let mut app = app(); // no tool injected
    app.open_sources();
    press(&mut app, KeyCode::Char('j')); // onto YouTube

    press(&mut app, KeyCode::Enter);

    assert!(
        !app.youtube_open(),
        "no tool, no screen: the row opens nothing broken"
    );
    assert_eq!(
        app.status(),
        "yt-dlp is not installed — install it to play YouTube \
         (brew install yt-dlp, or your package manager)",
        "the install sentence is the status line, got: {}",
        app.status()
    );
    assert_eq!(app.view(), View::Sources, "the cursor stays where it was");
    let screen = screen_of(&app);
    for row in [
        "Radio",
        "YouTube",
        "Local library",
        "Recently played",
        "Favourites",
        "Playlists",
    ] {
        assert!(
            screen.contains(row),
            "the sources screen stands with its rows, got:\n{screen}"
        );
    }
    assert!(
        !screen.contains("YouTube search"),
        "no search screen opened, got:\n{screen}"
    );
}

/// T5: the query line takes what is typed, `enter` searches off the event
/// loop, the worker's answer arrives on a tick and lists itself, `enter` on
/// a result queues exactly one `Entry::Youtube` with the pseudo path and
/// plays it from its own row, and `esc` goes back to the sources.
#[test]
fn the_screen_searches_queues_and_plays_a_result() {
    let dir = scratch("screen");
    let (mut app, program) = app_on_the_row(
        &dir,
        &format!("#!/bin/sh\ncat <<'JSON'\n{SEARCH_ANSWER}\nJSON\n"),
    );
    press(&mut app, KeyCode::Enter); // YouTube: the screen opens

    assert!(app.youtube_open(), "the tool is there: the screen opens");

    // Typing goes to the query, and `enter` searches.
    for character in "dawn".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    press(&mut app, KeyCode::Enter);

    assert_eq!(
        app.status(),
        "⟳ searching dawn…",
        "the search says what it is looking for"
    );

    // The worker's answer arrives on a tick, without a key press.
    assert!(
        until(&mut app, |app| screen_of(app).contains("Dawn")),
        "the results land on the list: got:\n{}",
        screen_of(&app)
    );
    let screen = screen_of(&app);
    assert!(
        screen.contains("Nightfall"),
        "both parsed results are listed, got:\n{screen}"
    );
    assert!(
        !screen.contains("garbage"),
        "a line nobody can use is nobody's row, got:\n{screen}"
    );

    // The cursor moves off the query row onto the first result, and
    // `enter` queues exactly one entry and plays it from there.
    press(&mut app, KeyCode::Char('j'));
    let action = app.on_key(key(KeyCode::Enter));
    // The play action is the same one a queue row answers today; applying
    // it would open an audio device, which no test can, so it is asserted
    // and left for the shell.
    match &action {
        Action::PlayQueue { from } => {
            assert_eq!(*from, 0, "the entry just queued is the one that plays");
        }
        other => panic!("enter on a result plays from it, got {other:?}"),
    }
    assert_eq!(
        app.queue().len(),
        1,
        "one result, one entry: got {:?}",
        app.queue()
    );
    assert_eq!(
        app.queue()[0],
        Entry::Youtube {
            path: PathBuf::from("youtube://abc123"),
            title: String::from("Dawn"),
            uploader: Some(String::from("Chill Channel")),
            duration: Some(212),
        },
        "the entry carries the identity and the body the search said"
    );

    // `esc` goes back the way you came in, to the sources screen.
    press(&mut app, KeyCode::Esc);
    assert!(!app.youtube_open(), "esc closes the screen");
    assert_eq!(app.view(), View::Sources);
    assert!(
        !screen_of(&app).contains("YouTube search"),
        "the sources screen is back, got:\n{}",
        screen_of(&app)
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = program;
}

/// S5: a search whose tool fails names the tool's own reason, one line —
/// never a blank screen, never a hang.
#[test]
fn a_failing_search_names_the_tool_s_own_sentence() {
    let dir = scratch("failed");
    let body = "#!/bin/sh\ncat >/dev/null\nprintf 'ERROR: [youtube] no network'\nexit 1\n";
    let (mut app, _) = app_on_the_row(&dir, body);
    press(&mut app, KeyCode::Enter); // YouTube: the screen opens

    for character in "anything".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    press(&mut app, KeyCode::Enter);

    assert_eq!(app.status(), "⟳ searching anything…");

    assert!(
        until(&mut app, |app| app.status().contains("no network")),
        "the tool's own sentence is the failure line, got: {}",
        app.status()
    );
    assert!(
        !app.status().contains("the search stopped"),
        "a tool failure is not a lost worker, got: {}",
        app.status()
    );
    // And the interface still answers: the screen did not hang.
    assert!(
        screen_of(&app).contains("type a query"),
        "the screen stands"
    );
    press(&mut app, KeyCode::Esc);
    assert!(!app.youtube_open());

    let _ = std::fs::remove_dir_all(&dir);
}

/// A result the search marked live (`live_status: is_live`) is known before
/// it is chosen: its row wears the `✗` the radio wears for a station velo
/// cannot play, with no length, and choosing it anyway is refused with one
/// sentence and queues nothing — while the recorded result beside it still
/// queues and plays from its own row.
#[test]
fn a_live_result_is_marked_and_refused_while_a_recorded_one_still_queues() {
    let dir = scratch("live");
    let (mut app, _) = app_on_the_row(
        &dir,
        &format!("#!/bin/sh\ncat <<'JSON'\n{SEARCH_ANSWER}\nJSON\n"),
    );
    press(&mut app, KeyCode::Enter); // the screen opens
    for character in "dawn".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    press(&mut app, KeyCode::Enter);
    assert!(
        until(&mut app, |app| screen_of(app).contains("Dawn")),
        "the search answered"
    );

    let screen = screen_of(&app);
    let nightfall = row_line(&screen, "Nightfall");
    assert!(
        nightfall.contains("✗"),
        "the live result is marked the way an unplayable station is, got:\n{screen}"
    );
    assert!(
        !nightfall.contains(':'),
        "a live row shows no length, got: {nightfall}"
    );
    assert!(
        row_line(&screen, "Dawn").contains("3:32"),
        "the recorded row beside it keeps its length, got:\n{screen}"
    );

    // Choosing the live row anyway: one sentence, no queue entry.
    press(&mut app, KeyCode::Char('j')); // onto Dawn
    press(&mut app, KeyCode::Char('j')); // onto Nightfall
    press(&mut app, KeyCode::Enter);
    assert!(
        app.status().contains("HLS"),
        "the refusal says what it is and what to do: {}",
        app.status()
    );
    assert_eq!(
        app.queue().len(),
        0,
        "a refused result never joins the queue: {:?}",
        app.queue()
    );

    // The recorded result beside it still queues and plays from its row.
    press(&mut app, KeyCode::Char('k')); // back onto Dawn
    let action = app.on_key(key(KeyCode::Enter));
    match &action {
        Action::PlayQueue { from } => {
            assert_eq!(*from, 0, "the recorded result plays from its row");
        }
        other => panic!("enter on the recorded result queues it, got {other:?}"),
    }
    assert_eq!(
        app.queue().len(),
        1,
        "the recorded result queued exactly one entry: {:?}",
        app.queue()
    );

    press(&mut app, KeyCode::Esc);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The queue keeps the screen asleep (issue #23's rule): `tab` away and the
/// queue is what the body draws; `tab` back and the screen is exactly where
/// it was left.
#[test]
fn tab_away_puts_the_queue_over_the_screen_and_tab_back_wakes_it() {
    let dir = scratch("tab");
    let (mut app, _) = app_on_the_row(
        &dir,
        &format!("#!/bin/sh\ncat <<'JSON'\n{SEARCH_ANSWER}\nJSON\n"),
    );
    press(&mut app, KeyCode::Enter); // the screen opens
    for character in "dawn".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    press(&mut app, KeyCode::Enter);
    assert!(
        until(&mut app, |app| screen_of(app).contains("Dawn")),
        "the search answered"
    );

    press(&mut app, KeyCode::Tab);
    assert_eq!(app.view(), View::Queue);
    let screen = screen_of(&app);
    assert!(
        !screen.contains("YouTube search") && !screen.contains("Dawn"),
        "the screen sleeps under the queue, got:\n{screen}"
    );

    press(&mut app, KeyCode::Tab);
    assert_eq!(app.view(), View::Sources);
    assert!(
        screen_of(&app).contains("Dawn"),
        "the screen wakes where it was left, got:\n{}",
        screen_of(&app)
    );

    press(&mut app, KeyCode::Esc);
    let _ = std::fs::remove_dir_all(&dir);
}

/// S6's pin: a queued `youtube://` first entry goes to the start's resolve
/// worker, never on the event loop. The gate is [`App::is_uncached_remote`]
/// answering for a `youtube://` row and the fresh-start gate handing the
/// entry to `start_as_pending`; the observable shape is the fetching line
/// naming the entry by the body the search said, the queue standing while
/// the resolve runs, and the tool's own sentence arriving from the worker.
#[test]
fn a_queued_youtube_entry_resolves_on_the_worker_never_the_event_loop() {
    let dir = scratch("worker");
    let (mut app, _) = app_on_the_row(
        &dir,
        &format!("#!/bin/sh\ncat <<'JSON'\n{SEARCH_ANSWER}\nJSON\n"),
    );
    press(&mut app, KeyCode::Enter); // the screen opens
    // The session's resolver is the YouTube side over a program that is not
    // there: the resolve cannot succeed, which is what makes both its
    // thread and its failure observable.
    app.set_resolver(Some(std::sync::Arc::new(
        velo::youtube::resolve::Resolver::new(PathBuf::from("/velo-test/does-not-exist/yt-dlp")),
    )));
    for character in "dawn".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    press(&mut app, KeyCode::Enter);
    assert!(
        until(&mut app, |app| screen_of(app).contains("Dawn")),
        "the search answered"
    );

    // `enter` on the result queues exactly one entry and starts it the way
    // a queue row starts; the start is applied here, the way the shell
    // applies it.
    press(&mut app, KeyCode::Char('j'));
    let action = app.on_key(key(KeyCode::Enter));
    app.apply(action);

    // The pending path took over: the fetching line names the entry by its
    // title — never the bare id — and the queue stands where the user put
    // it. A synchronous open on this thread would have built a player (or
    // failed one) here and cleared the queue on failure.
    assert_eq!(
        app.status(),
        "⟳ fetching Dawn…",
        "the resolve waits on the worker, and the line names the entry"
    );
    assert_eq!(
        app.queue().len(),
        1,
        "the queue is untouched while the resolve runs: {:?}",
        app.queue()
    );

    // The worker's failure lands on the tick as the tool's own sentence,
    // one line, and the queue was never advanced behind the user's back.
    assert!(
        until(&mut app, |app| app
            .status()
            .contains("yt-dlp is not installed")),
        "the worker's answer names the reason, got: {}",
        app.status()
    );
    assert_eq!(
        app.queue().len(),
        1,
        "a failed fetch does not clear the queue"
    );

    press(&mut app, KeyCode::Esc);
    let _ = std::fs::remove_dir_all(&dir);
}

/// S7: nothing about the query is stored — no config write, no history
/// write, nothing on disk that carries what was typed.
#[test]
fn the_query_never_reaches_the_config_file_or_the_history_file() {
    let dir = scratch("privacy");
    let config = dir.join("config.toml");
    std::fs::write(&config, "volume = 40\n").expect("the config file is written");
    let program = fake(
        &dir,
        &format!("#!/bin/sh\ncat <<'JSON'\n{SEARCH_ANSWER}\nJSON\n"),
    );
    let mut app = app();
    app.set_youtube_program(Some(program));
    app.set_config_file(Some(config.clone()));
    app.open_sources();
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Enter); // the screen opens

    for character in "my secret query".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    press(&mut app, KeyCode::Enter);
    assert!(
        until(&mut app, |app| screen_of(app).contains("Dawn")),
        "the search answered"
    );
    app.tick();
    app.tick();

    assert_eq!(
        std::fs::read_to_string(&config).expect("the config file is read"),
        "volume = 40\n",
        "the config file is untouched by the search"
    );
    assert!(
        !velo::history::path_beside(&config).exists(),
        "the history file never appears"
    );
    assert_eq!(
        dir.read_dir().expect("the folder is read").count(),
        2,
        "the only files in the folder are the two the test wrote: {:?}",
        dir.read_dir()
            .expect("the folder is read")
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>()
    );

    press(&mut app, KeyCode::Esc);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The playing entry marks its source (T5): `playing_source` answers
/// `Source::Youtube` while one plays, so the row carries the playing mark.
#[test]
fn a_playing_youtube_entry_marks_the_youtube_row() {
    let mut app = app();
    app.show_playing_for_tests(
        Entry::Youtube {
            path: PathBuf::from("youtube://abc123"),
            title: String::from("Dawn"),
            uploader: Some(String::from("Chill Channel")),
            duration: Some(212),
        },
        Duration::ZERO,
        Some(Duration::from_secs(212)),
        None,
    );
    app.open_sources();

    let screen = screen_of(&app);
    let line = row_line(&screen, "YouTube");
    assert!(
        line.contains("▶"),
        "the playing entry marks the YouTube row, got:\n{screen}"
    );
    assert!(
        !row_line(&screen, "Radio").contains("▶"),
        "no other row wears the mark, got:\n{screen}"
    );
}

/// S3's live case and the pipe route's price (L9): any `YouTube` entry —
/// live or finite — plays the tool's own pipe, which has no position to seek
/// to, so a seek is refused with one sentence instead of rebuilding the
/// player at an offset the stream cannot honour: a fresh open at zero would
/// silently restart the track. The refusal says what is true: the stream
/// comes from the tool, and seeking it lands in a later slice.
#[test]
fn a_youtube_entry_refuses_a_seek_with_one_sentence() {
    for (name, duration) in [("Streaming now", None), ("Dawn", Some(212))] {
        let mut app = app();
        app.show_playing_for_tests(
            Entry::Youtube {
                path: PathBuf::from("youtube://live1"),
                title: String::from(name),
                uploader: Some(String::from("Evening")),
                duration,
            },
            Duration::ZERO,
            duration.map(Duration::from_secs),
            None,
        );

        press(&mut app, KeyCode::Right);

        assert_eq!(
            app.status(),
            "velo: a YouTube stream comes from yt-dlp and seeking it lands in a later slice",
            "the refusal says itself, once, whatever length the entry carries"
        );
    }
}

/// `ctrl+s` on a `YouTube` row says what the row is — never "already a file on
/// disk", and never a save prompt for something that cannot be copied.
#[test]
fn ctrl_s_on_a_youtube_row_says_what_it_is() {
    let mut app = app(); // no server configured; the resolver is the YouTube side
    app.set_resolver(Some(std::sync::Arc::new(
        velo::youtube::resolve::Resolver::new(PathBuf::from("/velo-test/does-not-exist/yt-dlp")),
    )));

    // Both shapes a YouTube row reaches the interface as: the entry the search
    // queued, and the one a stored path rebuilds.
    for row in [
        Entry::Youtube {
            path: PathBuf::from("youtube://abc123"),
            title: String::from("A video"),
            uploader: None,
            duration: Some(9),
        },
        Entry::File(PathBuf::from("youtube://abc123")),
    ] {
        app.show_playing_for_tests(row, Duration::ZERO, None, None);
        let action = app.on_key(ctrl(KeyCode::Char('s')));
        app.apply(action);

        assert!(
            !app.save_prompt_open(),
            "a resolved stream is not offered a save prompt: {}",
            app.status()
        );
        assert!(
            app.status().contains("resolved YouTube stream"),
            "the row is named for what it is: got {}",
            app.status()
        );
        assert!(
            !app.status().contains("already a file on disk"),
            "the file sentence never appears: {}",
            app.status()
        );
    }
}

// ---- the two modes ----------------------------------------------------------

/// One video description as `--dump-single-json` answers it: what the
/// worker's answer turns into one queue entry.
const VIDEO_OBJECT: &str = r#"{"id":"abc123def45","title":"Dawn","uploader":"Chill Channel","duration":212.0,"live_status":null}"#;

/// A broadcast description: no length, and `live_status` naming it live.
const LIVE_OBJECT: &str = r#"{"id":"live1234567","title":"Streaming now","channel":"Evening","duration":null,"live_status":"is_live"}"#;

/// Types one query and starts its search, leaving the screen in the
/// browsing mode a started search enters.
fn search_from_the_row(dir: &Path, body: &str, query: &str) -> (App<'static>, PathBuf) {
    let (mut app, program) = app_on_the_row(
        dir,
        &format!("#!/bin/sh\ncat <<'JSON'\n{SEARCH_ANSWER}\nJSON\n{body}"),
    );
    press(&mut app, KeyCode::Enter); // the screen opens
    for character in query.chars() {
        press(&mut app, KeyCode::Char(character));
    }
    press(&mut app, KeyCode::Enter); // the search starts: browsing
    (app, program)
}

/// Typing is the mode the screen opens in, and every printable — `a`, `?`,
/// `u` and `j` among them — lands in the query, because typing owns the
/// keyboard until something refocuses away from the query.
#[test]
fn typing_mode_takes_every_printable_into_the_query() {
    let dir = scratch("typing");
    let (mut app, program) = app_on_the_row(&dir, "#!/bin/sh\ntrue\n");
    press(&mut app, KeyCode::Enter); // the screen opens

    assert!(app.youtube_typing(), "the screen opens typing");
    for character in ['a', '?', 'u', 'j'] {
        press(&mut app, KeyCode::Char(character));
    }

    assert_eq!(app.youtube_query(), Some("a?uj"), "every key typed");
    assert!(app.youtube_typing());
    assert!(!app.help_open(), "`?` typed, not the help");
    assert!(!app.url_prompt_open(), "`u` typed, not the URL prompt");

    // `enter` in typing mode searches what is typed: not a YouTube thing,
    // so it is a query, never a load.
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.status(), "⟳ searching a?uj…");

    press(&mut app, KeyCode::Esc);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = program;
}

/// A started search enters browsing: `?` opens the help, `a` does what `a`
/// does on the sources screen it sits over, `u` opens the URL prompt, `j`
/// moves the results and `enter` plays the selected one — and nothing of it
/// lands in the query.
#[test]
fn after_the_search_starts_the_app_s_own_keys_fall_through() {
    let dir = scratch("browsing");
    let (mut app, program) = search_from_the_row(&dir, "", "dawn");

    assert!(!app.youtube_typing(), "a started search leaves the query");
    assert_eq!(app.youtube_query(), Some("dawn"));

    // `?` opens the help, the way it does everywhere else.
    press(&mut app, KeyCode::Char('?'));
    assert!(app.help_open(), "help opens from browsing");
    assert_eq!(app.youtube_query(), Some("dawn"), "the query is untouched");
    press(&mut app, KeyCode::Esc); // the help closes

    // `a` does what `a` does on the sources screen underneath — which
    // swallows it — and never types into the query.
    let status = app.status().to_owned();
    press(&mut app, KeyCode::Char('a'));
    assert_eq!(app.youtube_query(), Some("dawn"));
    assert_eq!(app.status(), status);
    assert_eq!(app.queue().len(), 0);

    // `u` opens the URL prompt, the global key it is.
    press(&mut app, KeyCode::Char('u'));
    assert!(app.url_prompt_open());
    press(&mut app, KeyCode::Esc);

    // `j` moves onto the first result and `enter` plays it — once the
    // results have landed, the way the worker hands them over.
    assert!(
        until(&mut app, |app| screen_of(app).contains("Dawn")),
        "the search answered"
    );
    press(&mut app, KeyCode::Char('j'));
    let action = app.on_key(key(KeyCode::Enter));
    match &action {
        Action::PlayQueue { from } => {
            assert_eq!(*from, 0, "the selected result is the one that plays");
        }
        other => panic!("enter in browsing plays the result, got {other:?}"),
    }
    assert_eq!(app.queue().len(), 1, "one result, one entry");

    press(&mut app, KeyCode::Esc);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = program;
}

/// `/` returns to typing with the current query, and what is typed then
/// appends — `j` typing again among them.
#[test]
fn slash_returns_to_typing_and_typing_appends() {
    let dir = scratch("refocus");
    let (mut app, program) = search_from_the_row(&dir, "", "dawn");
    assert!(until(&mut app, |app| screen_of(app).contains("Dawn")));

    press(&mut app, KeyCode::Char('/'));
    assert!(app.youtube_typing(), "`/` refocuses the query");
    assert_eq!(app.youtube_query(), Some("dawn"));

    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.youtube_query(), Some("dawnj"), "typing appends");

    press(&mut app, KeyCode::Esc);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = program;
}

// ---- the load: a URL or an id, from both doors ------------------------------

/// The `u` prompt with a `YouTube` URL opens the screen and loads the one
/// video it names: one `Entry::Youtube` queued, played from its row, the
/// user landed on the screen.
#[test]
fn the_url_prompt_loads_a_youtube_url_through_the_screen() {
    let dir = scratch("url-load");
    let (mut app, program) = app_on_the_row(
        &dir,
        &format!("#!/bin/sh\ncat <<'JSON'\n{VIDEO_OBJECT}\nJSON\n"),
    );
    // The session's resolver is the YouTube side over a program that is
    // not there: the resolve waits on its worker and the queue stands,
    // which is what makes the queued entry observable (no audio device
    // opens in a test).
    app.set_resolver(Some(std::sync::Arc::new(
        velo::youtube::resolve::Resolver::new(PathBuf::from("/velo-test/does-not-exist/yt-dlp")),
    )));
    press(&mut app, KeyCode::Char('u'));
    assert!(app.url_prompt_open());
    for character in "https://youtu.be/abc123def45".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    press(&mut app, KeyCode::Enter);

    assert!(
        !app.url_prompt_open(),
        "an accepted answer closes the prompt"
    );
    assert!(app.youtube_open(), "the screen opens for the load");
    assert!(
        until(&mut app, |app| !app.queue().is_empty()),
        "the described video queues, got: {}",
        app.status()
    );
    assert_eq!(app.view(), View::Sources, "the user lands on the screen");
    assert_eq!(
        app.queue(),
        vec![Entry::Youtube {
            path: PathBuf::from("youtube://abc123def45"),
            title: String::from("Dawn"),
            uploader: Some(String::from("Chill Channel")),
            duration: Some(212),
        }],
        "one entry, the identity and the body the description said"
    );

    press(&mut app, KeyCode::Esc);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = program;
}

/// An `http` URL keeps the prompt's own path byte for byte: no `YouTube`
/// screen, the ordinary station machinery — here a refusal, the machine
/// having no server to answer.
#[test]
fn the_url_prompt_keeps_its_own_path_for_an_http_url() {
    let dir = scratch("url-http");
    let (mut app, program) = app_on_the_row(&dir, "#!/bin/sh\ntrue\n");
    press(&mut app, KeyCode::Char('u'));
    for character in "http://127.0.0.1:1/stream".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    press(&mut app, KeyCode::Enter);

    assert!(
        !app.youtube_open(),
        "an http URL never opens the YouTube screen"
    );
    assert_eq!(
        app.queue(),
        [] as [Entry; 0],
        "a refused URL queues nothing"
    );
    assert!(
        app.status().contains("velo:"),
        "the ordinary path's refusal stands, got: {}",
        app.status()
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = program;
}

/// The search field with a `YouTube` URL loads that video instead of
/// searching for the text; a normal query still searches.
#[test]
fn a_youtube_url_in_the_query_loads_and_a_query_still_searches() {
    let dir = scratch("query-load");
    let (mut app, program) = app_on_the_row(
        &dir,
        &format!("#!/bin/sh\ncat <<'JSON'\n{VIDEO_OBJECT}\nJSON\n"),
    );
    app.set_resolver(Some(std::sync::Arc::new(
        velo::youtube::resolve::Resolver::new(PathBuf::from("/velo-test/does-not-exist/yt-dlp")),
    )));
    press(&mut app, KeyCode::Enter); // the screen opens
    for character in "youtube.com/watch?v=abc123def45".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    press(&mut app, KeyCode::Enter);

    assert!(
        until(&mut app, |app| !app.queue().is_empty()),
        "the URL loads, got: {}",
        app.status()
    );
    assert_eq!(
        app.queue(),
        vec![Entry::Youtube {
            path: PathBuf::from("youtube://abc123def45"),
            title: String::from("Dawn"),
            uploader: Some(String::from("Chill Channel")),
            duration: Some(212),
        }],
        "the load queued one entry, the search never ran"
    );

    press(&mut app, KeyCode::Esc);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = program;
}

/// A live description pasted into the `u` prompt is refused with the
/// existing live sentence, queues nothing, and opens nothing: the screen it
/// needed as a worker closes, the user stays where they were.
#[test]
fn a_live_url_in_the_prompt_is_refused_and_queues_nothing() {
    let dir = scratch("url-live");
    let (mut app, program) = app_on_the_row(
        &dir,
        &format!("#!/bin/sh\ncat <<'JSON'\n{LIVE_OBJECT}\nJSON\n"),
    );
    press(&mut app, KeyCode::Char('u'));
    for character in "https://youtu.be/live1234567".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    press(&mut app, KeyCode::Enter);

    assert!(
        until(&mut app, |app| app.status().contains("is live")),
        "the live sentence stands, got: {}",
        app.status()
    );
    assert_eq!(
        app.queue(),
        [] as [Entry; 0],
        "a refused live queues nothing"
    );
    assert!(
        !app.youtube_open(),
        "a refused load opens nothing over the sources"
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = program;
}

/// A live id typed into the query is refused the same way: one sentence,
/// nothing queued, the screen stays where it was.
#[test]
fn a_live_id_in_the_query_is_refused_and_queues_nothing() {
    let dir = scratch("query-live");
    let (mut app, program) = app_on_the_row(
        &dir,
        &format!("#!/bin/sh\ncat <<'JSON'\n{LIVE_OBJECT}\nJSON\n"),
    );
    press(&mut app, KeyCode::Enter); // the screen opens
    for character in "youtube.com/watch?v=live1234567".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    press(&mut app, KeyCode::Enter);

    assert!(
        until(&mut app, |app| app.status().contains("is live")),
        "the live sentence stands, got: {}",
        app.status()
    );
    assert_eq!(
        app.queue(),
        [] as [Entry; 0],
        "a refused live queues nothing"
    );
    assert!(app.youtube_open(), "the screen the query lives on stays");

    press(&mut app, KeyCode::Esc);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = program;
}

/// A paste that arrives while a job is already running is refused, and the
/// refusal is said: the prompt closed, the status names why the paste was
/// not loaded — never the searching line standing in its place — and the
/// screen stays usable, the running search's answer still landing on it.
#[test]
fn a_paste_refused_while_a_job_is_running_says_so_and_the_screen_stays_usable() {
    let dir = scratch("busy");
    // The search answer takes its time, so a job is in flight when the
    // paste arrives.
    let body = format!("#!/bin/sh\nsleep 2\ncat <<'JSON'\n{SEARCH_ANSWER}\nJSON\n");
    let (mut app, program) = app_on_the_row(&dir, &body);
    press(&mut app, KeyCode::Enter); // the screen opens
    for character in "dawn".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    press(&mut app, KeyCode::Enter); // the search starts: browsing
    assert_eq!(app.status(), "⟳ searching dawn…", "the job is in flight");

    // The `u` prompt: paste a YouTube URL while the search runs.
    press(&mut app, KeyCode::Char('u'));
    assert!(
        app.url_prompt_open(),
        "`u` opens the prompt while a job runs"
    );
    for character in "https://youtu.be/abc123def45".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    press(&mut app, KeyCode::Enter);

    assert!(!app.url_prompt_open(), "the prompt closed on submit");
    assert_eq!(
        app.status(),
        "velo: a job is already running — the YouTube link was not loaded",
        "the refusal is said, not swallowed by the searching line: {}",
        app.status()
    );

    // The screen stays usable: the running search still answers, nothing
    // was queued, and the refusal stands beside it — never replaced by the
    // searching line mid-flight.
    assert!(
        until(&mut app, |app| screen_of(app).contains("Dawn")),
        "the running search still answers, got:\n{}",
        screen_of(&app)
    );
    assert!(app.youtube_open(), "the screen stays open and usable");
    assert_eq!(
        app.queue(),
        [] as [Entry; 0],
        "the refused paste queued nothing"
    );
    assert_eq!(
        app.status(),
        "velo: a job is already running — the YouTube link was not loaded",
        "the refusal still stands once the answer landed, got: {}",
        app.status()
    );

    press(&mut app, KeyCode::Esc);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = program;
}

// ---- what is playing is visible ---------------------------------------------

/// The screen names what is playing when the playing entry is a `YouTube`
/// one, and says nothing when it is not — the shell reads the entry, the
/// screen draws the line, and the screen holds no queue.
#[test]
fn the_screen_names_what_is_playing_when_it_is_a_youtube_entry() {
    let dir = scratch("now-playing");
    let (mut app, program) = app_on_the_row(&dir, "#!/bin/sh\ntrue\n");

    // Nothing playing: no line.
    press(&mut app, KeyCode::Enter); // the screen opens
    let screen = screen_of(&app);
    assert!(
        !screen.contains("Streaming now"),
        "nothing playing, nothing named, got:\n{screen}"
    );
    press(&mut app, KeyCode::Esc);

    // A YouTube entry standing: its title is on the screen, beside the
    // source rows it was queued from.
    app.show_playing_for_tests(
        Entry::Youtube {
            path: PathBuf::from("youtube://live1234567"),
            title: String::from("Streaming now"),
            uploader: Some(String::from("Evening")),
            duration: None,
        },
        Duration::ZERO,
        None,
        None,
    );
    press(&mut app, KeyCode::Enter);
    let screen = screen_of(&app);
    assert!(
        screen.contains("▶ Streaming now"),
        "the line names what is playing, got:\n{screen}"
    );

    // A file standing: the line is silent — the file is the app's
    // now-playing block's to name, never the screen's line, and no
    // invented name appears in its place.
    app.show_playing_for_tests(
        Entry::File(PathBuf::from("/music/somewhere/a-flac.flac")),
        Duration::ZERO,
        None,
        None,
    );
    let screen = screen_of(&app);
    assert!(
        !screen.contains("▶ Streaming now") && !screen.contains("▶ a-flac"),
        "a non-YouTube entry is not the screen's to name, got:\n{screen}"
    );

    press(&mut app, KeyCode::Esc);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = program;
}

/// An eleven-character query that parses as a video id but is not one falls
/// back to the search the user meant: `dawn-chorus` is eleven characters of
/// `[A-Za-z0-9_-]`, so the describe is attempted — and when the tool refuses
/// it, the search runs instead of the refusal standing.
#[test]
fn a_query_that_parses_as_an_id_but_is_not_one_falls_back_to_searching() {
    let dir = scratch("fallback");
    // The describe refuses in the tool's own words; the search answers.
    let body = format!(
        "#!/bin/sh\ncase \"$*\" in\n  *--flat-playlist*) cat <<'JSON'\n{SEARCH_ANSWER}\nJSON\n    ;;\n  *) printf 'ERROR: [youtube] dawn-chor: Video unavailable\\n' >&2; exit 1 ;;\nesac\n"
    );
    let (mut app, program) = app_on_the_row(&dir, &body);
    press(&mut app, KeyCode::Enter); // the screen opens
    for character in "dawn-chorus".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    assert_eq!(
        velo::youtube::input_video_id("dawn-chorus").as_deref(),
        Some("dawn-chorus"),
        "the phrase does parse as an id: that is why it needs the fallback"
    );
    press(&mut app, KeyCode::Enter);

    assert!(
        until(&mut app, |app| screen_of(app).contains("Nightfall")),
        "the search ran after the describe refused, got: {}",
        app.status()
    );
    assert_eq!(
        app.queue(),
        [] as [Entry; 0],
        "a failed describe queues nothing on its own"
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = program;
}

/// `a` on a result appends it to the queue without playing it, and `A` appends
/// the whole answer — the live one refused, the rest in order. Typing still
/// types them: `/` brings the keyboard back to the query.
#[test]
fn a_appends_a_result_and_upper_a_appends_the_answer() {
    let dir = scratch("append");
    // The helper already answers with SEARCH_ANSWER: one recorded result and
    // one live one.
    let (mut app, program) = search_from_the_row(&dir, "", "dawn");

    assert!(
        until(&mut app, |app| screen_of(app).contains("Dawn")),
        "the answer lands before anything is appended: {}",
        app.status()
    );
    assert_eq!(app.queue(), [] as [Entry; 0], "a search queues nothing");

    // `a` on the first result: queued, not played.
    press(&mut app, KeyCode::Char('j')); // off the query row, onto the first result
    assert_eq!(app.on_key(KeyCode::Char('a').into()), Action::None);
    assert_eq!(
        app.queue().len(),
        1,
        "one entry appended, got: {}",
        app.status()
    );
    match &app.queue()[0] {
        Entry::Youtube { path, title, .. } => {
            assert_eq!(path, &PathBuf::from("youtube://abc123"));
            assert_eq!(title, "Dawn");
        }
        other => panic!("a result appends as a YouTube entry, got {other:?}"),
    }
    assert!(
        app.status().contains("added 1 result"),
        "the append says what it did: {}",
        app.status()
    );

    // `A`: the whole answer, the live one refused in its own sentence.
    assert_eq!(app.on_key(KeyCode::Char('A').into()), Action::None);
    assert_eq!(
        app.queue().len(),
        2,
        "only the recorded result joins, got: {}",
        app.status()
    );
    assert_eq!(
        app.status().matches("added").count(),
        1,
        "the append counted the recorded result: {}",
        app.status()
    );
    assert!(
        app.status().contains("skipped 1 live broadcast"),
        "the live one is named, not silently dropped: {}",
        app.status()
    );

    // And typing still types: `/` comes back to the query and `a` is a letter.
    press(&mut app, KeyCode::Char('/'));
    press(&mut app, KeyCode::Char('a'));
    assert_eq!(
        app.youtube_query(),
        Some("dawna"),
        "the letter went back into the query instead of appending"
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = program;
}
