//! The sources screen (issue #3), tested without a terminal: what the first
//! screen shows, what `enter` and `esc` do to it, and that the library's and
//! the radio's own keys are untouched once a source is open.

use velo::ui::crossterm::event::{KeyCode, KeyEvent};
use velo::ui::{Action, App, BrowseBy, Level, View};

fn app() -> App<'static> {
    App::browse(None)
}

/// An app with a library configured but no prompt to answer: entering
/// `Local library` opens no path card (S12).
fn app_with_a_library() -> App<'static> {
    let mut app = app();
    app.set_library_root(Some(std::path::PathBuf::from("/music")));
    app
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::from(code)
}

/// One key press, its action applied.
fn press(app: &mut App<'_>, code: KeyCode) -> Action {
    let action = app.on_key(key(code));
    app.apply(action);
    action
}

/// S1, S2: the first screen lists what velo can play, with the cursor
/// already on the first row. `Favourites` (issue #7) is last, so `j` from
/// the top still lands on `Radio`.
#[test]
fn the_sources_screen_names_the_sources_with_the_cursor_on_the_first() {
    let mut app = app();
    app.open_sources();

    assert_eq!(app.view(), View::Sources);
    assert_eq!(
        app.sources_labels(),
        ["Local library", "Radio", "Recently played", "Favourites"]
    );
    assert_eq!(
        app.sources_selected(),
        0,
        "the cursor starts on the first row"
    );
}

/// The cursor moves with the list keys and clamps at both ends.
#[test]
fn the_source_cursor_moves_with_the_list_keys_and_clamps() {
    let mut app = app();
    app.open_sources();

    press(&mut app, KeyCode::Char('j'));
    assert_eq!(
        app.sources_selected(),
        1,
        "the first j still lands on Radio"
    );
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.sources_selected(), 2);
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.sources_selected(), 3, "Favourites is the last source");
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.sources_selected(), 3, "j stops at the last source");
    press(&mut app, KeyCode::Char('k'));
    assert_eq!(app.sources_selected(), 2);
    press(&mut app, KeyCode::Char('G'));
    assert_eq!(app.sources_selected(), 3);
    press(&mut app, KeyCode::Char('g'));
    assert_eq!(app.sources_selected(), 0);
}

/// S3, S7: entering the local library lands on its artists view, and the
/// library's own keys still switch the four views inside it.
#[test]
fn entering_the_local_library_lands_on_the_library_and_its_keys_still_work() {
    let mut app = app_with_a_library();
    app.open_sources();

    press(&mut app, KeyCode::Enter);
    assert_eq!(app.view(), View::Library);
    assert_eq!(app.level(), Some(Level::Artists));
    assert!(
        !app.library_prompt_open(),
        "a configured library is not asked for again"
    );

    assert_eq!(
        app.on_key(key(KeyCode::Char('2'))),
        Action::Browse(BrowseBy::Albums),
        "the four view keys belong to the library again (S7)"
    );
    app.apply(Action::Browse(BrowseBy::Albums));
    assert_eq!(app.level(), Some(Level::AllAlbums));
}

/// S12 (issue #27) with S14: the recorded `view = "queue"` still parses but
/// is no longer applied — the session leaves the reader where the launch put
/// them, and entering `Local library` lands on the library, never on the
/// queue the file recorded.
#[test]
fn entering_the_local_library_lands_on_the_library_even_when_the_saved_tab_was_the_queue() {
    let mut app = app_with_a_library();
    app.set_session(velo::config::Session {
        view: View::Queue,
        ..velo::config::Session::default()
    });
    assert_eq!(
        app.view(),
        View::Library,
        "the retired tab is not applied (S14)"
    );

    // The shell opens `Sources` at launch; the library entry is behind it.
    app.open_sources();
    assert_eq!(app.view(), View::Sources);

    press(&mut app, KeyCode::Enter);
    assert_eq!(app.view(), View::Library);
    assert_eq!(app.level(), Some(Level::Artists));
}

/// S12 (issue #27): with no library configured, entering `Local library`
/// opens the prompt — the reader is asked where the music is instead of
/// being left somewhere else.
#[test]
fn entering_the_local_library_without_one_configured_asks_for_its_path() {
    let mut app = app(); // no open index, no configured root
    app.open_sources();
    press(&mut app, KeyCode::Enter);

    assert_eq!(app.view(), View::Library);
    assert!(
        app.library_prompt_open(),
        "the reader is asked where the music is"
    );
}

/// S6: `esc` walks back to the sources screen and stays there — the way out
/// of a screen is the way in.
#[test]
fn esc_from_the_top_of_the_library_lands_on_sources_and_stays() {
    let mut app = app_with_a_library();
    app.open_sources();
    press(&mut app, KeyCode::Enter);

    press(&mut app, KeyCode::Esc);
    assert_eq!(app.view(), View::Sources, "one step back reaches Sources");
    assert_eq!(
        app.level(),
        Some(Level::Artists),
        "the library keeps its level"
    );

    press(&mut app, KeyCode::Esc);
    assert_eq!(app.view(), View::Sources, "the top of all is Sources");
}

/// S6: closing the radio lands on the sources screen, not on the library.
#[test]
fn esc_closing_the_radio_lands_on_sources() {
    let mut app = app();
    app.open_sources();
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Enter);
    assert!(app.radio_open(), "enter on Radio opens the radio view");

    press(&mut app, KeyCode::Esc);
    assert!(!app.radio_open(), "esc closes the radio");
    assert_eq!(app.view(), View::Sources);
}

/// S7: `R` is still an accelerator from inside the library, and closing that
/// radio comes back to the library — the way out is the way in, not the
/// sources screen the user never crossed.
#[test]
fn r_still_opens_the_radio_from_inside_the_library() {
    let mut app = app_with_a_library();
    app.open_sources();
    press(&mut app, KeyCode::Enter);

    assert_eq!(app.on_key(key(KeyCode::Char('R'))), Action::None);
    assert!(app.radio_open());

    press(&mut app, KeyCode::Esc);
    assert!(!app.radio_open());
    assert_eq!(
        app.view(),
        View::Library,
        "the way out of the radio is the way in (S6)"
    );
}

/// S1: the flat command-line list has no sources screen above it — there is
/// nothing to choose from when the files were given.
#[test]
fn the_flat_files_list_never_shows_a_sources_screen() {
    let mut app = App::new(vec![std::path::PathBuf::from("/t/one.flac")]);
    app.open_sources();

    assert_eq!(
        app.view(),
        View::Library,
        "the file launch stays on its list"
    );
}

/// S8 (issue #23): `tab` is one round trip. From the sources screen it goes
/// to the queue, and `tab` again returns to the sources screen with its
/// cursor exactly where it was left.
#[test]
fn tab_from_the_sources_screen_reaches_the_queue_and_returns_to_it() {
    let mut app = app();
    app.open_sources();
    press(&mut app, KeyCode::Char('j')); // onto Radio
    assert_eq!(app.sources_selected(), 1);

    press(&mut app, KeyCode::Tab);
    assert_eq!(app.view(), View::Queue);

    press(&mut app, KeyCode::Tab);
    assert_eq!(app.view(), View::Sources);
    assert_eq!(app.sources_selected(), 1, "the cursor is where it was left");
}
