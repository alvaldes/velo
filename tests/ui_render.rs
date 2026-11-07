//! What the shell actually puts on the screen (S1, S13).
//!
//! A terminal interface can be tested without a terminal: ratatui's test backend
//! renders a frame into a buffer, so the layout, the empty state and the key
//! hints are asserted instead of eyeballed. These tests exist because the
//! interface was first verified by construction, and construction never noticed
//! a separator dangling in the footer.

use std::fs;
use std::path::PathBuf;

use velo::library::Library;
use velo::ui::App;
use velo::ui::crossterm::event::{KeyCode, KeyEvent};
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
    for expected in ["library", "queue", "V E L O"] {
        assert!(
            lines[1].contains(expected),
            "the title bar shows {expected:?}, got: {}",
            lines[1]
        );
    }
}

#[test]
fn the_title_bar_lights_the_view_that_tab_switches_to() {
    use velo::ui::theme::{ACCENT, DIM};

    let mut app = App::new(tracks(&["one.flac"]));
    let library_x = 3; // "│ library  queue": the `l` of library
    let queue_x = 12; // the `q` of queue
    assert_eq!(cell_at(&app, library_x, 1).fg, ACCENT);
    assert_eq!(cell_at(&app, queue_x, 1).fg, DIM);

    press(&mut app, KeyCode::Tab);
    assert_eq!(cell_at(&app, library_x, 1).fg, DIM);
    assert_eq!(cell_at(&app, queue_x, 1).fg, ACCENT);
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

#[test]
fn a_tall_window_draws_the_spectrum_as_a_quiet_baseline_when_nothing_plays() {
    use velo::ui::theme::BORDER;

    let app = App::new(tracks(&["one.flac"]));
    let tall = lines_at(&app, 100, 32);
    let baseline = tall
        .iter()
        .position(|line| line.contains("▁ ▁ ▁ ▁"))
        .unwrap_or_else(|| panic!("a baseline of bars, got:\n{}", tall.join("\n")));
    let clock = tall
        .iter()
        .position(|line| line.contains("■ stopped"))
        .unwrap();
    let progress = tall
        .iter()
        .position(|line| line.starts_with("│ ──────"))
        .unwrap();
    assert!(
        clock < baseline && baseline < progress,
        "the bars sit between the clock and the progress bar"
    );
    let x = tall[baseline].chars().position(|c| c == '▁').unwrap();
    assert_eq!(
        cell_at_size(&app, 100, 32, x, baseline).fg,
        BORDER,
        "silence is drawn as quietly as the frame"
    );

    // The 80×24 window keeps its seven-row block: no spectrum there.
    assert!(!screen_at(&app, 80, 24).contains("▁ ▁ ▁"));
}

#[test]
fn a_quiet_player_redraws_slowly_and_nothing_is_animating() {
    let app = App::new(tracks(&["one.flac"]));
    assert_eq!(app.frame_interval(), std::time::Duration::from_millis(100));
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
    use velo::ui::theme::SECONDARY;

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
    assert_eq!(cell.fg, SECONDARY, "keys stand apart from what they do");
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

// ---- the lists (ui-redesign T3, S1, S3) ------------------------------------

#[test]
fn a_list_has_a_header_line_and_numbered_rows_on_an_accent_selection() {
    use velo::ui::ratatui::style::Color;

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
    // the selection is the terminal's accent with its darkest color on top.
    assert_eq!(selected.bg, Color::Magenta, "the selection is the accent");
    assert_eq!(selected.fg, Color::Black, "and its text the darkest color");
}

/// ui-redesign T8 (S5): the interface wears the terminal's own colors. It
/// paints no background of its own, so a translucent terminal stays
/// translucent, and every role is an ANSI palette entry the theme defines.
#[test]
fn the_interface_takes_its_colors_from_the_terminal_palette() {
    use velo::ui::ratatui::style::Color;
    use velo::ui::theme::{ACCENT, BORDER, DIM, SECONDARY, TEXT};

    let app = App::new(tracks(&["one.flac"]));
    let empty = cell_at(&app, 40, 15);
    assert_eq!(empty.bg, Color::Reset, "the terminal's own background");

    let palette = [ACCENT, SECONDARY, DIM, BORDER, TEXT];
    assert!(
        palette.iter().all(|color| !matches!(color, Color::Rgb(..))),
        "no role is a fixed RGB color: {palette:?}"
    );
    assert_eq!(cell_at(&app, 0, 0).fg, BORDER, "the frame");
}

#[test]
fn library_rows_put_their_facts_against_the_right_edge() {
    let temp = TempLibrary::new("right-facts");
    temp.write_wav("Tide/2019 - Harbor/01 - Dawn.wav");
    temp.index();
    let mut app = App::browse(Some(&temp.library));

    let artists = screen_at(&app, 80, 24);
    assert!(
        artists.contains("▸─ Artists  [1 artist]"),
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
        tracks.contains("▸─ Tide › Harbor  [1 track]"),
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

// ---- cover art (T8, S10) ---------------------------------------------------

/// The interface's whole 80×24 screen with no image protocol. Cover art may
/// not move a single cell of it: no protocol is ever attached to a
/// [`TestBackend`]-backed app. Re-recorded on purpose whenever the design
/// changes (ui-redesign T1: the window frame, the title bar, the key hints;
/// T2: the now-playing block; T3: the list header and numbered rows; T10:
/// the `ReplayGain` mode beside the volume).
const SCREEN_WITHOUT_COVER_ART: &str = concat!(
    "╭──────────────────────────────────────────────────────────────────────────────╮\n",
    "│ library  queue                                                       V E L O │\n",
    "│ ♫ nothing playing                                                            │\n",
    "│ enter plays the selection                                                    │\n",
    "│                                                                              │\n",
    "│ --:--   ■ stopped                                                            │\n",
    "│ ──────────────────────────────────────────────────────────────────────────── │\n",
    "│  ◂◂   ▶ play   ▸▸                            RG track   VOL ━━━━━━━━━━━● 100 │\n",
    "│                                                                              │\n",
    "│ ▸─ Files  [3 tracks]                                                         │\n",
    "│ ▸ 1. one.flac  /music                                                        │\n",
    "│ ▸ 2. two.flac  /music                                                        │\n",
    "│ ▸ 3. three.flac  /music                                                      │\n",
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
