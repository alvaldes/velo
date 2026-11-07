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

#[test]
fn the_footer_shows_the_volume_and_the_help_lists_the_seek_keys() {
    let app = App::new(tracks(&["one.flac"]));
    let screen = render(&app);

    assert!(
        screen.contains("vol 100%"),
        "the footer shows the volume percentage, got: {screen}"
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

/// The interface's 80×24 screen exactly as it was the commit before cover art
/// touched [`App::draw`]. The artwork change may not move a single cell at
/// this size: no protocol is ever attached to a [`TestBackend`]-backed app.
const SCREEN_BEFORE_COVER_ART: &str = concat!(
    "velo  3 tracks                                                           stopped\n",
    "  one.flac  /music                                                              \n",
    "  two.flac  /music                                                              \n",
    "  three.flac  /music                                                            \n",
    "                                                                                \n",
    "                                                                                \n",
    "                                                                                \n",
    "                                                                                \n",
    "                                                                                \n",
    "                                                                                \n",
    "                                                                                \n",
    "                                                                                \n",
    "                                                                                \n",
    "                                                                                \n",
    "                                                                                \n",
    "                                                                                \n",
    "                                                                                \n",
    "                                                                                \n",
    "                                                                                \n",
    "                                                                                \n",
    "                                                                                \n",
    "                                                                                \n",
    "vol 100%                                                                        \n",
    "j/k move  enter play  space pause  ←/→ seek  / search  tab queue  ? help  q quit",
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
fn no_protocol_and_an_80x24_terminal_draw_exactly_the_screen_before_cover_art() {
    let app = App::new(tracks(&["one.flac", "two.flac", "three.flac"]));
    let screen = screen_at(&app, 80, 24);

    assert_eq!(
        screen, SCREEN_BEFORE_COVER_ART,
        "cover art must leave the 80×24 interface unchanged"
    );
}
