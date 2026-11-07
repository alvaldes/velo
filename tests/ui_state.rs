//! The keymap layer, tested without a terminal: every rule of
//! `App::on_key` observed through the returned [`Action`].

use std::fs;
use std::path::{Path, PathBuf};

use lofty::config::WriteOptions;
use lofty::prelude::*;
use lofty::tag::{Tag, TagType};
use velo::library::Library;
use velo::ui::crossterm::event::{KeyCode, KeyEvent};
use velo::ui::{Action, App, Level};

fn app_with_three_tracks() -> App<'static> {
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
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::Char(','),
        KeyCode::Char('.'),
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

// ---- transport keys (T6) ------------------------------------------------

#[test]
fn left_and_right_and_comma_and_dot_seek_five_seconds() {
    let mut app = app_with_three_tracks();

    // `Left` and `,` seek back five seconds…
    assert_eq!(
        app.on_key(key(KeyCode::Left)),
        Action::Seek { delta_secs: -5 }
    );
    assert_eq!(
        app.on_key(key(KeyCode::Char(','))),
        Action::Seek { delta_secs: -5 }
    );
    // …and `Right` and `.` seek forward five seconds.
    assert_eq!(
        app.on_key(key(KeyCode::Right)),
        Action::Seek { delta_secs: 5 }
    );
    assert_eq!(
        app.on_key(key(KeyCode::Char('.'))),
        Action::Seek { delta_secs: 5 }
    );
}

#[test]
fn the_seek_target_never_leaves_the_track() {
    use std::time::Duration;

    use velo::audio::seek_target;

    let short = Some(Duration::from_secs(3));
    // Back past the start clamps to the start…
    assert_eq!(seek_target(Duration::ZERO, -5, short), Duration::ZERO);
    // …and forward past the end clamps to the end.
    assert_eq!(
        seek_target(Duration::from_secs(2), 5, short),
        Duration::from_secs(3)
    );
}

// ---- browsing the library (T5) ------------------------------------------

/// A temp library with its own index, self-removing on drop. Same guarantee
/// as `tests/library.rs`: the user's real index is never touched, and these
/// tests never set `VELO_DB` (`std::env::set_var` is `unsafe` in edition 2024
/// and these tests run in parallel in one process).
struct TempLibrary {
    root: PathBuf,
    library: Library,
}

impl TempLibrary {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("velo-ui-{name}-{}", std::process::id()));
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

/// Adds `ID3v2` tags to a file already on disk; tags win over the path layout.
fn tag_file(path: &Path, set: impl FnOnce(&mut Tag)) {
    let mut tagged = lofty::read_from_path(path).unwrap();
    tagged.insert_tag(Tag::new(TagType::Id3v2));
    let tag = tagged.tag_mut(TagType::Id3v2).unwrap();
    set(tag);
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    tagged.save_to(&mut file, WriteOptions::default()).unwrap();
}

#[test]
fn enter_on_an_artist_shows_its_albums_and_esc_returns_to_the_artists() {
    let temp = TempLibrary::new("browse-artists");
    temp.write_wav("Tide/2019 - Harbor/01 - Dawn.wav");
    temp.write_wav("Tide/2019 - Lantern/01 - Far.wav");
    temp.index();

    let mut app = App::browse(Some(&temp.library));
    assert_eq!(app.level(), Some(Level::Artists));
    assert_eq!(app.artists().len(), 1);

    assert_eq!(app.on_key(key(KeyCode::Enter)), Action::Descend);
    app.apply(Action::Descend);

    assert_eq!(
        app.level(),
        Some(Level::Albums {
            artist: "Tide".to_owned()
        })
    );
    let titles: Vec<&str> = app
        .albums()
        .iter()
        .map(|album| album.title.as_str())
        .collect();
    assert_eq!(titles, ["Harbor", "Lantern"]);

    assert_eq!(app.on_key(key(KeyCode::Esc)), Action::Ascend);
    app.apply(Action::Ascend);
    assert_eq!(app.level(), Some(Level::Artists));
}

#[test]
fn enter_on_an_album_shows_its_tracks_ordered_by_disc_and_track_number() {
    let temp = TempLibrary::new("browse-album");
    // The tags, not the walk order, decide the rows: the second disc's first
    // track comes after every track of the first disc. Without the disc tags
    // the fallback order would be First, Later, Middle — different.
    let later = temp.write_wav("Tide/2019 - Harbor/01 - Later.wav");
    tag_file(&later, |tag| {
        tag.set_track(1);
        tag.set_disk(2);
    });
    let middle = temp.write_wav("Tide/2019 - Harbor/02 - Middle.wav");
    tag_file(&middle, |tag| {
        tag.set_track(2);
        tag.set_disk(1);
    });
    let first = temp.write_wav("Tide/2019 - Harbor/01 - First.wav");
    tag_file(&first, |tag| {
        tag.set_track(1);
        tag.set_disk(1);
    });
    temp.index();

    let mut app = App::browse(Some(&temp.library));
    assert_eq!(app.on_key(key(KeyCode::Enter)), Action::Descend);
    app.apply(Action::Descend); // the albums of Tide
    assert_eq!(app.on_key(key(KeyCode::Enter)), Action::Descend);
    app.apply(Action::Descend); // the tracks of Harbor

    let titles: Vec<String> = app.tracks().iter().map(|t| t.title.clone()).collect();
    assert_eq!(titles, ["First", "Middle", "Later"]);
}

#[test]
fn enter_on_a_track_plays_the_whole_album_from_the_selection() {
    let temp = TempLibrary::new("browse-play");
    temp.write_wav("Tide/2019 - Harbor/01 - A.wav");
    temp.write_wav("Tide/2019 - Harbor/02 - B.wav");
    temp.write_wav("Tide/2019 - Harbor/03 - C.wav");
    temp.index();

    let mut app = App::browse(Some(&temp.library));
    app.on_key(key(KeyCode::Enter));
    app.apply(Action::Descend);
    app.on_key(key(KeyCode::Enter));
    app.apply(Action::Descend);
    app.on_key(key(KeyCode::Char('j')));
    app.on_key(key(KeyCode::Char('j')));

    // Playing starts the album at the selection: the action carries the
    // album-relative offset and `apply` queues the rest of the album.
    assert_eq!(app.on_key(key(KeyCode::Enter)), Action::Play { from: 2 });
    assert_eq!(app.selected(), 2);
}

#[test]
fn ascending_from_the_artist_level_stays_at_the_artists() {
    let temp = TempLibrary::new("browse-ascend");
    temp.write_wav("Tide/2019 - Harbor/01 - Dawn.wav");
    temp.index();

    let mut app = App::browse(Some(&temp.library));
    // All the way down to the tracks.
    for _ in 0..2 {
        assert_eq!(app.on_key(key(KeyCode::Enter)), Action::Descend);
        app.apply(Action::Descend);
    }

    // Two ascents land back at the artists…
    for _ in 0..2 {
        assert_eq!(app.on_key(key(KeyCode::Esc)), Action::Ascend);
        app.apply(Action::Ascend);
    }
    assert_eq!(app.level(), Some(Level::Artists));

    // …and a third, plus `Backspace`, is absorbed there instead of
    // underflowing the level stack.
    assert_eq!(app.on_key(key(KeyCode::Esc)), Action::Ascend);
    app.apply(Action::Ascend);
    assert_eq!(app.level(), Some(Level::Artists));

    assert_eq!(app.on_key(key(KeyCode::Backspace)), Action::Ascend);
    app.apply(Action::Ascend);
    assert_eq!(app.level(), Some(Level::Artists));
}
