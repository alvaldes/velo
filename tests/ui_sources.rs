//! The sources screen (issue #3), tested without a terminal: what the first
//! screen shows, what `enter` and `esc` do to it, and that the library's and
//! the radio's own keys are untouched once a source is open.

use velo::ui::crossterm::event::{KeyCode, KeyEvent};
use velo::ui::{Action, App, BrowseBy, Level, View};

fn app() -> App<'static> {
    App::browse(None)
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
/// already on the first row.
#[test]
fn the_sources_screen_names_both_sources_with_the_cursor_on_the_first() {
    let mut app = app();
    app.open_sources();

    assert_eq!(app.view(), View::Sources);
    assert_eq!(app.sources_labels(), ["Local library", "Radio"]);
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
    assert_eq!(app.sources_selected(), 1);
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.sources_selected(), 1, "j stops at the last source");
    press(&mut app, KeyCode::Char('k'));
    assert_eq!(app.sources_selected(), 0);
    press(&mut app, KeyCode::Char('G'));
    assert_eq!(app.sources_selected(), 1);
    press(&mut app, KeyCode::Char('g'));
    assert_eq!(app.sources_selected(), 0);
}

/// S3, S7: entering the local library lands on its artists view, and the
/// library's own keys still switch the four views inside it.
#[test]
fn entering_the_local_library_lands_on_the_library_and_its_keys_still_work() {
    let mut app = app();
    app.open_sources();

    press(&mut app, KeyCode::Enter);
    assert_eq!(app.view(), View::Library);
    assert_eq!(app.level(), Some(Level::Artists));

    assert_eq!(
        app.on_key(key(KeyCode::Char('2'))),
        Action::Browse(BrowseBy::Albums),
        "the four view keys belong to the library again (S7)"
    );
    app.apply(Action::Browse(BrowseBy::Albums));
    assert_eq!(app.level(), Some(Level::AllAlbums));
}

/// S6: `esc` walks back to the sources screen and stays there — the way out
/// of a screen is the way in.
#[test]
fn esc_from_the_top_of_the_library_lands_on_sources_and_stays() {
    let mut app = app();
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
    let mut app = app();
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

/// `tab` switches the library and the queue; on the sources screen it is
/// inert, because neither list is what the keys act on.
#[test]
fn tab_is_inert_on_the_sources_screen() {
    let mut app = app();
    app.open_sources();

    press(&mut app, KeyCode::Tab);
    assert_eq!(app.view(), View::Sources);
}
