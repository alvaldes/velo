//! The keymap layer, tested without a terminal: every rule of
//! `App::on_key` observed through the returned [`Action`].

use std::path::PathBuf;

use velo::ui::crossterm::event::{KeyCode, KeyEvent};
use velo::ui::{Action, App};

fn app_with_three_tracks() -> App {
    App::new(vec![
        PathBuf::from("/library/artist/one.flac"),
        PathBuf::from("/library/artist/two.flac"),
        PathBuf::from("/library/artist/three.flac"),
    ])
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::from(code)
}

#[test]
fn j_moves_down_and_clamps_at_the_last_row() {
    let mut app = app_with_three_tracks();
    assert_eq!(app.selected(), 0);

    assert_eq!(app.on_key(key(KeyCode::Char('j'))), Action::None);
    assert_eq!(app.selected(), 1);
    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(app.selected(), 2);

    // At the last row `j` stays put instead of panicking or wrapping.
    assert_eq!(app.on_key(key(KeyCode::Char('j'))), Action::None);
    assert_eq!(app.selected(), 2);

    // `Down` is the same movement.
    let mut app = app_with_three_tracks();
    app.on_key(key(KeyCode::Down));
    assert_eq!(app.selected(), 1);
}

#[test]
fn k_moves_up_and_clamps_at_the_first_row() {
    let mut app = app_with_three_tracks();
    app.on_key(key(KeyCode::Char('j')));
    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(app.selected(), 2);

    assert_eq!(app.on_key(key(KeyCode::Char('k'))), Action::None);
    assert_eq!(app.selected(), 1);
    app.on_key(key(KeyCode::Char('k')));
    assert_eq!(app.selected(), 0);

    // At the first row `k` stays put.
    assert_eq!(app.on_key(key(KeyCode::Char('k'))), Action::None);
    assert_eq!(app.selected(), 0);

    // `Up` is the same movement.
    let mut app = app_with_three_tracks();
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Up));
    assert_eq!(app.selected(), 0);
}

#[test]
fn g_selects_the_first_track_and_g_the_last() {
    let mut app = app_with_three_tracks();
    app.on_key(key(KeyCode::Char('G')));
    assert_eq!(app.selected(), 2);
    app.on_key(key(KeyCode::Char('g')));
    assert_eq!(app.selected(), 0);
}

#[test]
fn an_empty_list_never_panics_on_any_key() {
    let mut app = App::new(Vec::new());
    for code in [
        KeyCode::Char('j'),
        KeyCode::Down,
        KeyCode::Char('k'),
        KeyCode::Up,
        KeyCode::Char('g'),
        KeyCode::Char('G'),
        KeyCode::Enter,
        KeyCode::Char(' '),
        KeyCode::Char('+'),
        KeyCode::Char('-'),
        KeyCode::Char('?'),
        KeyCode::Esc,
        KeyCode::Char('q'),
    ] {
        let _ = app.on_key(key(code));
        let _ = app.selected();
    }
}

#[test]
fn question_mark_opens_help_and_esc_closes_it() {
    let mut app = app_with_three_tracks();
    assert!(!app.help_open());

    assert_eq!(app.on_key(key(KeyCode::Char('?'))), Action::None);
    assert!(app.help_open());

    assert_eq!(app.on_key(key(KeyCode::Esc)), Action::None);
    assert!(!app.help_open());

    // `?` toggles: it also closes an open overlay.
    app.on_key(key(KeyCode::Char('?')));
    app.on_key(key(KeyCode::Char('?')));
    assert!(!app.help_open());
}

#[test]
fn volume_moves_in_five_percent_steps_and_clamps_at_the_bounds() {
    let mut app = app_with_three_tracks();

    assert_eq!(app.on_key(key(KeyCode::Char('+'))), Action::SetVolume(100));
    // At 100, `+` clamps instead of overflowing.
    assert_eq!(app.on_key(key(KeyCode::Char('+'))), Action::SetVolume(100));
    assert_eq!(app.volume(), 100);

    assert_eq!(app.on_key(key(KeyCode::Char('-'))), Action::SetVolume(95));
    assert_eq!(app.volume(), 95);

    let mut app = app_with_three_tracks();
    app.on_key(key(KeyCode::Char('-')));
    assert_eq!(app.volume(), 95);
    for _ in 0..24 {
        app.on_key(key(KeyCode::Char('-')));
    }
    // At 0, `-` clamps instead of underflowing.
    assert_eq!(app.on_key(key(KeyCode::Char('-'))), Action::SetVolume(0));
    assert_eq!(app.volume(), 0);
}

#[test]
fn q_returns_quit() {
    let mut app = app_with_three_tracks();
    assert_eq!(app.on_key(key(KeyCode::Char('q'))), Action::Quit);
}

#[test]
fn enter_returns_play_from_the_selection() {
    let mut app = app_with_three_tracks();
    assert_eq!(app.on_key(key(KeyCode::Enter)), Action::Play { from: 0 });
    app.on_key(key(KeyCode::Char('j')));
    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(app.on_key(key(KeyCode::Enter)), Action::Play { from: 2 });
}
