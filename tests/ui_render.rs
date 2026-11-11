//! What the shell actually puts on the screen (S1, S13).
//!
//! A terminal interface can be tested without a terminal: ratatui's test backend
//! renders a frame into a buffer, so the layout, the empty state and the key
//! hints are asserted instead of eyeballed. These tests exist because the
//! interface was first verified by construction, and construction never noticed
//! a separator dangling in the footer.

use std::fs;
use std::path::PathBuf;

use velo::audio::output::Entry;
use velo::library::Library;
use velo::radio::client::HttpTransport;
use velo::ui::App;
use velo::ui::crossterm::event::{KeyCode, KeyEvent};
use velo::ui::radio_data::RadioData;
use velo::ui::ratatui::Terminal;
use velo::ui::ratatui::backend::TestBackend;
use velo::ui::ratatui::buffer::{Buffer, Cell};
use velo::ui::visualizer::Visualizer;

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
        "V E L O", "2 tracks", "one.flac", "two.flac", "stopped", "j/k move", "q quit",
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

#[test]
fn the_volume_slider_shows_the_volume_and_the_help_lists_the_seek_keys() {
    let app = App::new(tracks(&["one.flac"]));
    let screen = render(&app);

    assert!(
        screen.contains("VOL") && screen.contains("● 100"),
        "the slider shows the volume with its knob and number, got: {screen}"
    );

    let mut app = App::new(tracks(&["one.flac"]));
    app.on_key(velo::ui::crossterm::event::KeyEvent::from(
        velo::ui::crossterm::event::KeyCode::Char('?'),
    ));
    let helped = render(&app);

    assert!(
        helped.contains("seek 5 s"),
        "the help lists the seek keys, got: {helped}"
    );
    let whole = screen_at(&app, 80, 24);
    assert!(
        whole.contains("> / . / n") && whole.contains("next track"),
        "the help lists the skip keys, got:\n{whole}"
    );
}

// ---- the window (ui-redesign T1, S1) ---------------------------------------

/// The cell at `(x, y)` of an 80×24 frame of `app`.
fn cell_at(app: &App, x: u16, y: u16) -> Cell {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("a test terminal");
    terminal
        .draw(|frame| app.draw(frame))
        .expect("the frame draws");
    terminal.backend().buffer()[(x, y)].clone()
}

#[test]
fn the_window_is_framed_and_its_title_bar_names_the_views_and_the_app() {
    let app = App::new(tracks(&["one.flac"]));
    let screen = screen_at(&app, 80, 24);
    let lines: Vec<&str> = screen.lines().collect();

    assert!(lines[0].starts_with('╭') && lines[0].ends_with('╮'));
    assert!(lines[23].starts_with('╰') && lines[23].ends_with('╯'));
    for expected in ["sources", "queue", "V E L O"] {
        assert!(
            lines[1].contains(expected),
            "the title bar shows {expected:?}, got: {}",
            lines[1]
        );
    }
}

#[test]
fn the_title_bar_lights_the_view_that_tab_switches_to() {
    use velo::ui::theme::Theme;

    let theme = Theme::default();

    let mut app = App::new(tracks(&["one.flac"]));
    let sources_x = 3; // "│ sources  queue": the `s` of sources
    let queue_x = 12; // the `q` of queue
    assert_eq!(cell_at(&app, sources_x, 1).fg, theme.accent);
    assert_eq!(cell_at(&app, queue_x, 1).fg, theme.dim);

    press(&mut app, KeyCode::Tab);
    assert_eq!(cell_at(&app, sources_x, 1).fg, theme.dim);
    assert_eq!(cell_at(&app, queue_x, 1).fg, theme.accent);
}

/// S11 (issue #26): the strip names the round trip — `sources` and `queue` —
/// and `sources` is lit anywhere inside the sources screen: on it, inside
/// the local library and under the radio view, which is drawn over both.
#[test]
fn the_title_bar_lights_sources_anywhere_inside_it_and_the_queue_on_the_queue() {
    use velo::ui::theme::Theme;

    let theme = Theme::default();
    let sources_x = 3; // "│ sources  queue": the `s` of sources
    let queue_x = 12; // the `q` of queue

    // The launch sits on the library, which lives inside Local library.
    let mut app = App::browse(None);
    app.set_library_root(Some(std::path::PathBuf::from("/music")));
    assert_eq!(cell_at(&app, sources_x, 1).fg, theme.accent);
    assert_eq!(cell_at(&app, queue_x, 1).fg, theme.dim);

    // The sources screen itself.
    app.open_sources();
    assert_eq!(cell_at(&app, sources_x, 1).fg, theme.accent);

    // The queue is the other half of the round trip.
    press(&mut app, KeyCode::Tab);
    assert_eq!(cell_at(&app, sources_x, 1).fg, theme.dim);
    assert_eq!(cell_at(&app, queue_x, 1).fg, theme.accent);

    // Back on the sources screen, the radio drawn over it stays inside it.
    press(&mut app, KeyCode::Tab);
    // Radio leads the list now (the author's order), so it is the row the
    // cursor starts on and `enter` opens it without moving.
    press(&mut app, KeyCode::Enter);
    assert!(app.radio_open());
    assert_eq!(cell_at(&app, sources_x, 1).fg, theme.accent);
    assert_eq!(cell_at(&app, queue_x, 1).fg, theme.dim);
}

// ---- the now-playing block (ui-redesign T2, S1, S4) ------------------------

#[test]
fn the_now_playing_block_says_nothing_plays_and_shows_the_state_and_transport() {
    let app = App::new(tracks(&["one.flac", "two.flac"]));
    let screen = screen_at(&app, 80, 24);

    for expected in [
        "♫ nothing playing",
        "■ stopped",
        "◂◂",
        "▶ play",
        "▸▸",
        "VOL",
    ] {
        assert!(
            screen.contains(expected),
            "the block shows {expected:?}, got:\n{screen}"
        );
    }
    assert!(
        !screen.contains("❚❚ pause"),
        "nothing is playing, so the primary button does not offer a pause"
    );
}

#[test]
fn the_volume_slider_follows_the_volume_keys() {
    let mut app = App::new(tracks(&["one.flac"]));
    press(&mut app, KeyCode::Char('-'));
    let screen = screen_at(&app, 80, 24);
    let slider = screen
        .lines()
        .find(|line| line.contains("VOL"))
        .expect("a volume row");

    assert!(
        slider.contains("●─  95") && !slider.contains("100"),
        "one step down moves the knob off the end and the number to 95, got: {slider}"
    );
}

/// playback-and-library T2: shuffle and repeat sit beside the transport
/// buttons, dim while off and in the accent while on.
#[test]
fn the_transport_row_shows_shuffle_and_repeat_and_lights_them_when_on() {
    use velo::ui::theme::Theme;

    let theme = Theme::default();

    let mut app = App::new(tracks(&["one.flac"]));
    let row_of = |app: &App| {
        screen_at(app, 80, 24)
            .lines()
            .find(|line| line.contains("VOL"))
            .expect("a transport row")
            .to_owned()
    };
    let column = |row: &str, word: &str| {
        let byte = row.find(word).expect("the word on the row");
        u16::try_from(row[..byte].chars().count()).unwrap()
    };

    let row = row_of(&app);
    assert!(
        row.contains("shuffle") && row.contains("repeat off"),
        "{row}"
    );
    let y = u16::try_from(
        screen_at(&app, 80, 24)
            .lines()
            .position(|line| line.contains("VOL"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(cell_at(&app, column(&row, "shuffle"), y).fg, theme.dim);

    press(&mut app, KeyCode::Char('r'));
    press(&mut app, KeyCode::Char('z'));
    let row = row_of(&app);
    assert!(row.contains("repeat all"), "{row}");
    assert_eq!(cell_at(&app, column(&row, "shuffle"), y).fg, theme.accent);
    assert_eq!(cell_at(&app, column(&row, "all"), y).fg, theme.accent);
}

/// ui-redesign T10: `ReplayGain` arrived from `main` (T11 there) as a config
/// key with nothing on screen; the transport row now says which mode the
/// audio path applies, in the config's own words, beside the volume.
#[test]
fn the_transport_row_names_the_replay_gain_mode_beside_the_volume() {
    use velo::config::ReplayGainMode;

    let mut app = App::new(tracks(&["one.flac"]));
    for (mode, label) in [
        (ReplayGainMode::Track, "RG track"),
        (ReplayGainMode::Album, "RG album"),
        (ReplayGainMode::Off, "RG off"),
    ] {
        app.set_replay_gain(mode);
        let screen = screen_at(&app, 80, 24);
        let row = screen
            .lines()
            .find(|line| line.contains("VOL"))
            .expect("a volume row");
        let rg = row.find(label);
        let vol = row.find("VOL").unwrap();
        assert!(
            rg.is_some_and(|at| at < vol),
            "{label:?} sits before VOL, got: {row}"
        );
    }
}

/// ui-redesign T7, reported by the user: the slider is set against the right
/// edge, so a number one digit shorter (100 → 95, 10 → 5) shifted the whole
/// slider one column. The number now always takes three columns.
#[test]
fn the_volume_slider_does_not_move_when_the_number_loses_a_digit() {
    let mut app = App::new(tracks(&["one.flac"]));
    let column = |app: &App| {
        let screen = screen_at(app, 80, 24);
        let row = screen
            .lines()
            .find(|line| line.contains("VOL"))
            .expect("a volume row")
            .to_owned();
        row.chars()
            .collect::<Vec<_>>()
            .windows(3)
            .position(|w| w == ['V', 'O', 'L'])
            .expect("VOL on the row")
    };

    let at_100 = column(&app);
    for step in 1..=20 {
        press(&mut app, KeyCode::Char('-'));
        assert_eq!(
            column(&app),
            at_100,
            "the slider stays put at {}%",
            100 - step * 5
        );
    }
}

#[test]
fn a_short_terminal_shrinks_the_block_before_the_list_disappears() {
    let app = App::new(tracks(&["one.flac", "two.flac", "three.flac"]));

    // 12 rows: the compact block leaves the list two of its rows.
    let short = screen_at(&app, 90, 12);
    for expected in ["one.flac", "two.flac", "■ stopped", "VOL"] {
        assert!(
            short.contains(expected),
            "a 90×12 window still shows {expected:?}, got:\n{short}"
        );
    }

    // 8 rows: one line of block, and the first track is still there.
    let tiny = screen_at(&app, 60, 8);
    for expected in ["one.flac", "stopped"] {
        assert!(
            tiny.contains(expected),
            "a 60×8 window still shows {expected:?}, got:\n{tiny}"
        );
    }
}

// ---- the spectrum (ui-redesign T5c, S1, S2, S4) ----------------------------

/// The rows of a frame of `app` at this size.
fn lines_at(app: &App, width: u16, height: u16) -> Vec<String> {
    screen_at(app, width, height)
        .lines()
        .map(str::to_owned)
        .collect()
}

/// A tall window keeps the band area between the clock and the progress bar
/// even though nothing plays draws nothing in it: silence renders nothing
/// for every visualizer (issue #40).
#[test]
fn a_tall_window_keeps_the_band_between_the_clock_and_the_progress_bar() {
    let app = App::new(tracks(&["one.flac"]));
    let tall = lines_at(&app, 100, 32);
    let clock = tall
        .iter()
        .position(|line| line.contains("■ stopped"))
        .unwrap();
    let progress = tall
        .iter()
        .position(|line| line.starts_with("│ ──────"))
        .unwrap();
    assert!(
        clock + 1 < progress,
        "rows for the band sit between the clock and the progress bar"
    );
    // Silence draws nothing at all (issue #40): no bars, no baseline.
    for row in tall.iter().skip(clock + 1).take(progress - clock - 1) {
        assert!(
            !row.contains("▁") && !row.contains("█"),
            "silence draws nothing, got: {row}"
        );
    }

    // The 80×24 window keeps its seven-row block: no band there either.
    assert!(!screen_at(&app, 80, 24).contains("▁"));
}

#[test]
fn a_quiet_player_redraws_slowly_and_nothing_is_animating() {
    let app = App::new(tracks(&["one.flac"]));
    assert_eq!(app.frame_interval(), std::time::Duration::from_millis(100));
}

/// Low power (issue #11, S2): the redraw cadence drops to four times the
/// ordinary interval, whatever the ordinary one would have been.
#[test]
fn low_power_quarters_the_redraw_cadence() {
    let mut app = App::new(tracks(&["one.flac"]));
    app.set_ui(velo::config::UiParts {
        low_power: true,
        ..velo::config::UiParts::default()
    });
    // A quiet screen redraws at ten ordinary; in low power, a quarter of
    // that rate.
    assert_eq!(app.frame_interval(), std::time::Duration::from_millis(400));
}

/// Low power (issue #11, S2): the spectrum is left out entirely — on the
/// same tall window that otherwise draws the bars — and its rows go back to
/// the list; the layout is otherwise unchanged (S3).
#[test]
fn low_power_leaves_the_spectrum_out_and_gives_its_rows_back() {
    let mut app = App::new(tracks(&["one.flac", "two.flac", "three.flac"]));
    // The band's rows are reserved whenever a visualizer is in force, even
    // though silence draws nothing (issue #40): the layout is what low
    // power changes, so the list's rows are the observable.
    let rows_before = screen_at(&app, 100, 32)
        .lines()
        .filter(|line| line.contains("one.flac"))
        .count();

    app.set_ui(velo::config::UiParts {
        low_power: true,
        ..velo::config::UiParts::default()
    });
    let tall = screen_at(&app, 100, 32);
    assert!(
        !tall.contains("▁") && !tall.contains("█"),
        "low power leaves the visualizer out, got:\n{tall}"
    );
    // The rows the band gave up hold list rows again.
    let rows_after = tall
        .lines()
        .filter(|line| line.contains("one.flac") || line.contains("two.flac"))
        .count();
    assert!(
        rows_after >= rows_before,
        "the spectrum's rows go back to the list"
    );
}

/// The cell at `(x, y)` of a `width`×`height` frame of `app`.
fn cell_at_size(app: &App, width: u16, height: u16, x: usize, y: usize) -> Cell {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("a test terminal");
    terminal
        .draw(|frame| app.draw(frame))
        .expect("the frame draws");
    terminal.backend().buffer()[(u16::try_from(x).unwrap(), u16::try_from(y).unwrap())].clone()
}

// ---- the help overlay (ui-redesign T4, S1, S3) -----------------------------

#[test]
fn the_help_is_a_rounded_card_titled_keys_with_the_keys_in_their_own_color() {
    use velo::ui::theme::Theme;

    let theme = Theme::default();

    let mut app = App::new(tracks(&["one.flac"]));
    press(&mut app, KeyCode::Char('?'));
    let screen = screen_at(&app, 80, 24);
    let lines: Vec<&str> = screen.lines().collect();

    let top = lines
        .iter()
        .position(|line| line.contains("╭ keys "))
        .unwrap_or_else(|| panic!("a rounded card titled keys, got:\n{screen}"));
    let row = lines
        .iter()
        .position(|line| line.contains("first track"))
        .expect("the g row");
    assert!(row > top);

    // The key is the first thing after the card's left edge, the second `│`.
    let key_x = lines[row]
        .chars()
        .enumerate()
        .filter(|&(_, c)| c == '│')
        .nth(1)
        .map(|(x, _)| x)
        .and_then(|edge| {
            lines[row]
                .chars()
                .enumerate()
                .skip(edge + 1)
                .find(|&(_, c)| c != ' ')
        })
        .map(|(x, c)| {
            assert_eq!(c, 'g', "got: {}", lines[row]);
            x
        })
        .expect("a key on the row");
    let cell = cell_at(
        &app,
        u16::try_from(key_x).unwrap(),
        u16::try_from(row).unwrap(),
    );
    assert_eq!(
        cell.fg, theme.secondary,
        "keys stand apart from what they do"
    );
}

// ---- browsing the library (T5) ------------------------------------------

/// A temp library with its own index, self-removing on drop. The index is
/// opened at an explicit path inside it, never through `VELO_DB`.
struct TempLibrary {
    root: PathBuf,
    library: Library,
}

impl TempLibrary {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("velo-ui-render-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let library = Library::open_at(&root.join("index.db")).unwrap();
        Self { root, library }
    }

    /// Writes a small valid PCM WAV file (0.1 s, 44.1 kHz, stereo).
    fn write_wav(&self, relative: &str) -> PathBuf {
        let path = self.root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();

        let sample_rate: u32 = 44_100;
        let channels: u16 = 2;
        let frames: usize = 4_410;
        let data_len: u32 = u32::try_from(frames * usize::from(channels) * 2).unwrap();

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes()); // PCM
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&(sample_rate * u32::from(channels) * 2).to_le_bytes());
        bytes.extend_from_slice(&(channels * 2).to_le_bytes()); // block align
        bytes.extend_from_slice(&16_u16.to_le_bytes()); // bits per sample
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        bytes.extend(
            std::iter::repeat_n(0_i16, frames * usize::from(channels)).flat_map(i16::to_le_bytes),
        );

        fs::write(&path, bytes).unwrap();
        path
    }

    fn index(&self) {
        self.library.index(&self.root).unwrap();
    }
}

impl Drop for TempLibrary {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn press(app: &mut App<'_>, code: KeyCode) {
    let action = app.on_key(KeyEvent::from(code));
    app.apply(action);
}

/// A ctrl-modified key press, the way a terminal reports it.
fn ctrl(app: &mut App<'_>, code: KeyCode) {
    let event = KeyEvent::new(code, velo::ui::crossterm::event::KeyModifiers::CONTROL);
    let action = app.on_key(event);
    app.apply(action);
}

// ---- the lists (ui-redesign T3, S1, S3) ------------------------------------

#[test]
fn a_list_has_a_header_line_and_numbered_rows_on_an_accent_selection() {
    let app = App::new(tracks(&["one.flac", "two.flac", "three.flac"]));
    let screen = screen_at(&app, 80, 24);

    for expected in ["▸─ Files  [3 tracks]", "1. one.flac", "3. three.flac"] {
        assert!(
            screen.contains(expected),
            "the list shows {expected:?}, got:\n{screen}"
        );
    }
    let row = screen
        .lines()
        .position(|line| line.contains("1. one.flac"))
        .expect("the first row");
    let selected = cell_at(&app, 10, u16::try_from(row).unwrap());
    // ui-redesign T8: no ANSI color is a raised surface on every theme, so
    // the selection is the accent with the ink (its darkest color) on top.
    let theme = velo::ui::theme::Theme::default();
    assert_eq!(selected.bg, theme.accent, "the selection is the accent");
    assert_eq!(selected.fg, theme.ink, "and its text the darkest color");
}

/// The interface wears the resolved `[theme]` palette. The factory defaults
/// are velo's own palette (issue #57) — its dark background, its warm text,
/// the gray dim — and the help card's border and title show the accent at
/// work.
#[test]
fn the_interface_takes_its_colors_from_the_terminal_palette() {
    use velo::ui::ratatui::style::Color;

    let app = App::new(tracks(&["one.flac", "two.flac"]));
    let empty = cell_at(&app, 40, 15);
    assert_eq!(
        empty.bg,
        Color::Rgb(0x14, 0x1c, 0x28),
        "the factory velo background (issue #57)"
    );

    // The body's ordinary text wears the factory text colour (a row that is
    // not the selection, whose cells wear the accent).
    let screen = screen_at(&app, 80, 24);
    let row = screen
        .lines()
        .find(|line| line.contains("2. two.flac"))
        .expect("a list row");
    let x = u16::try_from(row.find("two.flac").unwrap()).unwrap();
    let y = u16::try_from(
        screen
            .lines()
            .position(|line| line.contains("2. two.flac"))
            .unwrap(),
    )
    .unwrap();
    let name = cell_at(&app, x, y);
    assert_eq!(
        name.fg,
        Color::Rgb(0xF4, 0xF0, 0xE2),
        "the factory velo text (issue #57)"
    );
    assert_eq!(name.bg, Color::Rgb(0x14, 0x1c, 0x28));

    // A dim hint recedes in the default dim gray.
    let hints = screen
        .lines()
        .find(|line| line.contains("q quit"))
        .expect("the key hints");
    let hint_x = u16::try_from(hints[..hints.find("quit").unwrap()].chars().count()).unwrap();
    let hint_y = u16::try_from(
        screen
            .lines()
            .position(|line| line.contains("q quit"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        cell_at(&app, hint_x, hint_y).fg,
        Color::Rgb(0x76, 0x76, 0x76),
        "the dim hint is the default dim gray"
    );

    // The help card's border and title wear the accent.
    let mut app = App::new(tracks(&["one.flac"]));
    press(&mut app, KeyCode::Char('?'));
    let screen = screen_at(&app, 80, 24);
    let lines: Vec<&str> = screen.lines().collect();
    let top = lines
        .iter()
        .position(|line| line.contains("╭ keys "))
        .expect("the help card is open");
    let title_at = lines[top].find(" keys ").unwrap();
    let corner_x = u16::try_from(title_at - 1).unwrap();
    let title_x = u16::try_from(title_at + 1).unwrap();
    let top = u16::try_from(top).unwrap();
    assert_eq!(
        cell_at(&app, corner_x, top).fg,
        Color::Rgb(0xf4, 0xba, 0xa3),
        "the card's border is the default accent"
    );
    assert_eq!(
        cell_at(&app, title_x, top).fg,
        Color::Rgb(0xf4, 0xba, 0xa3),
        "the card's title is the default accent"
    );
}

/// A configured `[theme]` reaches every cell: the accent on the card's
/// border and the text color on a list row, and not a trace of the old ANSI
/// magenta anywhere on the screen.
#[test]
fn a_configured_palette_reaches_the_cells() {
    use velo::config::{ThemeColor, ThemeColors};
    use velo::ui::ratatui::style::Color;

    let mut app = App::new(tracks(&["one.flac", "two.flac"]));
    app.set_theme(ThemeColors {
        accent: ThemeColor::Rgb {
            red: 0x12,
            green: 0x34,
            blue: 0x56,
        },
        text: ThemeColor::Rgb {
            red: 0xe0,
            green: 0xe0,
            blue: 0xe0,
        },
        ..ThemeColors::default()
    });

    // A list row wears the configured text color (not the selected row,
    // whose cells wear the accent and the ink).
    let screen = screen_at(&app, 80, 24);
    let row = screen
        .lines()
        .find(|line| line.contains("2. two.flac"))
        .expect("a list row");
    let x = u16::try_from(row.find("two.flac").unwrap()).unwrap();
    let y = u16::try_from(
        screen
            .lines()
            .position(|line| line.contains("2. two.flac"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        cell_at(&app, x, y).fg,
        Color::Rgb(0xe0, 0xe0, 0xe0),
        "the list row is the configured text"
    );

    // The card's border wears the configured accent.
    press(&mut app, KeyCode::Char('?'));
    let lines = lines_at(&app, 80, 24);
    let top = lines
        .iter()
        .position(|line| line.contains("╭ keys "))
        .expect("the help card is open");
    let corner_x = u16::try_from(lines[top].find(" keys ").unwrap() - 1).unwrap();
    let top = u16::try_from(top).unwrap();
    assert_eq!(
        cell_at(&app, corner_x, top).fg,
        Color::Rgb(0x12, 0x34, 0x56),
        "the card's border is the configured accent"
    );

    // And not a trace of the old ANSI magenta is left on the screen.
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("a test terminal");
    terminal
        .draw(|frame| app.draw(frame))
        .expect("the frame draws");
    for cell in terminal.backend().buffer().content() {
        assert!(
            !matches!(cell.fg, Color::Magenta) && !matches!(cell.bg, Color::Magenta),
            "no ANSI magenta is left, got: {cell:?}"
        );
    }
}

#[test]
fn library_rows_put_their_facts_against_the_right_edge() {
    let temp = TempLibrary::new("right-facts");
    temp.write_wav("Tide/2019 - Harbor/01 - Dawn.wav");
    temp.index();
    let mut app = App::browse(Some(&temp.library));

    let artists = screen_at(&app, 80, 24);
    assert!(
        artists.contains("▸─ 1 artists  2 albums  3 tracks  4 genres   [1 artist]"),
        "got:\n{artists}"
    );
    assert!(
        artists
            .lines()
            .any(|line| line.contains("1. Tide") && line.ends_with("1 album · 1 track │")),
        "the artist's counts sit at the right edge, got:\n{artists}"
    );

    press(&mut app, KeyCode::Enter); // Tide
    press(&mut app, KeyCode::Enter); // Harbor
    let tracks = screen_at(&app, 80, 24);
    assert!(
        tracks.contains("▸─ 1 artists  2 albums  3 tracks  4 genres   Tide › Harbor  [1 track]"),
        "got:\n{tracks}"
    );
    assert!(
        tracks
            .lines()
            .any(|line| line.contains("1. Dawn") && line.ends_with("0:00 │")),
        "the track's length sits at the right edge, got:\n{tracks}"
    );

    press(&mut app, KeyCode::Char('/'));
    press(&mut app, KeyCode::Char('d'));
    let search = screen_at(&app, 80, 24);
    assert!(
        search.contains("▸─ Search /d  [1 match]"),
        "the search names its query and its count, got:\n{search}"
    );
}

#[test]
fn the_queue_has_its_own_header() {
    let mut app = App::new(tracks(&["one.flac", "two.flac"]));
    press(&mut app, KeyCode::Char('a'));
    press(&mut app, KeyCode::Tab);
    let screen = screen_at(&app, 80, 24);

    assert!(
        screen.contains("▸─ Queue  [1 track]") && screen.contains("1. one.flac"),
        "got:\n{screen}"
    );
}

/// An empty level must explain itself, in the spirit of the flat "no tracks"
/// empty state, instead of panicking. Both empty levels are reached with real
/// index states: an artist whose rows vanished between the snapshot and the
/// descent, and an album whose tracks did the same (a stale snapshot plus a
/// re-index is exactly how this happens to a real user).
#[test]
fn an_artist_without_albums_and_an_album_without_tracks_draw_an_empty_state() {
    let temp = TempLibrary::new("empty-levels");
    temp.write_wav("Solo/2019 - Single/01 - One.wav");
    temp.write_wav("Tide/2019 - Harbor/01 - Dawn.wav");
    temp.write_wav("Tide/2019 - Lantern/01 - Far.wav");
    temp.index();

    let mut app = App::browse(Some(&temp.library));

    // Solo's only file disappears after the interface took its snapshot.
    fs::remove_file(temp.root.join("Solo/2019 - Single/01 - One.wav")).unwrap();
    temp.index();
    press(&mut app, KeyCode::Enter); // into Solo: zero albums
    let screen = render(&app);
    assert!(
        screen.contains("no albums"),
        "an artist without albums must explain itself, got: {screen}"
    );

    press(&mut app, KeyCode::Esc); // back to the artists
    press(&mut app, KeyCode::Char('j')); // onto Tide
    press(&mut app, KeyCode::Enter); // into Tide: Lantern is second
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Enter); // into Lantern

    // Lantern's only track disappears too.
    fs::remove_file(temp.root.join("Tide/2019 - Lantern/01 - Far.wav")).unwrap();
    temp.index();
    press(&mut app, KeyCode::Esc); // back onto the cached album list
    press(&mut app, KeyCode::Char('j')); // onto Lantern again
    press(&mut app, KeyCode::Enter); // into Lantern: zero tracks

    let screen = render(&app);
    assert!(
        screen.contains("no tracks"),
        "an album without tracks must explain itself, got: {screen}"
    );
}

#[test]
fn a_count_of_one_is_not_pluralised() {
    assert_eq!(velo::ui::plural(1, "album", "albums"), "1 album");
    assert_eq!(velo::ui::plural(1, "track", "tracks"), "1 track");
    assert_eq!(velo::ui::plural(0, "track", "tracks"), "0 tracks");
    assert_eq!(velo::ui::plural(20, "artist", "artists"), "20 artists");
}

// ---- the queue view (T18b, S11) ------------------------------------------

#[test]
fn the_queue_view_draws_the_queued_file_names_and_empty_state() {
    let mut app = App::new(tracks(&["one.flac", "two.flac"]));

    // An empty queue explains itself instead of drawing nothing.
    press(&mut app, KeyCode::Tab);
    let empty = render(&app);
    assert!(
        empty.contains("nothing queued"),
        "an empty queue explains itself, got: {empty}"
    );

    // Queue both tracks, then look at the queue view.
    press(&mut app, KeyCode::Tab); // back to the library
    press(&mut app, KeyCode::Char('a'));
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('a'));
    press(&mut app, KeyCode::Tab); // into the queue
    let screen = render(&app);

    for expected in ["one.flac", "two.flac"] {
        assert!(
            screen.contains(expected),
            "the queue view must show {expected:?}, got: {screen}"
        );
    }
    assert!(
        !screen.contains("nothing queued"),
        "the empty-state hint is gone once something is queued"
    );
}

// ---- a station row reads as live (issue #17) -------------------------------

/// A station queued among the tracks keeps its row and reads as live: the
/// station's name where a file's name stands, and `● live` — the same fact
/// the transport shows for a live stream — where a track shows its position.
#[test]
fn a_station_row_in_the_queue_reads_as_live() {
    let mut app = App::new(tracks(&["one.flac"]));
    // A station plays (the fixture stands in for the player): it heads the
    // queue, and the file queued after it keeps its track row.
    app.show_playing_for_tests(
        Entry::Station {
            url: String::from("https://stream.example/nova.mp3"),
            name: String::from("Radio Nova"),
        },
        std::time::Duration::ZERO,
        None,
        None,
    );
    press(&mut app, KeyCode::Char('a')); // one.flac queues after the station
    press(&mut app, KeyCode::Tab); // into the queue

    // One row per line, so the marker is read on the station's own row.
    let screen = screen_at(&app, 90, 12);
    // The queue row is the one that carries the station's position in the
    // queue (`1. `) — the fact a track shows there and a station does not.
    let station_row = screen
        .lines()
        .find(|line| line.contains("1. ") && line.contains("Radio Nova"))
        .expect("the queue shows the station by its name");
    assert!(
        station_row.contains("● live"),
        "the station row reads as live where a track shows its position, got: {station_row:?}"
    );
    let track_row = screen
        .lines()
        .find(|line| line.contains("2. ") && line.contains("one.flac"))
        .expect("the queued file keeps its track row");
    assert!(
        !track_row.contains("● live"),
        "a track row is not live, got: {track_row:?}"
    );
}

// ---- the first screen says where the sound comes from ---------------------

/// The Sources screen is where a person chooses an origin, so it also answers
/// "what is playing, and from where": the row that owns the playing entry
/// carries `▶`, the way a list row does. A station is radio; a file the index
/// knows is the local library.
#[test]
fn the_sources_screen_marks_the_row_what_plays_came_from() {
    let mut app = App::browse(None);
    app.set_library_root(Some(std::path::PathBuf::from("/music")));
    app.show_playing_for_tests(
        Entry::Station {
            url: String::from("https://stream.example/nova.mp3"),
            name: String::from("Radio Nova"),
        },
        std::time::Duration::ZERO,
        None,
        None,
    );
    app.open_sources();
    let screen = screen_at(&app, 80, 24);
    let radio = screen
        .lines()
        .find(|line| line.contains("1. Radio"))
        .unwrap_or_else(|| panic!("the radio row is on the first screen:\n{screen}"));
    assert!(
        radio.contains('▶'),
        "the radio row is marked, got: {radio:?}"
    );
    let library = screen
        .lines()
        .find(|line| line.contains("2. Local library"))
        .unwrap_or_else(|| panic!("the library row is on the first screen:\n{screen}"));
    assert!(
        !library.contains('▶'),
        "a row that does not own the playback stays bare, got: {library:?}"
    );
}

// ---- the transport's ladder as the window narrows -------------------------

/// The play button and the volume's number are the row; everything else leaves
/// as the width runs out, and in this order (author's report): the gauge first,
/// then the ReplayGain mode, then shuffle and repeat, then the two skip arrows.
/// RG used to outlive shuffle, which is what this pins.
#[test]
fn a_narrower_window_gives_the_transport_up_in_order() {
    let app = App::new(tracks(&["one.flac"]));
    let row_at = |width: u16| -> String {
        let screen = screen_at(&app, width, 30);
        screen
            .lines()
            .find(|line| line.contains("VOL"))
            .unwrap_or_default()
            .to_owned()
    };

    let wide = row_at(120);
    for piece in ["◂◂", "▸▸", "shuffle", "RG", "VOL"] {
        assert!(wide.contains(piece), "{piece} fits at 120, got: {wide:?}");
    }

    // Monotonic: a piece that has left never comes back as the window narrows.
    let widths = [120u16, 110, 100, 92, 84, 76, 68, 60, 52, 46, 40];
    let rows: Vec<String> = widths.iter().map(|width| row_at(*width)).collect();
    for pair in rows.windows(2) {
        let (wider, narrower) = (&pair[0], &pair[1]);
        for piece in ["◂◂", "shuffle", "RG"] {
            if !wider.contains(piece) {
                assert!(
                    !narrower.contains(piece),
                    "{piece} must not come back when the window narrows: {wider:?} -> {narrower:?}"
                );
            }
        }
    }

    // RG leaves before shuffle does: the order the author asked for.
    let rg_gone = rows
        .iter()
        .find(|row| !row.contains("RG"))
        .expect("RG leaves at some width");
    assert!(
        rg_gone.contains("shuffle"),
        "shuffle outlives RG, got: {rg_gone:?}"
    );

    // The narrowest row still carries the transport and the volume's number.
    let narrowest = rows.last().expect("a row at every width");
    assert!(
        narrowest.contains("play"),
        "the play button never leaves: {narrowest:?}"
    );
    assert!(
        narrowest.contains("VOL"),
        "the volume label never leaves: {narrowest:?}"
    );
    assert!(
        narrowest.contains("100"),
        "the volume's number never leaves: {narrowest:?}"
    );
}

// ---- the short block keeps the row readable --------------------------------

/// A window short enough gives the block three rows, and that row has to hold
/// the transport and the volume and nothing else: shuffle, repeat and the
/// ReplayGain mode need room to read. Crammed in they printed as
/// `shuffleRG track` against the slider. `z`, `r` and the settings card still
/// reach all three, and the tall block still shows them.
#[test]
fn the_short_block_keeps_only_the_transport_and_the_volume() {
    let app = App::new(tracks(&["one.flac"]));
    let screen = screen_at(&app, 100, 12);
    let row = screen
        .lines()
        .find(|line| line.contains("VOL"))
        .expect("the transport row carries the volume slider");
    assert!(row.contains("play"), "the transport is there, got: {row:?}");
    assert!(
        !row.contains("shuffle") && !row.contains("repeat") && !row.contains("RG"),
        "the middle elements step out of a short row, got: {row:?}"
    );
    // The tall block still carries them: the rule is the height, not a removal.
    let tall = screen_at(&app, 100, 30);
    let tall_row = tall
        .lines()
        .find(|line| line.contains("shuffle") && line.contains("VOL"))
        .expect("the tall block keeps shuffle beside the volume");
    assert!(
        tall_row.contains("RG"),
        "and the ReplayGain mode, got: {tall_row:?}"
    );
}

// ---- a live station says it once -------------------------------------------

/// The clock slot holds a position and a length, and a stream has neither. It
/// used to paint `● live` there as a stand-in while the transport pill on the
/// same row painted the same words, so a playing station read `● live   ● live`.
/// One marker per row: the pill's, which is the transport's own state.
#[test]
fn a_live_station_says_live_once_on_the_clock_row() {
    let mut app = App::new(tracks(&["one.flac"]));
    app.show_playing_for_tests(
        Entry::Station {
            url: String::from("https://stream.example/nova.mp3"),
            name: String::from("Radio Nova"),
        },
        std::time::Duration::ZERO,
        None,
        None,
    );

    // The tall block: the clock and the pill share one row, and the station is
    // not in the list on screen, so the whole screen carries the marker once.
    let screen = screen_at(&app, 100, 24);
    assert_eq!(
        screen.matches("● live").count(),
        1,
        "the live marker appears once on screen, got:\n{screen}"
    );
}

// ---- the states a row can carry (issue #53) --------------------------------
// A live stream cannot pause: the state next to the clock and the row marker
// tell a stopped station from a paused track. The shell computes the mark per
// list from the transport's state plus whether the row is the entry it stands
// on.

/// `space` on a track pauses it: the row carries `⏸` and the transport says
/// `paused`.
#[test]
fn a_paused_track_row_carries_the_pause_marker_and_the_transport_says_paused() {
    let mut app = App::new(tracks(&["one.flac"]));
    // The fixture stands in for the player, paused (issue #53): no test can
    // build a real one.
    app.show_playing_for_tests(
        Entry::File(PathBuf::from("/music/one.flac")),
        std::time::Duration::ZERO,
        Some(std::time::Duration::from_secs(180)),
        None,
    );
    app.set_play_state_for_tests(velo::ui::PlayState::Paused);
    press(&mut app, KeyCode::Tab); // into the queue

    let screen = screen_at(&app, 80, 24);
    let row = screen
        .lines()
        .find(|line| line.contains("1. one.flac"))
        .expect("the queue shows the paused track");
    assert!(
        row.contains("⏸"),
        "the paused track carries the pause marker, got: {row:?}"
    );
    assert!(
        !row.contains('▶') && !row.contains("■"),
        "a paused track is neither playing nor stopped, got: {row:?}"
    );
    assert!(
        screen.contains("paused"),
        "the transport says paused, got:\n{screen}"
    );
}

/// A station that stopped is still the entry the queue stands on, and its
/// being stopped is worth saying: the row carries `■` and the transport says
/// `■ stopped` — while a live stream can never read as paused.
#[test]
fn a_stopped_station_row_carries_the_stopped_marker_and_the_transport_says_stopped() {
    let mut app = App::new(tracks(&["one.flac"]));
    // The station played; its stream has stopped, and the queue still stands
    // on it (the fixture stands in for both halves).
    app.show_playing_for_tests(
        Entry::Station {
            url: String::from("https://stream.example/nova.mp3"),
            name: String::from("Radio Nova"),
        },
        std::time::Duration::ZERO,
        None,
        None,
    );
    app.set_play_state_for_tests(velo::ui::PlayState::Stopped);
    press(&mut app, KeyCode::Tab); // into the queue

    let screen = screen_at(&app, 80, 24);
    let row = screen
        .lines()
        .find(|line| line.contains("1. ") && line.contains("Radio Nova"))
        .expect("the queue still shows the station it stands on");
    assert!(
        row.contains("■"),
        "the stopped station carries the stopped marker, got: {row:?}"
    );
    assert!(
        !row.contains('▶'),
        "a stopped station does not read as playing, got: {row:?}"
    );
    assert!(
        screen.contains("■ stopped"),
        "the transport says stopped, got:\n{screen}"
    );
}

// ---- the radio's place on screen (issue #50) -------------------------------

/// With the radio on screen the screen header and the list stand down, but
/// the radio drew only into the body area: an empty row stayed where the
/// header was and the whole list sat a row too low. The radio draws from
/// the row the content starts on, header row included (issue #50).
#[test]
fn the_radio_draws_from_the_row_the_screen_header_takes() {
    // The wired view with no directory: the top level renders, no request.
    let mut app = App::new(tracks(&["one.flac", "two.flac"]));
    app.set_radio_wire(Box::new(RadioData::<HttpTransport>::new(None, None, None)));
    press(&mut app, KeyCode::Char('R')); // the radio opens over its wiring
    let with_radio = screen_at(&app, 80, 24);
    let plain = screen_at(&App::new(tracks(&["one.flac", "two.flac"])), 80, 24);

    let radio_header = with_radio
        .lines()
        .position(|line| line.contains("▸─ Radio"))
        .expect("the radio names itself in its header line");
    let list_header = plain
        .lines()
        .position(|line| line.contains("▸─ Files"))
        .expect("the list names itself in the screen header");
    assert_eq!(
        radio_header, list_header,
        "the radio's header line sits on the row the screen header takes, \
         with no empty row above it:\n{with_radio}"
    );
    assert!(
        with_radio.contains("Browse all countries"),
        "the top level's sections are on screen:\n{with_radio}"
    );
}

// ---- cover art (T8, S10) ---------------------------------------------------

/// The marker is only for the row that plays (issue #52): an idle row
/// carries two spaces where the glyph was, so numbers, names and facts stay
/// in their columns — the selection already has its own background.
#[test]
fn an_idle_row_carries_no_marker_and_the_playing_row_keeps_its_glyph() {
    let mut app = App::new(tracks(&["one.flac", "two.flac"]));
    // two.flac plays (the fixture stands in for the player) and heads the
    // queue; one.flac queues after it, idle.
    app.show_playing_for_tests(
        Entry::File(PathBuf::from("/music/two.flac")),
        std::time::Duration::ZERO,
        None,
        None,
    );
    press(&mut app, KeyCode::Char('a')); // one.flac queues after it
    press(&mut app, KeyCode::Tab); // into the queue

    let screen = screen_at(&app, 80, 24);
    let playing_row = screen
        .lines()
        .find(|line| line.contains("1. two.flac"))
        .expect("the queue shows the playing row first");
    assert!(
        playing_row.contains("▶"),
        "the row that plays keeps its marker, got: {playing_row:?}"
    );
    let idle_row = screen
        .lines()
        .find(|line| line.contains("2. one.flac"))
        .expect("the queued file keeps its row");
    assert!(
        !idle_row.contains('▸'),
        "an idle row carries no marker, got: {idle_row:?}"
    );
    assert!(
        idle_row.contains("  2. one.flac"),
        "the two spaces keep the number in its column, got: {idle_row:?}"
    );
}

/// The interface's whole 80×24 screen with no image protocol. Cover art may
/// not move a single cell of it: no protocol is ever attached to a
/// [`TestBackend`]-backed app. Re-recorded on purpose whenever the design
/// changes (ui-redesign T1: the window frame, the title bar, the key hints;
/// T2: the now-playing block; T3: the list header and numbered rows; T10:
/// the `ReplayGain` mode beside the volume).
const SCREEN_WITHOUT_COVER_ART: &str = concat!(
    "╭──────────────────────────────────────────────────────────────────────────────╮\n",
    "│ sources  queue                                                       V E L O │\n",
    "│ ♫ nothing playing                                                            │\n",
    "│ enter plays the selection                                                    │\n",
    "│                                                                              │\n",
    "│ --:--   ■ stopped                                                            │\n",
    "│ ──────────────────────────────────────────────────────────────────────────── │\n",
    "│  ◂◂   ▶ play   ▸▸    shuffle  repeat off     RG track   VOL ━━━━━━━━━━━● 100 │\n",
    "│                                                                              │\n",
    "│ ▸─ Files  [3 tracks]                                                         │\n",
    "│   1. one.flac  /music                                                        │\n",
    "│   2. two.flac  /music                                                        │\n",
    "│   3. three.flac  /music                                                      │\n",
    "│                                                                              │\n",
    "│                                                                              │\n",
    "│                                                                              │\n",
    "│                                                                              │\n",
    "│                                                                              │\n",
    "│                                                                              │\n",
    "│                                                                              │\n",
    "│                                                                              │\n",
    "│                                                                              │\n",
    "│ j/k move  enter play  space pause  ←/→ seek  / search  ? help  q quit        │\n",
    "╰──────────────────────────────────────────────────────────────────────────────╯",
);

fn screen_at(app: &App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("a test terminal");
    terminal
        .draw(|frame| app.draw(frame))
        .expect("the frame draws");
    let buffer: &Buffer = terminal.backend().buffer();
    let screen: String = buffer.content().iter().map(Cell::symbol).collect();
    let chars: Vec<char> = screen.chars().collect();
    chars
        .chunks(usize::from(width))
        .map(|line| line.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn no_protocol_and_an_80x24_terminal_draw_exactly_the_screen_without_cover_art() {
    let app = App::new(tracks(&["one.flac", "two.flac", "three.flac"]));
    let screen = screen_at(&app, 80, 24);

    assert_eq!(
        screen, SCREEN_WITHOUT_COVER_ART,
        "cover art must leave the 80×24 interface unchanged"
    );
}

// ---- the library prompt (T5, S4) ----------------------------------------

#[test]
fn the_library_prompt_is_a_floating_card_with_the_typed_path_and_its_error() {
    let mut app = App::browse(None);
    app.ask_for_library(None);
    for character in "no/such/velo-dir".chars() {
        let action = app.on_key(KeyEvent::from(KeyCode::Char(character)));
        app.apply(action);
    }
    let screen = screen_at(&app, 90, 24);
    assert!(screen.contains("library"), "{screen}");
    assert!(
        screen.contains("~/no/such/velo-dir"),
        "the typed path follows the fixed ~/\n{screen}"
    );
    assert!(
        screen.contains("tab completes"),
        "the card says how to complete\n{screen}"
    );

    let action = app.on_key(KeyEvent::from(KeyCode::Enter));
    app.apply(action);
    let screen = screen_at(&app, 90, 24);
    assert!(screen.contains("not a directory"), "{screen}");
}

#[test]
fn after_esc_the_empty_browser_says_how_to_set_the_library() {
    let mut app = App::browse(None);
    app.ask_for_library(None);
    let action = app.on_key(KeyEvent::from(KeyCode::Esc));
    app.apply(action);
    let screen = screen_at(&app, 100, 24);
    assert!(screen.contains("no library set"), "{screen}");
    assert!(screen.contains("--library"), "{screen}");
    assert!(screen.contains("config.toml"), "{screen}");
}

// ---- the library views (S3) -------------------------------------------------

#[test]
fn the_header_names_the_four_views_and_lights_the_active_one() {
    use velo::ui::theme::Theme;

    let theme = Theme::default();

    let temp = TempLibrary::new("views-header");
    temp.write_wav("Tide/2019 - Harbor/01 - Dawn.wav");
    temp.write_wav("Tide/2021 - Lantern/01 - Far.wav");
    temp.index();
    let mut app = App::browse(Some(&temp.library));
    press(&mut app, KeyCode::Char('2'));

    let screen = screen_at(&app, 80, 24);
    assert!(
        screen.contains("▸─ 1 artists  2 albums  3 tracks  4 genres   [2 albums]"),
        "got:\n{screen}"
    );
    // Every album says whose it is and its year at the right edge.
    assert!(
        screen
            .lines()
            .any(|line| line.contains("2. Lantern") && line.ends_with("Tide · 2021 │")),
        "got:\n{screen}"
    );
    let row = u16::try_from(
        screen
            .lines()
            .position(|line| line.contains("▸─ 1 artists"))
            .unwrap(),
    )
    .unwrap();
    let line = screen.lines().nth(usize::from(row)).unwrap();
    let column =
        |word: &str| u16::try_from(line[..line.find(word).unwrap()].chars().count()).unwrap();
    assert_eq!(
        cell_at(&app, column("albums"), row).fg,
        theme.accent,
        "the active view"
    );
    assert_eq!(
        cell_at(&app, column("artists"), row).fg,
        theme.dim,
        "the others"
    );
    assert_eq!(cell_at(&app, column("genres"), row).fg, theme.dim);

    // Every track names its artist and album beside its title.
    press(&mut app, KeyCode::Char('3'));
    let screen = screen_at(&app, 80, 24);
    assert!(
        screen.contains("▸─ 1 artists  2 albums  3 tracks  4 genres   [2 tracks]"),
        "got:\n{screen}"
    );
    assert!(screen.contains("1. Dawn  Tide · Harbor"), "got:\n{screen}");

    // Inside a genre the header says which one.
    press(&mut app, KeyCode::Char('4'));
    let screen = screen_at(&app, 80, 24);
    assert!(screen.contains("1. (no genre)"), "got:\n{screen}");
    press(&mut app, KeyCode::Enter);
    let screen = screen_at(&app, 80, 24);
    assert!(
        screen.contains("▸─ 1 artists  2 albums  3 tracks  4 genres   (no genre)  [2 tracks]"),
        "got:\n{screen}"
    );
}

/// The numbers in the header are the `1`–`4` keys' own row (S5.5): they show
/// by default and `[ui] view_numbers = false` leaves the bare names.
#[test]
fn the_header_numbers_the_views_and_view_numbers_false_hides_the_numbers() {
    let temp = TempLibrary::new("view-numbers");
    temp.write_wav("Tide/2019 - Harbor/01 - Dawn.wav");
    temp.index();
    let mut app = App::browse(Some(&temp.library));

    let screen = screen_at(&app, 80, 24);
    assert!(
        screen.contains("▸─ 1 artists  2 albums  3 tracks  4 genres   [1 artist]"),
        "the numbers are the keys, got:\n{screen}"
    );

    app.set_ui(UiParts {
        view_numbers: false,
        ..UiParts::default()
    });
    let screen = screen_at(&app, 80, 24);
    assert!(
        screen.contains("▸─ artists  albums  tracks  genres   [1 artist]"),
        "the bare names as before, got:\n{screen}"
    );
    assert!(!screen.contains("1 artists"), "no numbers:\n{screen}");
}

/// The cover advisory is a `[ui]` switch like any other (S5.4): on a window
/// too small for art the status line explains the hidden cover, and with
/// `advisories = false` it stays quiet — while an ordinary status message
/// still shows.
#[test]
fn the_cover_advisory_appears_on_a_small_window_and_advisories_false_silences_it() {
    let temp = TempLibrary::new("cover-advisory");
    let wav = temp.write_wav("Tide/01 - Dawn.wav");
    // Any decoded image will do for the advisory: the gate never looks at
    // the pixels.
    image::DynamicImage::new_rgba8(1, 1)
        .save(temp.root.join("Tide/cover.png"))
        .expect("a cover to decode");
    let art = velo::ui::artwork::Artwork::from_path(&wav).expect("the cover decodes");

    let mut app = App::new(tracks(&["one.flac"]));
    app.show_artwork(Some(velo::ui::artwork::Picker::new((8, 12))));
    app.show_decoded_artwork(Some(art));
    // The first frame records the window; the tick then explains the cover.
    let _ = screen_at(&app, 80, 40);
    app.tick();
    assert!(
        app.status().contains("cover art hidden"),
        "the advisory is the status, got: {:?}",
        app.status()
    );
    let screen = screen_at(&app, 80, 40);
    assert!(
        screen.contains("cover art hidden — the window is 80x40 and cover art needs 100x28"),
        "the advisory is drawn, got:\n{screen}"
    );

    // Advisories off: the same window is never explained, and ordinary
    // messages still show.
    let art = velo::ui::artwork::Artwork::from_path(&wav).expect("the cover decodes");
    let mut app = App::new(tracks(&["one.flac"]));
    app.show_artwork(Some(velo::ui::artwork::Picker::new((8, 12))));
    app.show_decoded_artwork(Some(art));
    app.set_ui(UiParts {
        advisories: false,
        ..UiParts::default()
    });
    let _ = screen_at(&app, 80, 40);
    app.tick();
    assert!(
        !app.status().contains("cover art hidden"),
        "the advisory is silenced, got: {:?}",
        app.status()
    );
    app.note("velo: indexing finished");
    let screen = screen_at(&app, 80, 40);
    assert!(
        screen.contains("velo: indexing finished"),
        "an ordinary message still shows, got:\n{screen}"
    );
}

#[test]
fn a_narrow_header_drops_the_count_before_the_view_names() {
    let temp = TempLibrary::new("views-narrow");
    temp.write_wav("A Rather Long Artist Name/2019 - An Even Longer Album Title/01 - Dawn.wav");
    temp.index();
    let mut app = App::browse(Some(&temp.library));
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Enter);

    let screen = screen_at(&app, 80, 24);
    let header = screen
        .lines()
        .find(|line| line.contains("▸─ 1 artists  2 albums  3 tracks  4 genres"))
        .expect("the views stay at 80 columns");
    assert!(
        !header.contains("[1 track]"),
        "the count went first: {header}"
    );
    assert!(header.contains("A Rather Long Artist Name ›"), "{header}");
}

// ---- refreshing the index (library-refresh T3, S3) --------------------------

#[test]
fn the_title_bar_says_indexing_while_an_index_runs_and_not_after() {
    let temp = TempLibrary::new("indexing-title");
    let mut app = App::browse(Some(&temp.library));
    app.index_started(&temp.root);
    let lines = lines_at(&app, 80, 24);
    assert!(lines[1].contains("⟳ indexing…"), "{}", lines[1]);
    let column = lines[1]
        .chars()
        .position(|character| character == '⟳')
        .unwrap();
    let cell = cell_at_size(&app, 80, 24, column, 1);
    assert_eq!(
        cell.fg,
        velo::ui::theme::Theme::default().accent,
        "the indicator is the accent"
    );
    assert!(
        lines[1].contains("V E L O"),
        "the wordmark stays: {}",
        lines[1]
    );

    app.index_finished(Err("stopped".to_owned()));
    let lines = lines_at(&app, 80, 24);
    assert!(!lines[1].contains("indexing"), "{}", lines[1]);
}

#[test]
fn the_help_names_the_key_that_refreshes_the_index() {
    let mut app = App::browse(None);
    press(&mut app, KeyCode::Char('?'));
    // The card is longer than 80×24 shows; tall enough for every row.
    let screen = screen_at(&app, 80, 50);
    assert!(screen.contains("refresh the library index"), "{screen}");
}

// ---- the help scrolls and filters (help-and-advisories T1, S1) -------------

/// The help card open on `app`, drawn at 80×14: too short for every row.
fn help_at_80x14(app: &App) -> String {
    screen_at(app, 80, 14)
}

/// The help card open on `app`, drawn at 80×40: tall enough for every row.
fn help_at_80x20(app: &App) -> String {
    screen_at(app, 80, 20)
}

fn help_at_80x40(app: &App) -> String {
    screen_at(app, 80, 40)
}

/// The rows the help card covers on `screen`: its two borders, the lines
/// between them and nothing else.
/// The range the help card declares, e.g. `1–12 of 29` → `(1, 12, 29)`.
///
/// The card is not the only place the screen says " of " — a label can too
/// ("shuffle what is left of the queue"), so every occurrence is tried and only
/// the one shaped like a range answers.
fn position_range(screen: &str) -> Option<(usize, usize, usize)> {
    for line in screen.lines() {
        let mut rest = line;
        while let Some((head, tail)) = rest.split_once(" of ") {
            let total: String = tail.chars().take_while(char::is_ascii_digit).collect();
            if let Some((first, last)) = head.rsplit_once('\u{2013}') {
                let first: String = first
                    .chars()
                    .rev()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect();
                if let (Ok(first), Ok(last), Ok(total)) =
                    (first.parse(), last.trim().parse(), total.parse())
                {
                    return Some((first, last, total));
                }
            }
            rest = tail;
        }
    }
    None
}

/// The total the help card declares, if it declares one at all.
fn position_total(screen: &str) -> Option<usize> {
    position_range(screen).map(|(_, _, total)| total)
}

fn help_card_height(screen: &str) -> usize {
    const TITLE: [char; 6] = ['╭', ' ', 'k', 'e', 'y', 's'];
    let lines: Vec<Vec<char>> = screen.lines().map(|line| line.chars().collect()).collect();
    let top = lines
        .iter()
        .position(|line| line.windows(TITLE.len()).any(|window| window == TITLE))
        .expect("the help card is on screen");
    let edge = lines[top]
        .windows(TITLE.len())
        .position(|window| window == TITLE)
        .expect("the title opens the card");
    let sides = lines[top + 1..]
        .iter()
        .take_while(|line| line.get(edge) == Some(&'│'))
        .count();
    sides + 2
}

#[test]
fn the_help_card_keeps_its_height_while_the_filter_narrows_it() {
    let mut app = App::new(tracks(&["one.flac"]));
    press(&mut app, KeyCode::Char('?'));
    let full = help_at_80x40(&app);
    let height = help_card_height(&full);
    assert!(full.contains("move down"), "{full}");
    assert!(full.contains("pause / resume"), "{full}");

    // Typing a filter adds its line without growing the card.
    press(&mut app, KeyCode::Char('/'));
    let typing = help_at_80x40(&app);
    assert!(typing.contains("▏"), "the filter is being typed:\n{typing}");
    assert_eq!(
        help_card_height(&typing),
        height,
        "the card grew when the filter opened:\n{typing}"
    );

    // One match keeps the card as tall as the whole list did.
    for character in "space".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    let one = help_at_80x40(&app);
    assert!(one.contains("pause / resume"), "{one}");
    assert!(!one.contains("move down"), "one row matches:\n{one}");
    assert_eq!(
        help_card_height(&one),
        height,
        "the card shrank to one row:\n{one}"
    );

    // And no match at all keeps it too.
    for _ in 0.."space".len() {
        press(&mut app, KeyCode::Backspace);
    }
    for character in "zzz".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    let none = help_at_80x40(&app);
    assert!(none.contains("no key matches"), "{none}");
    assert_eq!(
        help_card_height(&none),
        height,
        "the card shrank to the empty message:\n{none}"
    );
}

#[test]
fn the_help_scrolls_with_j_k_and_jumps_with_g_and_shift_g() {
    let mut app = App::new(tracks(&["one.flac"]));
    press(&mut app, KeyCode::Char('?'));
    // The card is taller than the terminal here, so it scrolls and says where it
    // is. Nothing below pins how many rows it has or how many fit: those are the
    // card's business, and a section added to it must not fail this test.
    let screen = help_at_80x20(&app);
    let Some((first, last, total)) = position_range(&screen) else {
        panic!("a scrolling card says where it is:\n{screen}");
    };
    assert_eq!(first, 1, "it starts at the top:\n{screen}");
    assert!(last < total, "and it does not fit:\n{screen}");
    assert!(screen.contains("j / Down"), "{screen}");
    assert!(!screen.contains("quit"), "the last row is below:\n{screen}");

    press(&mut app, KeyCode::Char('j'));
    let screen = help_at_80x20(&app);
    assert_eq!(
        position_range(&screen),
        Some((2, last + 1, total)),
        "one row down moves the window by one:\n{screen}"
    );

    press(&mut app, KeyCode::Char('G'));
    let screen = help_at_80x20(&app);
    assert_eq!(
        position_range(&screen),
        Some((total - last + 1, total, total)),
        "G lands on the end:\n{screen}"
    );
    assert!(screen.contains("quit"), "{screen}");
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(
        position_range(&help_at_80x20(&app)),
        Some((total - last + 1, total, total)),
        "j stops at the end"
    );

    press(&mut app, KeyCode::Char('g'));
    let screen = help_at_80x20(&app);
    assert_eq!(
        position_range(&screen),
        Some((1, last, total)),
        "g returns to the top:\n{screen}"
    );
}

#[test]
fn a_help_that_fits_shows_no_position() {
    let mut app = App::new(tracks(&["one.flac"]));
    press(&mut app, KeyCode::Char('?'));
    // Tall enough for the whole card: the property is "no position line when the
    // whole table fits", not "the table has N rows".
    let screen = screen_at(&app, 80, 50);
    assert!(
        screen.contains("quit") && screen.contains("j / Down"),
        "{screen}"
    );
    assert!(
        position_total(&screen).is_none(),
        "a card that fits shows no position line:\n{screen}"
    );
}

#[test]
fn slash_filters_the_help_and_says_when_nothing_matches() {
    let mut app = App::new(tracks(&["one.flac"]));
    press(&mut app, KeyCode::Char('?'));
    press(&mut app, KeyCode::Char('/'));
    for character in "SEEK".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    let screen = help_at_80x14(&app);
    assert!(screen.contains("/ SEEK"), "the filter shows:\n{screen}");
    assert!(screen.contains("← / →"), "{screen}");
    assert_eq!(screen.matches("seek 5 s").count(), 1, "{screen}");
    assert!(
        !screen.contains(", / ."),
        "the comma keys no longer seek (issue #38):\n{screen}"
    );
    assert!(!screen.contains("first track"), "{screen}");
    assert!(!screen.contains(" of "), "two rows fit:\n{screen}");

    for _ in 0..4 {
        press(&mut app, KeyCode::Backspace);
    }
    for character in "zzz".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    let screen = help_at_80x14(&app);
    assert!(screen.contains("/ zzz"), "{screen}");
    assert!(screen.contains("no key matches"), "{screen}");
    assert!(!screen.contains("seek 5 s"), "{screen}");
}

#[test]
fn enter_keeps_the_filter_and_j_k_scroll_again() {
    let mut app = App::new(tracks(&["one.flac"]));
    press(&mut app, KeyCode::Char('?'));
    press(&mut app, KeyCode::Char('/'));
    press(&mut app, KeyCode::Char('e'));
    let typing = help_at_80x14(&app);
    let total = typing
        .split(" of ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("many rows match e:\n{typing}"))
        .to_owned();
    press(&mut app, KeyCode::Char('j'));
    assert!(help_at_80x14(&app).contains("/ ej"), "j types while typing");
    press(&mut app, KeyCode::Backspace);

    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('j'));
    let screen = help_at_80x14(&app);
    assert!(screen.contains("/ e"), "the filter stays:\n{screen}");
    assert!(!screen.contains("/ ej"), "{screen}");
    assert!(
        screen.contains(&format!("2–12 of {total}")) || screen.contains("2–"),
        "{screen}"
    );
    assert!(screen.contains(&format!(" of {total}")), "{screen}");
}

#[test]
fn the_help_brings_back_the_library_rows() {
    let mut app = App::new(tracks(&["one.flac"]));
    press(&mut app, KeyCode::Char('?'));
    press(&mut app, KeyCode::Char('/'));
    for character in "library".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    let screen = screen_at(&app, 80, 24);
    for row in [
        "set the library folder",
        "refresh the library index",
        "no library set: a prompt asks for its path, tab completes",
    ] {
        assert!(screen.contains(row), "{row:?} in:\n{screen}");
    }
    assert!(
        !screen.contains("L / u"),
        "the merged row is gone:\n{screen}"
    );
}

// ---- showing and hiding the interface's parts (S5) --------------------------

use velo::config::UiParts;

fn app_with_ui(ui: UiParts) -> App<'static> {
    let mut app = App::new(tracks(&["one.flac", "two.flac", "three.flac"]));
    app.set_ui(ui);
    app
}

/// The top bar hidden gives its row to the content: the frame still closes
/// and the now-playing block starts on the first row inside it.
#[test]
fn hiding_the_top_bar_gives_its_row_to_the_content() {
    let app = app_with_ui(UiParts {
        top_bar: false,
        ..UiParts::default()
    });
    let screen = screen_at(&app, 80, 24);
    let lines: Vec<&str> = screen.lines().collect();

    assert!(lines[0].starts_with('╭') && lines[0].ends_with('╮'));
    assert!(lines[23].starts_with('╰') && lines[23].ends_with('╯'));
    assert!(
        !screen.contains("V E L O") && !screen.contains("sources  queue"),
        "the tabs and the wordmark are gone, got:\n{screen}"
    );
    assert!(
        lines[1].contains("♫ nothing playing"),
        "the content takes the bar's row, got: {}",
        lines[1]
    );
}

/// The hints hidden keep the status row, and no empty line is left where the
/// hints were: the status is the last row inside the frame.
#[test]
fn hiding_the_hints_keeps_the_status_row_and_no_empty_line() {
    let mut app = app_with_ui(UiParts {
        hints: false,
        ..UiParts::default()
    });
    app.note("velo: indexing finished");
    let screen = screen_at(&app, 80, 24);
    let lines: Vec<&str> = screen.lines().collect();

    assert!(
        !screen.contains("j/k move"),
        "the hints are gone, got:\n{screen}"
    );
    assert!(
        lines[22].contains("velo: indexing finished"),
        "the status is the last row inside the frame, no empty line after it, got:\n{screen}"
    );
    assert!(lines[23].starts_with('╰') && lines[23].ends_with('╯'));
}

/// The spectrum switched off resolves to `none` (issue #40, S5): the band
/// area is never reserved, so the list starts four rows higher than with
/// `bars` in force.
#[test]
fn hiding_the_spectrum_gives_its_rows_to_the_list() {
    let shown = lines_at(&app_with_ui(UiParts::default()), 80, 30);
    let header_row = shown
        .iter()
        .position(|line| line.contains("▸─ Files"))
        .expect("the list header");

    let hidden = app_with_ui(UiParts {
        visualizer: Visualizer::None,
        ..UiParts::default()
    });
    let screen = screen_at(&hidden, 80, 30);
    let lines: Vec<&str> = screen.lines().collect();

    assert!(!screen.contains("▁"), "no bars are drawn:\n{screen}");
    let new_header_row = lines
        .iter()
        .position(|line| line.contains("▸─ Files"))
        .expect("the list header");
    assert_eq!(
        new_header_row,
        header_row - 4,
        "the list starts where the bars were, got:\n{screen}"
    );
}

/// The cover hidden leaves no cover columns even with a picker installed on
/// a window above the art threshold — and a deliberately hidden cover is a
/// decision, so nothing explains it.
///
/// No fixture can hand the interface a decoded cover without a live stream
/// (the cover follows the track that plays), so the decoded-art leg of the
/// gate is pinned by a unit test beside the layout code it gates.
#[test]
fn hiding_the_cover_art_leaves_no_cover_columns_and_no_explanation() {
    let mut app = App::new(tracks(&["one.flac", "two.flac", "three.flac"]));
    app.show_artwork(Some(velo::ui::artwork::Picker::new((8, 12))));
    app.set_ui(UiParts {
        cover_art: false,
        ..UiParts::default()
    });
    let screen = screen_at(&app, 120, 30);

    let title = screen
        .lines()
        .find(|line| line.contains("♫ nothing playing"))
        .expect("the now-playing block");
    let column = title
        .chars()
        .position(|c| c == '♫')
        .expect("the block's title");
    assert_eq!(
        column, 2,
        "the block's text starts at the left edge: no cover columns, got: {title}"
    );

    app.tick();
    assert!(
        app.status().is_empty(),
        "a deliberately hidden cover is never explained, got: {:?}",
        app.status()
    );
}

/// Every part hidden at once still draws a window that closes, with the
/// lists and the block where the freed rows and columns went.
#[test]
fn hiding_every_part_at_once_still_draws_a_coherent_window() {
    let app = app_with_ui(UiParts {
        cover_art: false,
        hints: false,
        top_bar: false,
        advisories: true,
        view_numbers: true,
        low_power: false,
        visualizer: Visualizer::None,
    });
    let screen = screen_at(&app, 80, 24);
    let lines: Vec<&str> = screen.lines().collect();

    assert!(lines[0].starts_with('╭') && lines[0].ends_with('╮'));
    assert!(lines[23].starts_with('╰') && lines[23].ends_with('╯'));
    for expected in [
        "♫ nothing playing",
        "■ stopped",
        "1. one.flac",
        "3. three.flac",
    ] {
        assert!(
            screen.contains(expected),
            "the screen keeps {expected:?}, got:\n{screen}"
        );
    }
    for gone in ["V E L O", "j/k move", "▁"] {
        assert!(!screen.contains(gone), "{gone:?} is gone, got:\n{screen}");
    }
}

/// The defaults draw exactly the screen the golden test pins: a `[ui]` table
/// absent from the file changes nothing.
#[test]
fn the_default_ui_parts_draw_the_golden_screen() {
    let app = app_with_ui(UiParts::default());
    assert_eq!(
        screen_at(&app, 80, 24),
        SCREEN_WITHOUT_COVER_ART,
        "UiParts::default() must draw today's screen byte for byte"
    );
}

// ---- the settings card (S6, S7) ---------------------------------------------

/// The rows the card titled `settings` covers on `screen`: its two borders,
/// the lines between them and nothing else.
fn settings_card_height(screen: &str) -> usize {
    const TITLE: [char; 8] = [' ', 's', 'e', 't', 't', 'i', 'n', 'g'];
    let lines: Vec<Vec<char>> = screen.lines().map(|line| line.chars().collect()).collect();
    let top = lines
        .iter()
        .position(|line| {
            line.windows(TITLE.len() + 1)
                .any(|window| window[1..] == TITLE)
        })
        .expect("the settings card is on screen");
    let edge = lines[top]
        .iter()
        .position(|&cell| cell == '╭')
        .expect("the border opens the card");
    let sides = lines[top + 1..]
        .iter()
        .take_while(|line| line.get(edge) == Some(&'│'))
        .count();
    sides + 2
}

#[test]
fn the_settings_card_draws_its_title_sections_and_focused_row_in_the_accent() {
    use velo::ui::theme::Theme;

    let mut app = App::new(tracks(&["one.flac"]));
    ctrl(&mut app, velo::ui::crossterm::event::KeyCode::Char('p'));
    let screen = screen_at(&app, 80, 24);

    assert!(screen.contains(" settings "), "{screen}");
    for header in ["sound", "theme", "screen", "library"] {
        assert!(
            screen.contains(header),
            "the {header} section on:\n{screen}"
        );
    }
    for (label, value) in [
        ("replay gain", "track"),
        ("repeat", "off"),
        ("shuffle", "off"),
        ("theme", "velo"),
        ("transparent background", "off"),
        ("cover art", "shown"),
        ("hints", "shown"),
        ("advisories", "shown"),
        ("view numbers", "shown"),
        ("folder", "not set"),
    ] {
        assert!(
            screen.contains(label) && screen.contains(value),
            "{label} {value} on:\n{screen}"
        );
    }

    // The focused row is drawn in the accent colour, the headers dim. `find`
    // answers a byte offset; the buffer is indexed by columns.
    let theme = Theme::default();
    let column_of = |needle: &str| {
        screen.lines().find_map(|line| {
            let byte = line.find(needle)?;
            u16::try_from(line[..byte].chars().count()).ok()
        })
    };
    let y = u16::try_from(
        screen
            .lines()
            .position(|line| line.contains("replay gain"))
            .expect("the focused row"),
    )
    .unwrap();
    assert_eq!(
        cell_at(&app, column_of("replay gain").expect("the focused row"), y).fg,
        theme.accent,
        "the focused row is drawn in the accent:\n{screen}"
    );
    let header_y = u16::try_from(
        screen
            .lines()
            .position(|line| line.contains("sound"))
            .expect("the sound header"),
    )
    .unwrap();
    let header_cell = cell_at(
        &app,
        column_of("sound").expect("the sound header"),
        header_y,
    );
    assert_eq!(
        header_cell.fg, theme.dim,
        "the section headers are drawn dim:\n{screen}"
    );
}

#[test]
fn the_settings_card_keeps_its_height_while_the_cursor_walks_it() {
    let mut app = App::new(tracks(&["one.flac"]));
    ctrl(&mut app, velo::ui::crossterm::event::KeyCode::Char('p'));
    let height = settings_card_height(&screen_at(&app, 80, 14));

    for step in 0..20 {
        let screen = screen_at(&app, 80, 14);
        assert_eq!(
            settings_card_height(&screen),
            height,
            "the card resized at step {step}:\n{screen}"
        );
        press(&mut app, KeyCode::Char('j'));
    }
    assert!(
        screen_at(&app, 80, 14).contains("folder"),
        "the walk reached the last row"
    );
}

/// No fixture can hand the interface a decoded cover without a live stream
/// (the cover follows the track that plays), so this pins the columns leg of
/// the gate the same way `hiding_the_cover_art_leaves_no_cover_columns_and_no_explanation`
/// does — the switch flipped from inside the card, not from the file.
/// The theme picker (issue #37, issue #54): one row per theme, the factory
/// theme `velo` first, the card giving its keys over.
#[test]
fn the_theme_picker_draws_one_row_per_theme_over_the_settings_card() {
    let mut app = App::new(tracks(&["one.flac"]));
    ctrl(&mut app, velo::ui::crossterm::event::KeyCode::Char('p'));
    for _ in 0..3 {
        press(&mut app, KeyCode::Char('j'));
    }
    press(&mut app, KeyCode::Enter);

    let screen = screen_at(&app, 80, 24);
    assert!(screen.contains(" theme "), "the picker card:\n{screen}");
    for name in [
        "velo",
        "default terminal",
        "catppuccin-latte",
        "tokyo-night",
    ] {
        assert!(screen.contains(name), "{name} on:\n{screen}");
    }
    assert!(
        !screen.contains(" settings "),
        "the card gave the keys over:\n{screen}"
    );
}

#[test]
fn hiding_the_cover_from_the_card_removes_its_columns_on_the_next_frame() {
    let mut app = App::new(tracks(&["one.flac", "two.flac", "three.flac"]));
    app.show_artwork(Some(velo::ui::artwork::Picker::new((8, 12))));

    ctrl(&mut app, velo::ui::crossterm::event::KeyCode::Char('p'));
    for _ in 0..5 {
        press(&mut app, KeyCode::Char('j'));
    }
    assert_eq!(app.settings_focused(), "cover art");
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Esc);

    let screen = screen_at(&app, 120, 30);
    let title = screen
        .lines()
        .find(|line| line.contains("♫ nothing playing"))
        .expect("the now-playing block");
    let column = title
        .chars()
        .position(|c| c == '♫')
        .expect("the block's title");
    assert_eq!(
        column, 2,
        "the block's text starts at the left edge: no cover columns, got: {title}"
    );
}

#[test]
fn the_seek_prompt_card_shows_what_was_typed() {
    let mut app = App::new(tracks(&["one.flac", "two.flac", "three.flac"]));
    for code in [':', '1', ':', '2'] {
        app.on_key(KeyEvent::from(KeyCode::Char(code)));
    }

    let screen = screen_at(&app, 80, 24);
    for expected in ["Jump to where?", "1:20", "enter jump"] {
        assert!(
            screen.contains(expected),
            "the seek card must show {expected:?} while it is open"
        );
    }
}

/// A configured channel's song is a finite track (cliamp S4): its row — the
/// song's `https://…` URL, its identity (S3) — reads like any file's, with a
/// position where a station row reads `● live`, and it never shows the live
/// marker even while it sits beside one.
#[test]
fn a_remote_song_row_in_the_queue_reads_as_a_track_never_as_live() {
    let mut app = App::new(Vec::new());
    app.show_playing_for_tests(
        Entry::Station {
            url: String::from("https://stream.example/nova.mp3"),
            name: String::from("Radio Nova"),
        },
        std::time::Duration::ZERO,
        None,
        None,
    );
    app.queue_song_for_tests("https://radio.example/a.mp3");
    press(&mut app, KeyCode::Tab); // into the queue

    let screen = screen_at(&app, 90, 12);
    let song_row = screen
        .lines()
        .find(|line| line.contains("2. ") && line.contains("a.mp3"))
        .expect("the queue shows the song's row, its note the song's URL");
    assert!(
        song_row.contains("https://radio.example"),
        "the row's identity is the song's URL (S3), got: {song_row:?}"
    );
    assert!(
        !song_row.contains("● live"),
        "a finite song never reads as live, got: {song_row:?}"
    );
    let station_row = screen
        .lines()
        .find(|line| line.contains("1. ") && line.contains("Radio Nova"))
        .expect("the station beside it keeps its own row");
    assert!(
        station_row.contains("● live"),
        "the live/finite distinction holds in one queue, got: {station_row:?}"
    );
}
