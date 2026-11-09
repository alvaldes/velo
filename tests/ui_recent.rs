//! The `Recently played` source (issue #5, T2): the history's entries as
//! rows that play, queue and walk back like any other track list.
//!
//! The history is produced without a player: `App::browse(None)` plus
//! `set_config_file(Some(<temp>/velo/config.toml))` loads the file beside
//! the config the way a launch does, and `history_tick` records into it
//! headlessly. The playing marker (S4) is not tested here: `playing_path()`
//! needs a real player, and no TUI test has one.

use std::fs;
use std::path::{Path, PathBuf};

use velo::audio::output::Entry;
use velo::ui::crossterm::event::{KeyCode, KeyEvent};
use velo::ui::ratatui::Terminal;
use velo::ui::ratatui::backend::TestBackend;
use velo::ui::ratatui::buffer::Cell;
use velo::ui::{Action, App, Level, View};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::from(code)
}

/// One key press, its action applied.
fn press(app: &mut App<'_>, code: KeyCode) -> Action {
    let action = app.on_key(key(code));
    app.apply(action);
    action
}

/// A fresh temp directory, removed again when the test ends.
fn temp_dir(tag: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("velo-ui-recent-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

/// An app with a config file whose history the test has written: the same
/// file `velo history` reads, loaded the way a launch loads it.
fn app_with_history(dir: &Path, document: &str) -> App<'static> {
    let config = dir.join("velo").join("config.toml");
    let history = velo::history::path_beside(&config);
    fs::create_dir_all(history.parent().unwrap()).unwrap();
    fs::write(&history, document).unwrap();
    let mut app = App::browse(None);
    app.set_config_file(Some(config));
    app
}

/// Opens the third source with the cursor keys, the way the user does.
fn open_recently_played(app: &mut App<'static>) {
    app.open_sources();
    press(app, KeyCode::Char('j'));
    press(app, KeyCode::Char('j'));
    assert_eq!(press(app, KeyCode::Enter), Action::None);
}

/// What the screen shows, as one string.
fn screen(app: &App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(90, 12)).expect("a test terminal");
    terminal
        .draw(|frame| app.draw(frame))
        .expect("the frame draws");
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(Cell::symbol)
        .collect()
}

const HISTORY: &str = concat!(
    "[[entry]]\n",
    "path = \"/elsewhere/tide/01 - Nightfall.flac\"\n",
    "played_at = 20\n",
    "artist = \"Tide\"\n",
    "album = \"Harbor\"\n",
    "title = \"Nightfall\"\n",
    "\n",
    "[[entry]]\n",
    "path = \"/somewhere/else/song.mp3\"\n",
    "played_at = 10\n",
);

/// The third source opens the recent list: one row per entry, newest first,
/// following the library's row convention — the title with `artist · album`
/// dimmed after it, and the file's own name over its folder for a path the
/// index never knew. The metadata is written into the file by hand, because
/// `history_tick` can only describe a path a library row names and this
/// test has no library.
#[test]
fn the_recent_list_shows_the_history_newest_first_with_the_library_row_convention() {
    let dir = temp_dir("rows");
    let mut app = app_with_history(&dir, HISTORY);
    open_recently_played(&mut app);

    assert_eq!(app.view(), View::Library);
    assert_eq!(app.level(), Some(Level::Recent));
    assert_eq!(app.selected(), 0);
    assert_eq!(
        app.tracks()
            .iter()
            .map(|track| track.path.as_path())
            .collect::<Vec<_>>(),
        [
            Path::new("/elsewhere/tide/01 - Nightfall.flac"),
            Path::new("/somewhere/else/song.mp3"),
        ],
        "the entries are the history's, newest first"
    );

    let screen = screen(&app);
    assert!(
        screen.contains("Nightfall") && screen.contains("Tide · Harbor"),
        "the title with its artist · album note, got: {screen}"
    );
    assert!(
        screen.contains("song.mp3") && screen.contains("/somewhere/else"),
        "a path the index never knew keeps its file name over its folder, got: {screen}"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// The list is the tick's in-memory snapshot: entries `history_tick`
/// recorded for paths outside the library appear newest first, and the
/// screen never re-reads the file to show them.
#[test]
fn the_list_shows_what_the_tick_recorded_newest_first() {
    let dir = temp_dir("tick");
    let config = dir.join("velo").join("config.toml");
    let mut app = App::browse(None);
    app.set_config_file(Some(config));

    let first = PathBuf::from("/elsewhere/tide/01 - Nightfall.flac");
    let second = PathBuf::from("/elsewhere/tide/02 - Later.flac");
    app.history_tick(Some(first.clone()));
    app.history_tick(Some(second.clone()));

    open_recently_played(&mut app);
    assert_eq!(app.level(), Some(Level::Recent));
    assert_eq!(app.tracks().len(), 2);
    assert_eq!(app.tracks()[0].path, second, "newest first");
    assert_eq!(app.tracks()[1].path, first);
    assert_eq!(
        app.tracks()[0].title,
        "",
        "no title is invented: the row names the file instead"
    );
    assert_eq!(app.tracks()[0].artist, "", "the index knew nothing");
    assert_eq!(app.tracks()[0].album, "");
    let screen = screen(&app);
    assert!(
        screen.contains("02 - Later"),
        "the file's own name names an entry without a title, got: {screen}"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// `enter` on a row plays from it, answering `Play` for that track (S4);
/// the stream itself is the shell's to build, and no test has a device.
#[test]
fn enter_plays_from_the_selected_row() {
    let dir = temp_dir("enter");
    let mut app = app_with_history(&dir, HISTORY);
    open_recently_played(&mut app);

    assert_eq!(
        press(&mut app, KeyCode::Enter),
        Action::Play { from: 0 },
        "enter plays from the selection"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// `a` appends the selection's path to the queue, verbatim, without starting
/// a stream (S4).
#[test]
fn a_appends_the_selection_to_the_queue() {
    let dir = temp_dir("append");
    let mut app = app_with_history(&dir, HISTORY);
    open_recently_played(&mut app);

    press(&mut app, KeyCode::Char('a'));
    assert_eq!(
        app.queue(),
        [Entry::File(PathBuf::from(
            "/elsewhere/tide/01 - Nightfall.flac"
        ))],
        "the queue holds the history's path, verbatim"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// `esc` walks back to the sources screen (S6): the way out is the way in.
#[test]
fn esc_returns_to_the_sources_screen() {
    let dir = temp_dir("esc");
    let mut app = app_with_history(&dir, HISTORY);
    open_recently_played(&mut app);

    press(&mut app, KeyCode::Esc);
    assert_eq!(app.view(), View::Sources);
    assert_eq!(
        app.level(),
        Some(Level::Artists),
        "the library's top is kept"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// An empty history opens an empty list that says so, not an error (S4).
#[test]
fn an_empty_history_opens_an_empty_list_that_says_so() {
    let dir = temp_dir("empty");
    let config = dir.join("velo").join("config.toml");
    let mut app = App::browse(None);
    app.set_config_file(Some(config)); // no history file beside it yet
    open_recently_played(&mut app);

    assert_eq!(app.level(), Some(Level::Recent));
    assert_eq!(app.tracks(), [] as [velo::library::TrackEntry; 0]);
    let screen = screen(&app);
    assert!(
        screen.contains("nothing played yet"),
        "the empty list explains itself, got: {screen}"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// A `subsonic://…` entry keeps its path verbatim — in the row's track and
/// in the queue entry `a` appends — so the resolver still caches it at play
/// time (S5).
#[test]
fn a_subsonic_entry_keeps_its_path_verbatim() {
    let dir = temp_dir("subsonic");
    let url = "subsonic://music.example/rest/stream?id=42";
    let mut app = app_with_history(
        &dir,
        concat!(
            "[[entry]]\n",
            "path = \"subsonic://music.example/rest/stream?id=42\"\n",
            "played_at = 5\n",
            "artist = \"Tide\"\n",
            "album = \"Harbor\"\n",
            "title = \"Nightfall\"\n",
        ),
    );
    open_recently_played(&mut app);

    assert_eq!(
        app.tracks()[0].path,
        Path::new(url),
        "the row's track keeps the URL as its path"
    );

    press(&mut app, KeyCode::Char('a'));
    assert_eq!(
        app.queue(),
        [Entry::File(PathBuf::from(url))],
        "the queue holds the URL, never a resolved cache file"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn replaying_from_the_list_writes_no_metadata_the_history_did_not_have() {
    let dir = temp_dir("no-invented-metadata");
    let mut app = app_with_history(&dir, HISTORY);
    open_recently_played(&mut app);

    // The second row is the entry the index never described: the screen names
    // it from the file itself, and the row it synthesizes carries no title.
    // Beginning that path again must not write back a made-up title, nor a
    // blank artist: an absent field stays absent (S3).
    let path = PathBuf::from("/somewhere/else/song.mp3");
    app.history_tick(Some(path.clone()));

    let top = &app.recently_played()[0];
    assert_eq!(
        top.path,
        path.to_string_lossy(),
        "the beginning is recorded"
    );
    assert_eq!(top.artist, None, "no artist is invented for a bare path");
    assert_eq!(top.album, None, "no album is invented for a bare path");
    assert_eq!(top.title, None, "the file's own name is not a tag");

    let _ = fs::remove_dir_all(&dir);
}
