//! The `Favourites` source (issue #7, S2–S4): `f` on a library track row
//! marks it and again unmarks it, a marked track shows `★` before its title
//! in every library list, and `Sources` gains a `Favourites` row — last, so
//! `j` from the top still lands on `Radio` — that lists the marked tracks
//! playable and queueable like any other track list.
//!
//! The library is produced with a real index over a temp folder (the same
//! shape `tests/ui_render.rs` uses), because the marked row's identity is its
//! path and its metadata comes from the row the index holds. No player is
//! built: `enter` is asserted through its [`Action`], the way
//! `tests/ui_recent.rs` does.

use std::fs;
use std::path::PathBuf;

use lofty::config::WriteOptions;
use lofty::prelude::*;
use lofty::tag::{Tag, TagType};
use velo::audio::output::Entry;
use velo::library::Library;
use velo::ui::crossterm::event::{KeyCode, KeyEvent};
use velo::ui::ratatui::Terminal;
use velo::ui::ratatui::backend::TestBackend;
use velo::ui::ratatui::buffer::Cell;
use velo::ui::{Action, App, Level, View};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::from(code)
}

/// One key press, its action applied.
fn press(app: &mut App<'_>, code: KeyCode) -> Action {
    let action = app.on_key(key(code));
    app.apply(action);
    action
}

/// A temp library with its own index and its own `velo` config folder,
/// self-removing on drop.
struct TempLibrary {
    root: PathBuf,
    library: Library,
}

impl TempLibrary {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("velo-ui-favourites-{name}-{}", std::process::id()));
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

    /// Adds real `ID3v2` tags to a file already on disk.
    fn tag(&self, relative: &str, set: impl FnOnce(&mut Tag)) {
        let path = self.root.join(relative);
        let mut tagged = lofty::read_from_path(&path).unwrap();
        tagged.insert_tag(Tag::new(TagType::Id3v2));
        let tag = tagged.tag_mut(TagType::Id3v2).unwrap();
        set(tag);
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        tagged.save_to(&mut file, WriteOptions::default()).unwrap();
    }

    fn index(&self) {
        self.library.index(&self.root).unwrap();
    }

    /// The config file the app is pointed at; `library.toml` lives beside it.
    fn config_file(&self) -> PathBuf {
        self.root.join("velo").join("config.toml")
    }

    fn store_file(&self) -> PathBuf {
        velo::saved::path_beside(&self.config_file())
    }
}

impl Drop for TempLibrary {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// An app browsing the temp library, with the config file set the way a
/// launch does — the store is loaded from beside it.
fn app(temp: &TempLibrary) -> App<'_> {
    let mut app = App::browse(Some(&temp.library));
    app.set_config_file(Some(temp.config_file()));
    app
}

/// What the screen shows, as one string.
fn screen(app: &App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(90, 16)).expect("a test terminal");
    terminal
        .draw(|frame| app.draw(frame))
        .expect("the frame draws");
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(Cell::symbol)
        .collect()
}

/// Marks the track of the current row with `f`.
fn mark(app: &mut App<'_>) -> Action {
    press(app, KeyCode::Char('f'))
}

/// Every track's store text, straight from the file on disk.
fn stored_favorite(temp: &TempLibrary) -> Option<String> {
    let text = fs::read_to_string(temp.store_file()).ok()?;
    text.split("[[playlist]]")
        .next()
        .unwrap_or_default()
        .to_owned()
        .into()
}

// ---- S2: marking ------------------------------------------------------------

/// `f` on a track row of the library marks it, copies the row's metadata into
/// the store, and writes `library.toml` beside the config file immediately.
#[test]
fn f_on_a_track_row_marks_it_and_writes_the_file_at_once() {
    let temp = TempLibrary::new("mark");
    temp.write_wav("Tide/2019 - Harbor/01 - Nightfall.wav");
    temp.tag("Tide/2019 - Harbor/01 - Nightfall.wav", |tag| {
        tag.set_artist("Tide".to_owned());
        tag.set_album("Harbor".to_owned());
        tag.set_title("Nightfall".to_owned());
    });
    temp.index();
    let mut app = app(&temp);

    press(&mut app, KeyCode::Char('3')); // every track
    assert_eq!(app.level(), Some(Level::AllTracks));
    mark(&mut app);

    let store = stored_favorite(&temp).expect("the store file was written at once");
    assert!(store.contains("[[favorite]]"), "got:\n{store}");
    assert!(
        store.contains(&format!(
            "path = \"{}\"",
            temp.root
                .join("Tide/2019 - Harbor/01 - Nightfall.wav")
                .display()
        )),
        "the identity is the row's path exactly, got:\n{store}"
    );
    assert!(store.contains("artist = \"Tide\""), "got:\n{store}");
    assert!(store.contains("album = \"Harbor\""), "got:\n{store}");
    assert!(store.contains("title = \"Nightfall\""), "got:\n{store}");

    let _ = fs::remove_dir_all(&temp.root);
}

/// `f` again unmarks it: the store holds no row and the file says so.
#[test]
fn f_again_unmarks_the_track() {
    let temp = TempLibrary::new("unmark");
    temp.write_wav("Tide/2019 - Harbor/01 - Nightfall.wav");
    temp.index();
    let mut app = app(&temp);

    press(&mut app, KeyCode::Char('3'));
    mark(&mut app);
    assert!(temp.store_file().exists(), "the first mark wrote the file");
    mark(&mut app);

    let store = fs::read_to_string(temp.store_file()).unwrap();
    assert!(
        !store.contains("[[favorite]]"),
        "the unmark reached the file, got:\n{store}"
    );
    assert_eq!(app.favourite_rows().len(), 0);

    let _ = fs::remove_dir_all(&temp.root);
}

/// Marking twice never duplicates: three marks leave one row, and the second
/// press of the pair was the unmark.
#[test]
fn marking_twice_never_duplicates() {
    let temp = TempLibrary::new("dedupe");
    temp.write_wav("Tide/2019 - Harbor/01 - Nightfall.wav");
    temp.index();
    let mut app = app(&temp);

    press(&mut app, KeyCode::Char('3'));
    mark(&mut app);
    mark(&mut app);
    mark(&mut app);

    let saved = velo::saved::Saved::load(&temp.store_file()).unwrap();
    assert_eq!(
        saved.favorite.len(),
        1,
        "one row however often it was marked"
    );

    let _ = fs::remove_dir_all(&temp.root);
}

/// A mark survives a restart: a second app, loaded from the same config file
/// the way a launch loads it, draws the marker from the store (S3).
#[test]
fn the_marker_is_state_and_survives_a_restart() {
    let temp = TempLibrary::new("restart");
    temp.write_wav("Tide/2019 - Harbor/01 - Nightfall.wav");
    temp.tag("Tide/2019 - Harbor/01 - Nightfall.wav", |tag| {
        tag.set_title("Nightfall".to_owned());
    });
    temp.index();
    {
        let mut first = app(&temp);
        press(&mut first, KeyCode::Char('3'));
        mark(&mut first);
    }

    let mut second = app(&temp);
    press(&mut second, KeyCode::Char('3'));
    let now = screen(&second);
    assert!(
        now.contains("★ Nightfall"),
        "the marker comes from the store, not the session, got:\n{now}"
    );

    let _ = fs::remove_dir_all(&temp.root);
}

/// `f` writes nothing when there is no config file to put the store beside:
/// a mark that cannot be saved is lost, not fatal (the history's courtesy
/// rule).
#[test]
fn a_mark_without_a_config_file_is_lost_not_fatal() {
    let temp = TempLibrary::new("no-config");
    temp.write_wav("Tide/2019 - Harbor/01 - Nightfall.wav");
    temp.index();
    let mut app = App::browse(Some(&temp.library)); // no set_config_file

    press(&mut app, KeyCode::Char('3'));
    assert_eq!(mark(&mut app), Action::None, "nothing fatal happened");
    // (the assertion above is the check itself)

    let _ = fs::remove_dir_all(&temp.root);
}

// ---- S3: the marker in every library list -----------------------------------

/// The star shows in the album's track list and in a genre's track list too,
/// not only in every track.
#[test]
fn the_star_shows_in_the_album_and_genre_lists_too() {
    let temp = TempLibrary::new("star-everywhere");
    temp.write_wav("Tide/2019 - Harbor/01 - Nightfall.wav");
    temp.tag("Tide/2019 - Harbor/01 - Nightfall.wav", |tag| {
        tag.set_artist("Tide".to_owned());
        tag.set_album("Harbor".to_owned());
        tag.set_title("Nightfall".to_owned());
        tag.set_genre("Lo-Fi".to_owned());
    });
    temp.index();
    let mut app = app(&temp);

    // Mark from every track, then walk the album route.
    press(&mut app, KeyCode::Char('3'));
    mark(&mut app);
    press(&mut app, KeyCode::Char('1')); // artists
    press(&mut app, KeyCode::Enter); // Tide
    press(&mut app, KeyCode::Enter); // Harbor
    assert!(matches!(app.level(), Some(Level::Tracks { .. })));
    assert!(
        screen(&app).contains("★ Nightfall"),
        "the star follows the album list, got:\n{}",
        screen(&app)
    );

    // And the genre route.
    press(&mut app, KeyCode::Char('4')); // genres
    assert_eq!(app.level(), Some(Level::Genres));
    press(&mut app, KeyCode::Enter); // Lo-Fi
    assert!(matches!(app.level(), Some(Level::Genre { .. })));
    assert!(
        screen(&app).contains("★ Nightfall"),
        "the star follows the genre list, got:\n{}",
        screen(&app)
    );

    let _ = fs::remove_dir_all(&temp.root);
}

/// An unmarked track carries no star beside a marked one.
#[test]
fn an_unmarked_track_shows_no_star() {
    let temp = TempLibrary::new("no-star");
    temp.write_wav("Tide/2019 - Harbor/01 - Nightfall.wav");
    temp.write_wav("Tide/2019 - Harbor/02 - Later.wav");
    temp.index();
    let mut app = app(&temp);

    press(&mut app, KeyCode::Char('3'));
    mark(&mut app); // the first row only
    let now = screen(&app);
    assert!(now.contains("★ Nightfall"), "got:\n{now}");
    assert!(
        !now.contains("★ Later"),
        "the unmarked neighbour keeps no star, got:\n{now}"
    );

    let _ = fs::remove_dir_all(&temp.root);
}

// ---- S4: the list ------------------------------------------------------------

/// `Sources` gains `Favourites` after `Recently played`, so `j` from
/// the top still lands on `Radio` — and `Playlists` (issue #4) is the row
/// after it.
#[test]
fn favourites_is_before_the_last_source_and_j_from_the_top_still_lands_on_radio() {
    let temp = TempLibrary::new("sources");
    let mut app = app(&temp);
    app.open_sources();

    assert_eq!(
        app.sources_labels(),
        [
            "Local library",
            "Radio",
            "Recently played",
            "Favourites",
            "Playlists"
        ],
        "Playlists is last, Favourites before it"
    );
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(
        app.sources_selected(),
        1,
        "the first j still lands on Radio"
    );

    let _ = fs::remove_dir_all(&temp.root);
}

/// Opening `Favourites` lists the marked tracks, each row showing its star.
#[test]
fn opening_favourites_lists_the_marked_tracks() {
    let temp = TempLibrary::new("open");
    temp.write_wav("Tide/2019 - Harbor/01 - Nightfall.wav");
    temp.tag("Tide/2019 - Harbor/01 - Nightfall.wav", |tag| {
        tag.set_artist("Tide".to_owned());
        tag.set_album("Harbor".to_owned());
        tag.set_title("Nightfall".to_owned());
    });
    temp.index();
    let mut app = app(&temp);

    press(&mut app, KeyCode::Char('3'));
    mark(&mut app);
    app.open_sources();
    press(&mut app, KeyCode::Char('G')); // the last row: Playlists
    press(&mut app, KeyCode::Char('k')); // up onto Favourites
    press(&mut app, KeyCode::Enter);

    assert_eq!(app.view(), View::Library);
    assert_eq!(app.level(), Some(Level::Favourites));
    assert_eq!(app.favourite_rows().len(), 1);
    assert_eq!(
        app.favourite_rows()[0].path,
        temp.root
            .join("Tide/2019 - Harbor/01 - Nightfall.wav")
            .to_string_lossy()
    );
    let now = screen(&app);
    assert!(
        now.contains("★ Nightfall"),
        "the row shows its star, got:\n{now}"
    );
    assert!(
        now.contains("Tide · Harbor"),
        "the row names its metadata, got:\n{now}"
    );

    let _ = fs::remove_dir_all(&temp.root);
}

/// `enter` on a favourite row plays from it, answering `Play` for that row;
/// `a` queues the store's path verbatim.
#[test]
fn enter_plays_and_a_queues_a_favourite_row() {
    let temp = TempLibrary::new("play-queue");
    temp.write_wav("Tide/2019 - Harbor/01 - Nightfall.wav");
    temp.tag("Tide/2019 - Harbor/01 - Nightfall.wav", |tag| {
        tag.set_title("Nightfall".to_owned());
    });
    temp.index();
    let mut app = app(&temp);
    press(&mut app, KeyCode::Char('3'));
    mark(&mut app);
    app.open_sources();
    press(&mut app, KeyCode::Char('G'));
    press(&mut app, KeyCode::Char('k')); // up onto Favourites
    press(&mut app, KeyCode::Enter);

    assert_eq!(
        app.on_key(key(KeyCode::Enter)),
        Action::Play { from: 0 },
        "enter plays from the row (asserted, not applied: applying opens a device)"
    );
    press(&mut app, KeyCode::Char('a'));
    assert_eq!(
        app.queue(),
        [Entry::File(
            temp.root
                .join("Tide/2019 - Harbor/01 - Nightfall.wav")
                .clone()
        )],
        "`a` appends the store's path, verbatim"
    );

    let _ = fs::remove_dir_all(&temp.root);
}

/// `esc` from the favourites list returns to `Sources`, the way out is the
/// way in — the same walk the recent list takes.
#[test]
fn esc_returns_to_sources() {
    let temp = TempLibrary::new("esc");
    temp.write_wav("Tide/2019 - Harbor/01 - Nightfall.wav");
    temp.index();
    let mut app = app(&temp);
    press(&mut app, KeyCode::Char('3'));
    mark(&mut app);
    app.open_sources();
    press(&mut app, KeyCode::Char('G'));
    press(&mut app, KeyCode::Char('k')); // up onto Favourites
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.level(), Some(Level::Favourites));

    press(&mut app, KeyCode::Esc);
    assert_eq!(app.view(), View::Sources);
    assert_eq!(
        app.level(),
        Some(Level::AllTracks),
        "the library's top of its view is kept"
    );

    let _ = fs::remove_dir_all(&temp.root);
}

/// An empty store opens an empty list that says so, not an error.
#[test]
fn an_empty_store_opens_an_empty_list_that_says_so() {
    let temp = TempLibrary::new("empty");
    let mut app = app(&temp);
    app.open_sources();
    press(&mut app, KeyCode::Char('G'));
    press(&mut app, KeyCode::Char('k')); // up onto Favourites
    press(&mut app, KeyCode::Enter);

    assert_eq!(app.level(), Some(Level::Favourites));
    assert_eq!(app.favourite_rows(), [] as [velo::saved::Favorite; 0]);
    assert!(
        screen(&app).contains("nothing favourited yet"),
        "the empty list explains itself, got:\n{}",
        screen(&app)
    );

    let _ = fs::remove_dir_all(&temp.root);
}

/// Unmarking from the store leaves the list shorter the next time it is
/// opened: the list is the store's entries, never a copy the session keeps.
#[test]
fn the_list_reads_the_store_anew_every_time_it_opens() {
    let temp = TempLibrary::new("reopen");
    temp.write_wav("Tide/2019 - Harbor/01 - Nightfall.wav");
    temp.write_wav("Tide/2019 - Harbor/02 - Later.wav");
    temp.index();
    let mut app = app(&temp);
    press(&mut app, KeyCode::Char('3'));
    mark(&mut app); // 01
    press(&mut app, KeyCode::Char('j'));
    mark(&mut app); // 02
    app.open_sources();
    press(&mut app, KeyCode::Char('G'));
    press(&mut app, KeyCode::Char('k')); // up onto Favourites
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.favourite_rows().len(), 2);

    // Walk back out, unmark one, and open the list again.
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char('g')); // the top of Sources: Local library
    press(&mut app, KeyCode::Enter); // into the library
    assert_eq!(app.level(), Some(Level::AllTracks));
    press(&mut app, KeyCode::Char('k')); // onto the first row
    mark(&mut app); // unmark 01
    app.open_sources();
    press(&mut app, KeyCode::Char('G'));
    press(&mut app, KeyCode::Char('k')); // up onto Favourites
    press(&mut app, KeyCode::Enter);

    assert_eq!(
        app.favourite_rows().len(),
        1,
        "the list is the store's, re-read on open"
    );
    assert_eq!(
        app.favourite_rows()[0].path,
        temp.root
            .join("Tide/2019 - Harbor/02 - Later.wav")
            .to_string_lossy(),
        "the row that was left is the one that was never unmarked"
    );

    let _ = fs::remove_dir_all(&temp.root);
}
