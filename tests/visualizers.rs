//! The visualizers (issue #40): nine shapes the spectrum's band area can
//! take, the keys that choose between them, the picker, the full screen and
//! the config key.
//!
//! The shapes themselves are pure functions of (bands, waveform window,
//! elapsed time, area), so every shape test runs without a terminal: a known
//! band pattern and a known elapsed time must render a small expected
//! sample of cells, and silence must render nothing.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use clap::Parser as _;
use velo::config::{Cli, Config, UiParts};
use velo::ui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use velo::ui::ratatui::Terminal;
use velo::ui::ratatui::backend::TestBackend;
use velo::ui::ratatui::buffer::Buffer;
use velo::ui::ratatui::layout::Rect;
use velo::ui::theme::Theme;
use velo::ui::visualizer::Visualizer;
use velo::ui::{Action, App};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::from(code)
}

fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

fn press(app: &mut App<'_>, code: KeyCode) {
    let action = app.on_key(key(code));
    app.apply(action);
}

fn app() -> App<'static> {
    App::new(vec![PathBuf::from("/music/one.flac")])
}

fn cli_with_no_flags() -> Cli {
    Cli::parse_from(["velo"])
}

// ---- S2: the shapes are pure functions -------------------------------------

/// A band pattern with one silent band at the end, over the 8×4 area every
/// shape test draws into: four bands at two columns each.
const LEVELS: [f32; 4] = [0.25, 0.5, 1.0, 0.0];
const SILENCE: [f32; 4] = [0.0, 0.0, 0.0, 0.0];
const AREA: Rect = Rect {
    x: 0,
    y: 0,
    width: 8,
    height: 4,
};

/// A small known waveform: one full positive and negative swing.
const WAVEFORM: [f32; 8] = [0.0, 0.5, 1.0, 0.5, 0.0, -0.5, -1.0, -0.5];

fn drawn(
    visualizer: Visualizer,
    levels: &[f32],
    waveform: &[f32],
    elapsed: Duration,
) -> Vec<String> {
    visualizer
        .draw(levels, waveform, elapsed, AREA, &Theme::default())
        .iter()
        .map(ToString::to_string)
        .collect()
}

/// `none` draws nothing, with or without signal.
#[test]
fn none_draws_nothing_and_behaves_identically_in_silence() {
    let loud = drawn(Visualizer::None, &LEVELS, &WAVEFORM, Duration::ZERO);
    let quiet = drawn(Visualizer::None, &SILENCE, &[], Duration::ZERO);
    assert!(
        loud.iter().all(|row| row.trim().is_empty()),
        "loud: {loud:?}"
    );
    assert_eq!(loud, quiet, "none is the same whatever it is given");
    assert_eq!(loud.len(), 4, "one line per row of the area");
}

/// `bars`: filled bars per band, each one column with one of air, filling
/// from the bottom — the same band area the spectrum uses today.
#[test]
fn bars_fill_from_the_bottom_per_band() {
    let rows = drawn(Visualizer::Bars, &LEVELS, &[], Duration::ZERO);
    assert_eq!(
        rows[3], "█ █ █ ▁ ",
        "the bottom row carries every band, silent band and all"
    );
    assert_eq!(rows[0], "    █   ", "only the loudest band reaches the top");
    assert_eq!(
        rows[2], "  █ █   ",
        "the half band stops at its half height"
    );
}

/// `bars-outline`: the same bars as an outline — only each bar's top edge.
#[test]
fn bars_outline_draws_only_the_top_edge_of_each_bar() {
    let rows = drawn(Visualizer::BarsOutline, &LEVELS, &[], Duration::ZERO);
    assert_eq!(rows[0], "    █   ", "the loudest bar's top edge");
    assert_eq!(rows[2], "  █     ", "the half bar's top edge");
    assert_eq!(
        rows[3], "█     ▁ ",
        "the quarter bar's top edge, and the baseline"
    );
    assert!(
        rows.iter().all(|row| !row.contains("█ █")),
        "no bar is filled, got: {rows:?}"
    );
}

/// `wave`: one line undulating with the envelope, one dot per column.
#[test]
fn wave_draws_one_line_undulating_with_the_envelope() {
    let rows = drawn(Visualizer::Wave, &LEVELS, &[], Duration::ZERO);
    assert_eq!(
        rows[0], "    ••  ",
        "the loudest band lifts the middle columns"
    );
    assert_eq!(
        rows[2], "••      ",
        "the quietest band keeps the first columns low"
    );
    assert!(
        rows[3].trim().is_empty(),
        "the line never touches the floor: {rows:?}"
    );
}

/// `scatter`: one dot per band, at its own height, on its own column.
#[test]
fn scatter_draws_one_dot_per_band_at_its_height() {
    let rows = drawn(Visualizer::Scatter, &LEVELS, &[], Duration::ZERO);
    assert_eq!(rows[0], "    ●   ", "the loudest band's dot");
    assert_eq!(rows[1], "  ●     ", "the half band's dot");
    assert_eq!(rows[2], "●       ", "the quarter band's dot");
    assert_eq!(rows[3], "        ", "the silent band draws no dot");
}

/// `flame`: each band's bar with a peak marker above it that falls back over
/// time — at elapsed zero the peak is above the bar, later it has fallen to
/// rest on it.
#[test]
fn flame_peaks_fall_back_over_elapsed_time() {
    let at_rest = drawn(Visualizer::Flame, &LEVELS, &[], Duration::ZERO);
    assert_eq!(
        at_rest[1], "▄   █   ",
        "the quarter band's peak sits two rows up"
    );
    assert_eq!(
        at_rest[3], "█ █ █ ▁ ",
        "every loud band's bar fills from the bottom"
    );
    assert_eq!(
        at_rest[0], "  ▄ ▄   ",
        "the loudest band's peak rests on its top"
    );

    let fallen = drawn(Visualizer::Flame, &LEVELS, &[], Duration::from_secs(2));
    assert_eq!(fallen[3], "▄ █ █ ▁ ", "every peak has fallen onto its bar");
    assert_eq!(
        fallen[0], "    ▄   ",
        "the loudest band's peak still rests on its top"
    );
}

/// `matrix`: columns raining down — a head at the band's height with a
/// trail below it that moves with elapsed time.
#[test]
fn matrix_columns_rain_down_and_move_with_elapsed_time() {
    let rows = drawn(Visualizer::Matrix, &LEVELS, &[], Duration::ZERO);
    assert_eq!(rows[0], "    █   ", "the loudest band's head at the top");
    assert_eq!(
        rows[2], "█   ▒   ",
        "the quarter band's head and another's trail"
    );

    // The same bands a step later: the rain has moved, so the frame changed,
    // but deterministically — the same input is the same frame.
    let later = drawn(Visualizer::Matrix, &LEVELS, &[], Duration::from_millis(80));
    assert_ne!(rows, later, "the rain moves with time");
    assert_eq!(
        later,
        drawn(Visualizer::Matrix, &LEVELS, &[], Duration::from_millis(80)),
        "the same input draws the same frame"
    );
}

/// `scope`: the raw waveform, one dot per column.
#[test]
fn scope_draws_the_raw_waveform() {
    let rows = drawn(Visualizer::Scope, &LEVELS, &WAVEFORM, Duration::ZERO);
    assert_eq!(rows[0], "  ●     ", "the loudest positive sample");
    assert_eq!(rows[1], " ● ●    ", "the half-amplitude samples");
    assert_eq!(
        rows[3].matches('●').count(),
        3,
        "the negative samples below"
    );
}

/// `mirror`: bars mirrored from the centre — the two centre rows fill
/// first and the shape grows outward symmetrically.
#[test]
fn mirror_bars_grow_outward_from_the_centre_symmetrically() {
    let rows = drawn(Visualizer::Mirror, &LEVELS, &[], Duration::ZERO);
    assert_eq!(rows[1], "▄ █ █   ", "the centre rows carry every loud band");
    assert_eq!(rows[2], rows[1], "the shape is mirrored about the centre");
    assert_eq!(
        rows[3], "    █   ",
        "only the loudest band reaches the edge"
    );
}

/// With silence, every visualizer draws nothing (issue #40): no baseline, no
/// dot, no rain. `none` behaves identically, as the first test pins.
#[test]
fn silence_draws_nothing_for_every_visualizer() {
    for visualizer in Visualizer::ALL {
        let rows = drawn(visualizer, &SILENCE, &[0.0; 8], Duration::ZERO);
        assert!(
            rows.iter().all(|row| row.trim().is_empty()),
            "{visualizer:?} drew something in silence: {rows:?}"
        );
    }
}

/// Every visualizer is deterministic: the same input draws the same frame,
/// flame and matrix included — nothing anywhere reads a random source.
#[test]
fn every_visualizer_is_deterministic() {
    for visualizer in Visualizer::ALL {
        let first = drawn(visualizer, &LEVELS, &WAVEFORM, Duration::from_millis(120));
        let second = drawn(visualizer, &LEVELS, &WAVEFORM, Duration::from_millis(120));
        assert_eq!(first, second, "{visualizer:?} is not deterministic");
    }
}

/// Every visualizer keeps one line per row of its area, so it can be drawn
/// into the band area whatever its size.
#[test]
fn every_visualizer_draws_one_line_per_row() {
    for visualizer in Visualizer::ALL {
        let rows = drawn(visualizer, &LEVELS, &WAVEFORM, Duration::ZERO);
        assert_eq!(rows.len(), 4, "{visualizer:?} lines");
    }
}

// ---- the names and the order -----------------------------------------------

/// The picker's rows: `none` first, then the eight, in a stable order.
#[test]
fn the_visualizers_have_stable_names_in_a_stable_order() {
    let names: Vec<&str> = Visualizer::ALL.iter().map(|v| v.name()).collect();
    assert_eq!(
        names,
        [
            "none",
            "bars",
            "bars-outline",
            "wave",
            "scatter",
            "flame",
            "matrix",
            "scope",
            "mirror",
        ]
    );
    for visualizer in Visualizer::ALL {
        assert_eq!(Visualizer::from_name(visualizer.name()), Some(visualizer));
    }
    assert_eq!(Visualizer::from_name("bogus"), None);
}

// ---- S3: the keys -----------------------------------------------------------

/// `v` cycles through every visualizer in the picker's order, wrapping
/// through `none` back to `bars`.
#[test]
fn v_cycles_to_the_next_visualizer_and_wraps() {
    let mut app = app();
    assert_eq!(app.visualizer(), Visualizer::Bars);

    for expected in [
        Visualizer::BarsOutline,
        Visualizer::Wave,
        Visualizer::Scatter,
        Visualizer::Flame,
        Visualizer::Matrix,
        Visualizer::Scope,
        Visualizer::Mirror,
        Visualizer::None,
        Visualizer::Bars,
    ] {
        press(&mut app, KeyCode::Char('v'));
        assert_eq!(app.visualizer(), expected);
    }
}

/// `ctrl+v` opens the picker over the nine names, with the cursor on the
/// visualizer in force.
#[test]
fn ctrl_v_opens_the_picker_on_the_current_visualizer() {
    let mut app = app();
    assert!(!app.visualizer_picker_open());

    app.on_key(ctrl(KeyCode::Char('v')));

    assert!(app.visualizer_picker_open());
    assert_eq!(
        app.visualizer_picker_names(),
        [
            "none",
            "bars",
            "bars-outline",
            "wave",
            "scatter",
            "flame",
            "matrix",
            "scope",
            "mirror"
        ]
        .map(str::to_owned)
    );
    assert_eq!(
        app.visualizer_picker_cursor(),
        1,
        "the cursor starts on bars"
    );
}

/// The picker previews as the cursor moves — the preview is the interface —
/// and a preview never writes.
#[test]
fn moving_the_picker_cursor_previews_at_once_and_writes_nothing() {
    let dir = temp_dir("visualizer-picker-preview");
    let config = dir.join("config.toml");
    fs::write(&config, "[ui]\n").unwrap();
    let mut app = app();
    app.set_config_file(Some(config.clone()));
    let before = fs::read_to_string(&config).unwrap();

    app.on_key(ctrl(KeyCode::Char('v')));
    app.on_key(key(KeyCode::Char('j')));

    assert_eq!(
        app.visualizer(),
        Visualizer::BarsOutline,
        "the preview is applied"
    );
    assert!(app.visualizer_picker_open(), "the picker stays open");
    assert_eq!(
        fs::read_to_string(&config).unwrap(),
        before,
        "a preview never writes"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// `enter` keeps the focused visualizer, closes the picker and writes the
/// choice once; `esc` restores what was in force and writes nothing.
#[test]
fn enter_keeps_and_writes_while_esc_restores() {
    let dir = temp_dir("visualizer-picker-enter");
    let config = dir.join("config.toml");
    fs::write(&config, "[ui]\n").unwrap();
    let mut app = app();
    app.set_config_file(Some(config.clone()));

    app.on_key(ctrl(KeyCode::Char('v')));
    app.on_key(key(KeyCode::Char('j')));
    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(app.visualizer(), Visualizer::Wave);
    app.on_key(key(KeyCode::Enter));

    assert!(!app.visualizer_picker_open(), "enter closes the picker");
    assert_eq!(app.visualizer(), Visualizer::Wave);
    assert_eq!(
        fs::read_to_string(&config).unwrap(),
        "[ui]\nvisualizer = \"wave\"\n",
        "enter writes the choice"
    );

    // A second pass previews one step back and then backs out with `esc`.
    app.on_key(ctrl(KeyCode::Char('v')));
    app.on_key(key(KeyCode::Char('k')));
    assert_eq!(
        app.visualizer(),
        Visualizer::BarsOutline,
        "the preview moved back"
    );
    app.on_key(key(KeyCode::Esc));

    assert!(!app.visualizer_picker_open(), "esc closes the picker");
    assert_eq!(
        app.visualizer(),
        Visualizer::Wave,
        "esc restores what was in force"
    );
    assert_eq!(
        fs::read_to_string(&config).unwrap(),
        "[ui]\nvisualizer = \"wave\"\n",
        "esc writes nothing"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// The picker's navigation wraps at both ends, `none` included.
#[test]
fn the_visualizer_picker_wraps_at_the_ends() {
    let mut app = app();
    app.on_key(ctrl(KeyCode::Char('v')));

    app.on_key(key(KeyCode::Char('k')));
    assert_eq!(
        app.visualizer(),
        Visualizer::None,
        "up from the first row wraps"
    );
    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(
        app.visualizer(),
        Visualizer::Bars,
        "down wraps back to the start"
    );
}

/// While the picker is open it owns every key: `q` and `space` wait, exactly
/// as they do under the theme picker.
#[test]
fn every_other_key_waits_while_the_visualizer_picker_is_open() {
    let mut app = app();
    app.on_key(ctrl(KeyCode::Char('v')));

    for code in [KeyCode::Char('q'), KeyCode::Char(' '), KeyCode::Char('?')] {
        assert_eq!(
            app.on_key(key(code)),
            Action::None,
            "{code:?} reached the interface under the picker"
        );
        assert!(app.visualizer_picker_open(), "the picker stays open");
    }
    app.on_key(key(KeyCode::Esc));
    assert_eq!(
        app.on_key(key(KeyCode::Char('q'))),
        Action::Quit,
        "the keys come back when the picker closes"
    );
}

// ---- S4: the full screen ----------------------------------------------------

/// `V` takes the visualizer to the whole body: no list, no now-playing
/// block, but the header and the footer stay.
#[test]
fn v_takes_the_visualizer_to_the_whole_body() {
    let mut app = app();
    assert!(!app.visualizer_fullscreen());
    let before = render(&app);
    assert!(
        before.contains("1. one.flac"),
        "the list is there: {before}"
    );
    assert!(before.contains("♫ nothing playing"), "the block is there");

    press(&mut app, KeyCode::Char('V'));

    assert!(app.visualizer_fullscreen());
    let screen = render(&app);
    assert!(
        !screen.contains("1. one.flac"),
        "the list is gone: {screen}"
    );
    assert!(
        !screen.contains("♫"),
        "the now-playing block is gone: {screen}"
    );
    assert!(screen.contains("▸─ Files"), "the header stays: {screen}");
    assert!(screen.contains("j/k move"), "the footer stays: {screen}");
}

/// `V` again — or `esc` — returns to exactly the view it came from.
#[test]
fn v_and_esc_leave_the_full_screen_to_the_view_it_came_from() {
    let mut app = app();
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.view(), velo::ui::View::Queue, "standing on the queue");

    press(&mut app, KeyCode::Char('V'));
    assert!(app.visualizer_fullscreen());
    press(&mut app, KeyCode::Esc);
    assert!(!app.visualizer_fullscreen(), "esc leaves the full screen");
    assert_eq!(app.view(), velo::ui::View::Queue, "back to the queue");

    press(&mut app, KeyCode::Char('V'));
    press(&mut app, KeyCode::Char('V'));
    assert!(!app.visualizer_fullscreen(), "V again leaves it too");
    assert_eq!(app.view(), velo::ui::View::Queue);
}

/// The transport keys keep working while the visualizer is full screen:
/// `space`, the arrows, `n`/`p`, the volume and `q` are not swallowed.
#[test]
fn the_transport_keys_keep_working_while_full_screen() {
    let mut app = app();
    press(&mut app, KeyCode::Char('V'));

    assert_eq!(
        app.on_key(key(KeyCode::Char(' '))),
        Action::TogglePause,
        "space still pauses"
    );
    assert_eq!(
        app.on_key(key(KeyCode::Left)),
        Action::Seek { delta_secs: -5 },
        "the arrows still seek"
    );
    assert_eq!(
        app.on_key(key(KeyCode::Right)),
        Action::Seek { delta_secs: 5 }
    );
    assert_eq!(app.on_key(key(KeyCode::Char('n'))), Action::Next);
    assert_eq!(app.on_key(key(KeyCode::Char(','))), Action::Previous);
    assert_eq!(
        app.on_key(key(KeyCode::Char('-'))),
        Action::SetVolume(95),
        "the volume still works"
    );
    assert_eq!(app.on_key(key(KeyCode::Char('q'))), Action::Quit);
    assert!(
        app.visualizer_fullscreen(),
        "none of the transport keys left the full screen"
    );
}

// ---- S5: the config ---------------------------------------------------------

/// The config key resolves each written name.
#[test]
fn the_visualizer_key_resolves_the_written_name() {
    for (text, expected) in [
        ("[ui]\nvisualizer = \"wave\"\n", Visualizer::Wave),
        ("[ui]\nvisualizer = \"none\"\n", Visualizer::None),
        ("[ui]\nvisualizer = \"flame\"\n", Visualizer::Flame),
    ] {
        let dir = temp_dir("visualizer-config-present");
        let path = dir.join("config.toml");
        fs::write(&path, text).unwrap();
        let settings = Config::load_from(&path)
            .unwrap_or_else(|error| panic!("load failed: {error}"))
            .merged(&cli_with_no_flags());
        assert_eq!(settings.ui.visualizer, expected, "for {text:?}");
        let _ = fs::remove_dir_all(&dir);
    }
}

/// The old `[ui] spectrum` switch still means `none` when the new key is
/// absent: `false` resolves to `none`, `true` to `bars`, and neither to
/// `bars` — while a `visualizer` key present wins over the old switch.
#[test]
fn the_old_spectrum_switch_still_resolves_when_the_new_key_is_absent() {
    for (text, expected) in [
        ("[ui]\nspectrum = false\n", Visualizer::None),
        ("[ui]\nspectrum = true\n", Visualizer::Bars),
        ("[ui]\n", Visualizer::Bars),
        (
            "[ui]\nspectrum = false\nvisualizer = \"scatter\"\n",
            Visualizer::Scatter,
        ),
    ] {
        let dir = temp_dir("visualizer-config-legacy");
        let path = dir.join("config.toml");
        fs::write(&path, text).unwrap();
        let settings = Config::load_from(&path)
            .unwrap_or_else(|error| panic!("load failed: {error}"))
            .merged(&cli_with_no_flags());
        assert_eq!(settings.ui.visualizer, expected, "for {text:?}");
        let _ = fs::remove_dir_all(&dir);
    }
}

/// A name the file carries that no visualizer answers falls back to `bars`
/// instead of leaving the file unloadable.
#[test]
fn an_unknown_visualizer_name_falls_back_to_bars() {
    let dir = temp_dir("visualizer-config-unknown");
    let path = dir.join("config.toml");
    fs::write(&path, "[ui]\nvisualizer = \"bogus\"\n").unwrap();
    let settings = Config::load_from(&path)
        .unwrap_or_else(|error| panic!("load failed: {error}"))
        .merged(&cli_with_no_flags());
    assert_eq!(settings.ui.visualizer, Visualizer::Bars);
    let _ = fs::remove_dir_all(&dir);
}

// ---- the settings card ------------------------------------------------------

/// The spectrum row of the settings card reflects the new setting: its
/// `enter` opens the picker, and stepping it with the arrows cycles and
/// writes the choice.
#[test]
fn the_settings_spectrum_row_steps_the_visualizer_and_writes_it() {
    let dir = temp_dir("visualizer-settings-row");
    let config = dir.join("config.toml");
    fs::write(&config, "[ui]\n").unwrap();
    let mut app = app();
    app.set_config_file(Some(config.clone()));

    press_ctrl(&mut app, KeyCode::Char('p'));

    for _ in 0..6 {
        press(&mut app, KeyCode::Char('j'));
    }
    assert_eq!(
        app.settings_focused(),
        "spectrum",
        "the spectrum row is focused"
    );

    press(&mut app, KeyCode::Right);
    assert_eq!(app.visualizer(), Visualizer::BarsOutline, "stepped forward");
    assert_eq!(
        fs::read_to_string(&config).unwrap(),
        "[ui]\nvisualizer = \"bars-outline\"\n",
        "the step is written"
    );

    press(&mut app, KeyCode::Left);
    assert_eq!(app.visualizer(), Visualizer::Bars, "stepped back");
    assert_eq!(
        fs::read_to_string(&config).unwrap(),
        "[ui]\nvisualizer = \"bars\"\n",
        "and the step back is written too"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// `enter` on the spectrum row opens the visualizer picker, the way it does
/// on the theme row.
#[test]
fn enter_on_the_settings_spectrum_row_opens_the_picker() {
    let mut app = app();
    press_ctrl(&mut app, KeyCode::Char('p'));

    for _ in 0..6 {
        press(&mut app, KeyCode::Char('j'));
    }
    press(&mut app, KeyCode::Enter);

    assert!(app.visualizer_picker_open(), "the picker opened");
    assert!(!app.settings_open(), "the card handed the keys over");
}

// ---- S6: low power wins, none gives the rows back ---------------------------

/// `none` gives the band's rows back, exactly as low power does: on the same
/// tall window the list starts four rows higher than with `bars`.
#[test]
fn none_gives_the_band_rows_back_to_the_list() {
    let shown = lines_at(&app(), 80, 30);
    let header_row = shown
        .iter()
        .position(|line| line.contains("▸─ Files"))
        .expect("the list header");

    let mut quiet = app();
    quiet.set_ui(UiParts {
        visualizer: Visualizer::None,
        ..UiParts::default()
    });
    let screen = render_at(&quiet, 80, 30);
    let lines: Vec<&str> = screen.lines().collect();
    let new_header_row = lines
        .iter()
        .position(|line| line.contains("▸─ Files"))
        .expect("the list header");
    assert_eq!(
        new_header_row,
        header_row - 4,
        "the list starts where the band was, got:\n{screen}"
    );
}

/// Low power still wins over the visualizer: with `bars` chosen, a low-power
/// session draws nothing in the band area and gives its rows back.
#[test]
fn low_power_still_leaves_the_visualizer_out() {
    let mut app = app();
    app.set_ui(UiParts {
        low_power: true,
        ..UiParts::default()
    });
    let screen = render_at(&app, 100, 32);
    for glyph in ["▁", "█", "▄", "▒"] {
        assert!(
            !screen.contains(glyph),
            "low power leaves the visualizer out, got:\n{screen}"
        );
    }
}

// ---- render helpers ---------------------------------------------------------

fn render(app: &App) -> String {
    render_at(app, 90, 12)
}

fn render_at(app: &App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("a test terminal");
    terminal
        .draw(|frame| app.draw(frame))
        .expect("the frame draws");
    let buffer: &Buffer = terminal.backend().buffer();
    let screen: String = buffer
        .content()
        .iter()
        .map(|cell| cell.symbol().to_owned())
        .collect();
    let chars: Vec<char> = screen.chars().collect();
    chars
        .chunks(usize::from(width))
        .map(|line| line.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn lines_at(app: &App, width: u16, height: u16) -> Vec<String> {
    render_at(app, width, height)
        .lines()
        .map(str::to_owned)
        .collect()
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("velo-visualizers-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// One ctrl-modified key press, its action applied.
fn press_ctrl(app: &mut App<'_>, code: KeyCode) {
    let action = app.on_key(KeyEvent::new(code, KeyModifiers::CONTROL));
    app.apply(action);
}
