//! The info card (issue #43): what a track's card shows, that `esc` closes
//! it and that every other key waits while it is up. Rendered through
//! ratatui's test backend, so the card's own rows are asserted, not eyeballed.

use std::path::PathBuf;

use velo::ui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use velo::ui::{Action, App};

/// The known fixture: a two-second mp3.
fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/silence-then-tone.mp3")
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::from(code)
}

/// A ctrl-modified key press, the way a terminal reports it.
fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

fn screen_of(app: &App, width: u16, height: u16) -> String {
    use velo::ui::ratatui::Terminal;
    use velo::ui::ratatui::backend::TestBackend;
    use velo::ui::ratatui::buffer::{Buffer, Cell};

    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("a test terminal");
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

/// The card is open over a known fixture file and names it: the path, the
/// codec and the duration the format header carries.
#[test]
fn ctrl_i_opens_a_card_over_a_known_file_and_names_its_facts() {
    let path = fixture();
    let mut app = App::new(vec![path.clone()]);
    app.on_key(ctrl(KeyCode::Char('i')));

    let screen = screen_of(&app, 160, 24);
    assert!(
        screen.contains(path.to_string_lossy().as_ref()),
        "the card names the file verbatim, got:\n{screen}"
    );
    assert!(
        screen.contains("codec"),
        "the card has a codec row, got:\n{screen}"
    );
    assert!(
        screen.contains("mp3"),
        "a known mp3 names its codec, got:\n{screen}"
    );
    assert!(
        screen.contains("duration"),
        "the card has a duration row, got:\n{screen}"
    );
    assert!(
        screen.contains("00:02"),
        "the header says the track is two seconds long, got:\n{screen}"
    );
}

/// `esc` closes the card and every other key waits while it is open: `q`
/// does not quit, and the quit answer only comes back once the card is gone.
#[test]
fn esc_closes_the_card_and_every_other_key_waits_while_it_is_open() {
    let mut app = App::new(vec![fixture()]);
    app.on_key(ctrl(KeyCode::Char('i')));
    assert!(
        screen_of(&app, 160, 24).contains(" info "),
        "the card is on screen"
    );

    assert_eq!(app.on_key(key(KeyCode::Char('q'))), Action::None);
    assert!(
        screen_of(&app, 160, 24).contains(" info "),
        "`q` waits: the card is still on screen"
    );

    assert_eq!(app.on_key(key(KeyCode::Esc)), Action::None);
    assert!(
        !screen_of(&app, 160, 24).contains(" info "),
        "`esc` closes the card"
    );

    assert_eq!(app.on_key(key(KeyCode::Char('q'))), Action::Quit);
}

/// A remote entry is described by its own URI, verbatim — and the key press
/// never fails on it.
#[test]
fn a_remote_entry_shows_its_uri_verbatim() {
    let mut app = App::new(vec![PathBuf::from("subsonic://server.example/42")]);
    app.on_key(ctrl(KeyCode::Char('i')));

    let screen = screen_of(&app, 160, 24);
    assert!(
        screen.contains("subsonic://server.example/42"),
        "the URI is on the card, got:\n{screen}"
    );
}

/// The `ReplayGain` tags the file carries are on the card, as the scanner
/// wrote them (issue #43): one row per tag, the absent rowIndex left out.
#[test]
fn a_file_with_replaygain_tags_names_them_on_the_card() {
    use velo::audio::replay_gain::{GainTags, format_gain_db, format_peak, write_tags};

    let dir = std::env::temp_dir().join(format!("velo-ui-info-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the scratch directory exists");
    let path = dir.join("tagged.mp3");
    std::fs::copy(fixture(), &path).expect("the fixture copies");
    write_tags(
        &path,
        &GainTags {
            track_gain_db: -7.35,
            track_peak: 0.5,
            album_gain_db: -6.0,
            album_peak: 0.9,
        },
    )
    .expect("the tags write");

    let mut app = App::new(vec![path.clone()]);
    app.on_key(ctrl(KeyCode::Char('i')));
    let screen = screen_of(&app, 160, 24);

    assert!(screen.contains("track gain"), "got:\n{screen}");
    assert!(screen.contains("-7.35 dB"), "got:\n{screen}");
    assert!(screen.contains("track peak"), "got:\n{screen}");
    assert!(screen.contains(format_peak(0.5).as_str()), "got:\n{screen}");
    assert!(screen.contains("album gain"), "got:\n{screen}");
    assert!(
        screen.contains(format_gain_db(-6.0).as_str()),
        "got:\n{screen}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The sources screen holds no track, and nothing plays: the card does not
/// open, and one status line says so.
#[test]
fn the_sources_screen_with_nothing_playing_opens_nothing_and_says_so() {
    let mut app = App::browse(None);
    app.open_sources();
    app.on_key(ctrl(KeyCode::Char('i')));

    assert!(
        !screen_of(&app, 80, 24).contains(" info "),
        "no card opens where no track is"
    );
    assert!(
        !app.status().is_empty(),
        "the refusal is said in one status line, got: {:?}",
        app.status()
    );
}
