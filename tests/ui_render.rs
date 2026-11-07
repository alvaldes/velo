//! What the shell actually puts on the screen (S1, S13).
//!
//! A terminal interface can be tested without a terminal: ratatui's test backend
//! renders a frame into a buffer, so the layout, the empty state and the key
//! hints are asserted instead of eyeballed. These tests exist because the
//! interface was first verified by construction, and construction never noticed
//! a separator dangling in the footer.

use std::path::PathBuf;

use velo::ui::App;
use velo::ui::ratatui::Terminal;
use velo::ui::ratatui::backend::TestBackend;
use velo::ui::ratatui::buffer::{Buffer, Cell};

fn render(app: &App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(90, 12)).expect("a test terminal");
    terminal
        .draw(|frame| app.draw(frame))
        .expect("the frame draws");
    let buffer: &Buffer = terminal.backend().buffer();
    buffer
        .content()
        .iter()
        .map(Cell::symbol)
        .collect::<String>()
}

fn tracks(names: &[&str]) -> Vec<PathBuf> {
    names
        .iter()
        .map(|name| PathBuf::from(format!("/music/{name}")))
        .collect()
}

#[test]
fn the_shell_shows_its_name_the_tracks_the_state_and_the_keys() {
    let app = App::new(tracks(&["one.flac", "two.flac"]));
    let screen = render(&app);

    for expected in [
        "velo", "2 tracks", "one.flac", "two.flac", "stopped", "j/k move", "q quit",
    ] {
        assert!(
            screen.contains(expected),
            "the shell must show {expected:?} without a keypress"
        );
    }
}

#[test]
fn the_footer_has_no_dangling_separator() {
    let app = App::new(tracks(&["one.flac"]));
    let screen = render(&app);

    // A leading comma means a separator survived the removal of whatever used
    // to come before it, and reads as a typo on screen.
    assert!(
        !screen.contains(", stopped"),
        "the state must not be prefixed by an orphaned separator"
    );
    assert!(!screen.contains(", playing"));
    assert!(!screen.contains(", paused"));
}

#[test]
fn an_empty_library_draws_an_empty_state_instead_of_panicking() {
    let app = App::new(Vec::new());
    let screen = render(&app);

    assert!(screen.contains("0 tracks"), "the count is still shown");
    assert!(
        screen.contains("no tracks"),
        "an empty list explains itself instead of rendering nothing"
    );
}

#[test]
fn the_help_overlay_lists_the_keys_and_covers_the_list() {
    let mut app = App::new(tracks(&["one.flac"]));
    let quiet = render(&app);

    app.on_key(velo::ui::crossterm::event::KeyEvent::from(
        velo::ui::crossterm::event::KeyCode::Char('?'),
    ));
    let helped = render(&app);

    assert!(app.help_open(), "the question mark opens the help");
    assert_ne!(quiet, helped, "the help changes what is on screen");
    assert!(
        helped.contains("first track"),
        "the overlay documents the keys it claims to"
    );
}
