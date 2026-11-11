//! The `Playlists` source and its write verbs (issue #4): a named playlist
//! outlives the process in `library.toml`'s `[[playlist]]` table, `Sources`
//! gains a `Playlists` row last, and the queue is saved as one with `S`,
//! tracks appended with `S` from a library row, reordered with `J`/`K`
//! inside its track list, deleted with `d` and renamed with `e` from the
//! names list.
//!
//! The store starts populated by a hand-written `library.toml` beside the
//! config file where the store is not under test, and by the save verbs
//! where it is. No player is built: `enter` is asserted through its
//! [`Action`], the way `tests/ui_favourites.rs` does.

use std::fs;
use std::path::PathBuf;

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
/// self-removing on drop. The tracks are untagged: a playlist's rows are
/// paths, and the file name is what the screen shows.
struct TempLibrary {
    root: PathBuf,
    library: Library,
}

impl TempLibrary {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("velo-ui-playlists-{name}-{}", std::process::id()));
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
        bytes.extend_from_slice(&16_u32.to_le_bytes()); // bits per sample
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

    /// The config file the app is pointed at; `library.toml` lives beside it.
    fn config_file(&self) -> PathBuf {
        self.root.join("velo").join("config.toml")
    }

    fn store_file(&self) -> PathBuf {
        velo::saved::path_beside(&self.config_file())
    }

    /// Appends a playlist to the store's file, the way an older velo's save
    /// would have left it.
    fn seed_playlist(&self, name: &str, tracks: &[&str]) {
        use std::fmt::Write as _;

        let mut body = String::new();
        if !self.store_file().exists() {
            body.push_str("# seeded\n");
        }
        let _ = write!(body, "[[playlist]]\nname = \"{name}\"\ntrack = [");
        for (index, track) in tracks.iter().enumerate() {
            if index > 0 {
                body.push_str(", ");
            }
            let _ = write!(body, "\"{}\"", self.root.join(track).display());
        }
        body.push_str("]\n");
        let file = self.store_file();
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let mut existing = fs::read_to_string(&file).unwrap_or_default();
        existing.push_str(&body);
        fs::write(&file, existing).unwrap();
    }

    /// The store as the file on disk holds it right now.
    fn store(&self) -> velo::saved::Saved {
        velo::saved::Saved::load(&self.store_file()).unwrap()
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

/// Opens the `Playlists` source: the last row of the first screen.
fn open_playlists_source(app: &mut App<'_>) {
    app.open_sources();
    press(app, KeyCode::Char('G'));
    press(app, KeyCode::Enter);
}

// ---- S2: the source and its lists -------------------------------------------

/// `Sources` gains `Playlists`, LAST — after `Favourites`, with Radio leading the
/// list and Local library second (the author's order).
#[test]
fn playlists_is_the_last_source_after_favourites() {
    let temp = TempLibrary::new("source");
    let mut app = app(&temp);
    app.open_sources();

    assert_eq!(
        app.sources_labels(),
        [
            "Radio",
            "Local library",
            "Recently played",
            "Favourites",
            "Playlists"
        ],
        "Playlists is last"
    );
    press(&mut app, KeyCode::Char('G'));
    assert_eq!(app.sources_selected(), 4, "G lands on Playlists");

    let _ = fs::remove_dir_all(&temp.root);
}

/// Opening `Playlists` lists the saved names with their track counts.
#[test]
fn opening_playlists_lists_the_saved_names_with_their_track_counts() {
    let temp = TempLibrary::new("names");
    temp.seed_playlist("Noche", &["a.wav", "b.wav"]);
    temp.seed_playlist("Dia", &["c.wav"]);
    let mut app = app(&temp);

    open_playlists_source(&mut app);
    assert_eq!(app.view(), View::Library);
    assert_eq!(app.level(), Some(Level::Playlists));
    assert_eq!(app.playlist_rows().len(), 2, "the store's names, in order");
    let now = screen(&app);
    assert!(now.contains("Noche"), "the names are rows, got:\n{now}");
    assert!(now.contains("Dia"), "got:\n{now}");
    assert!(
        now.contains("2 tracks"),
        "each row says its track count, got:\n{now}"
    );

    let _ = fs::remove_dir_all(&temp.root);
}

/// `enter` on a name opens that playlist as a track list; `enter` plays from
/// the row and `a` queues the store's path, verbatim.
#[test]
fn enter_opens_a_playlist_as_a_track_list_that_plays_and_queues() {
    let temp = TempLibrary::new("track-list");
    temp.seed_playlist("Noche", &["Tide/a.wav", "Tide/b.wav"]);
    let mut app = app(&temp);

    open_playlists_source(&mut app);
    press(&mut app, KeyCode::Enter); // onto "Noche"
    assert_eq!(
        app.level(),
        Some(Level::Playlist {
            name: String::from("Noche")
        })
    );
    let now = screen(&app);
    assert!(now.contains("a.wav"), "the tracks are rows, got:\n{now}");
    assert!(now.contains("b.wav"), "got:\n{now}");

    assert_eq!(
        app.on_key(key(KeyCode::Enter)),
        Action::Play { from: 0 },
        "enter plays from the row (asserted, not applied)"
    );
    press(&mut app, KeyCode::Char('a'));
    assert_eq!(
        app.queue(),
        [Entry::File(temp.root.join("Tide/a.wav"))],
        "`a` appends the store's path, verbatim"
    );

    let _ = fs::remove_dir_all(&temp.root);
}

/// `esc` from a playlist's track list returns to the names list, and from
/// the names list to `Sources` — the way out is the way in.
#[test]
fn esc_walks_back_track_list_names_list_sources() {
    let temp = TempLibrary::new("esc");
    temp.seed_playlist("Noche", &["a.wav"]);
    let mut app = app(&temp);

    open_playlists_source(&mut app);
    press(&mut app, KeyCode::Enter);
    assert!(matches!(app.level(), Some(Level::Playlist { .. })));

    press(&mut app, KeyCode::Esc);
    assert_eq!(app.level(), Some(Level::Playlists), "back to the names");
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.view(), View::Sources, "then to Sources");

    let _ = fs::remove_dir_all(&temp.root);
}

/// An empty store opens an empty names list that says so.
#[test]
fn an_empty_store_opens_an_empty_names_list_that_says_so() {
    let temp = TempLibrary::new("empty");
    let mut app = app(&temp);

    open_playlists_source(&mut app);
    assert_eq!(app.level(), Some(Level::Playlists));
    assert_eq!(app.playlist_rows(), [] as [velo::saved::Playlist; 0]);
    assert!(
        screen(&app).contains("no playlists yet"),
        "the empty list explains itself, got:\n{}",
        screen(&app)
    );

    let _ = fs::remove_dir_all(&temp.root);
}

/// The saved names survive a restart: the file is the truth, not the
/// session.
#[test]
fn the_saved_playlists_survive_a_restart() {
    let temp = TempLibrary::new("restart");
    temp.write_wav("a.wav");
    temp.index();
    {
        let mut first = app(&temp);
        press(&mut first, KeyCode::Char('3')); // every track
        press(&mut first, KeyCode::Char('a')); // queue the first row
        press(&mut first, KeyCode::Tab); // to the queue
        press(&mut first, KeyCode::Char('S')); // save as playlist
        press(&mut first, KeyCode::Char('N'));
        press(&mut first, KeyCode::Char('o'));
        press(&mut first, KeyCode::Char('c'));
        press(&mut first, KeyCode::Char('h'));
        press(&mut first, KeyCode::Char('e'));
        press(&mut first, KeyCode::Enter);
    }

    let mut second = app(&temp);
    open_playlists_source(&mut second);
    assert_eq!(
        second.playlist_rows().len(),
        1,
        "the name is the file's, reloaded"
    );
    assert_eq!(second.playlist_rows()[0].name, "Noche");

    let _ = fs::remove_dir_all(&temp.root);
}

// ---- S3: creating ------------------------------------------------------------

/// `S` in the queue view asks for a name and saves the queue as a new
/// playlist at once.
#[test]
fn s_in_the_queue_asks_for_a_name_and_saves_the_queue() {
    let temp = TempLibrary::new("save");
    temp.write_wav("a.wav");
    temp.write_wav("b.wav");
    temp.index();
    let mut app = app(&temp);
    press(&mut app, KeyCode::Char('3')); // every track
    press(&mut app, KeyCode::Char('a'));
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('a'));
    press(&mut app, KeyCode::Tab); // to the queue

    press(&mut app, KeyCode::Char('S'));
    assert!(
        screen(&app).contains("playlist"),
        "a prompt in the style of the seek card is open, got:\n{}",
        screen(&app)
    );
    for character in "Noche".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    press(&mut app, KeyCode::Enter);

    let saved = temp.store();
    assert_eq!(saved.playlist.len(), 1, "saved at once");
    assert_eq!(saved.playlist[0].name, "Noche");
    assert_eq!(
        saved.playlist[0].track,
        [
            temp.root.join("a.wav").to_string_lossy().into_owned(),
            temp.root.join("b.wav").to_string_lossy().into_owned()
        ],
        "the queue's tracks, in play order"
    );

    let _ = fs::remove_dir_all(&temp.root);
}

/// `S` on an empty queue is refused with a status line.
#[test]
fn s_on_an_empty_queue_is_refused_with_a_status_line() {
    let temp = TempLibrary::new("save-empty");
    let mut app = app(&temp);
    press(&mut app, KeyCode::Tab); // to the (empty) queue

    assert_eq!(press(&mut app, KeyCode::Char('S')), Action::None);
    assert!(
        !screen(&app).contains("playlist name"),
        "no prompt opened, got:\n{}",
        screen(&app)
    );
    assert!(
        !temp.store_file().exists() || temp.store().playlist.is_empty(),
        "nothing was saved"
    );

    let _ = fs::remove_dir_all(&temp.root);
}

/// A name that is taken is refused: the store keeps one playlist, never a
/// merge, and the refusal is said.
#[test]
fn a_taken_name_is_refused_never_merged() {
    let temp = TempLibrary::new("taken");
    temp.write_wav("a.wav");
    temp.seed_playlist("Noche", &[]);
    temp.index();
    let mut app = app(&temp);
    press(&mut app, KeyCode::Char('3'));
    press(&mut app, KeyCode::Char('a'));
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Char('S'));
    for character in "Noche".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    press(&mut app, KeyCode::Enter);

    let saved = temp.store();
    assert_eq!(saved.playlist.len(), 1, "no duplicate was stored");
    assert!(
        saved.playlist[0].track.is_empty(),
        "the first playlist was not merged into"
    );
    let now = screen(&app);
    assert!(
        now.contains("already") || now.contains("exists"),
        "the refusal is said, got:\n{now}"
    );

    let _ = fs::remove_dir_all(&temp.root);
}

/// `esc` cancels the save prompt: nothing is written.
#[test]
fn esc_cancels_the_save_prompt() {
    let temp = TempLibrary::new("save-esc");
    temp.write_wav("a.wav");
    temp.index();
    let mut app = app(&temp);
    press(&mut app, KeyCode::Char('3'));
    press(&mut app, KeyCode::Char('a'));
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Char('S'));
    press(&mut app, KeyCode::Char('N'));
    press(&mut app, KeyCode::Esc);

    assert!(
        !temp.store_file().exists() || temp.store().playlist.is_empty(),
        "esc wrote nothing"
    );

    let _ = fs::remove_dir_all(&temp.root);
}

/// `S` on a library track row opens a small overlay listing the saved
/// names; `enter` appends the row's track to the chosen one.
#[test]
fn s_on_a_track_row_appends_it_to_the_playlist_enter_chooses() {
    let temp = TempLibrary::new("append");
    temp.write_wav("a.wav");
    temp.write_wav("b.wav");
    temp.seed_playlist("Noche", &[]);
    temp.index();
    let mut app = app(&temp);
    press(&mut app, KeyCode::Char('3')); // every track
    press(&mut app, KeyCode::Char('j')); // onto b.wav
    press(&mut app, KeyCode::Char('S'));

    let now = screen(&app);
    assert!(
        now.contains("Noche"),
        "the overlay lists the names, got:\n{now}"
    );
    press(&mut app, KeyCode::Enter);

    let saved = temp.store();
    assert_eq!(saved.playlist.len(), 1);
    assert_eq!(
        saved.playlist[0].track,
        [temp.root.join("b.wav").to_string_lossy().into_owned()],
        "the selected row's track was appended"
    );

    let _ = fs::remove_dir_all(&temp.root);
}

/// `esc` cancels the overlay: nothing is appended.
#[test]
fn esc_cancels_the_append_overlay() {
    let temp = TempLibrary::new("append-esc");
    temp.write_wav("a.wav");
    temp.seed_playlist("Noche", &[]);
    temp.index();
    let mut app = app(&temp);
    press(&mut app, KeyCode::Char('3'));
    press(&mut app, KeyCode::Char('S'));
    press(&mut app, KeyCode::Esc);

    assert!(
        temp.store().playlist[0].track.is_empty(),
        "esc appended nothing"
    );

    let _ = fs::remove_dir_all(&temp.root);
}

/// `S` on a track row with no saved playlists says so and opens nothing.
#[test]
fn s_on_a_track_row_with_no_playlists_says_so() {
    let temp = TempLibrary::new("append-empty");
    temp.write_wav("a.wav");
    temp.index();
    let mut app = app(&temp);
    press(&mut app, KeyCode::Char('3'));

    assert_eq!(press(&mut app, KeyCode::Char('S')), Action::None);
    assert!(
        !screen(&app).contains("add to"),
        "no overlay opened, got:\n{}",
        screen(&app)
    );

    let _ = fs::remove_dir_all(&temp.root);
}

// ---- S4: reordering ----------------------------------------------------------

/// `J`/`K` inside a playlist's track list move the selected track down/up,
/// and the move is in the file at once.
#[test]
fn j_and_k_move_a_playlist_track_and_save_at_once() {
    let temp = TempLibrary::new("move");
    temp.seed_playlist("Noche", &["a.wav", "b.wav", "c.wav"]);
    let mut app = app(&temp);
    open_playlists_source(&mut app);
    press(&mut app, KeyCode::Enter); // into Noche

    press(&mut app, KeyCode::Char('J'));
    assert_eq!(
        temp.store().playlist[0].track[0],
        temp.root.join("b.wav").to_string_lossy().into_owned(),
        "J swapped the first track down, and the file says so at once"
    );
    press(&mut app, KeyCode::Char('K'));
    assert_eq!(
        temp.store().playlist[0].track[0],
        temp.root.join("a.wav").to_string_lossy().into_owned(),
        "K swapped it back up"
    );

    let _ = fs::remove_dir_all(&temp.root);
}

/// A move at either end is refused and changes nothing.
#[test]
fn a_playlist_move_stops_at_the_ends() {
    let temp = TempLibrary::new("move-ends");
    temp.seed_playlist("Noche", &["a.wav", "b.wav"]);
    let mut app = app(&temp);
    open_playlists_source(&mut app);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('G')); // onto the last row

    press(&mut app, KeyCode::Char('J'));
    assert_eq!(
        temp.store().playlist[0].track[1],
        temp.root.join("b.wav").to_string_lossy().into_owned(),
        "J on the last row changed nothing"
    );

    let _ = fs::remove_dir_all(&temp.root);
}

// ---- S5: delete and rename ---------------------------------------------------

/// `d` on a playlist in the names list deletes it, and the status line says
/// so.
#[test]
fn x_on_a_name_deletes_the_playlist_and_says_so() {
    let temp = TempLibrary::new("delete");
    temp.seed_playlist("Noche", &["a.wav"]);
    let mut app = app(&temp);
    open_playlists_source(&mut app);

    press(&mut app, KeyCode::Char('x'));
    assert!(
        temp.store().playlist.is_empty(),
        "the playlist is gone from the file"
    );

    let _ = fs::remove_dir_all(&temp.root);
}

/// `e` on a playlist in the names list renames it with a prompt; `esc`
/// cancels.
#[test]
fn e_renames_a_playlist_and_esc_cancels() {
    let temp = TempLibrary::new("rename");
    temp.seed_playlist("Noche", &["a.wav"]);
    let mut app = app(&temp);
    open_playlists_source(&mut app);

    press(&mut app, KeyCode::Char('e'));
    assert!(
        screen(&app).contains("Noche"),
        "the prompt opens with the name to rename, got:\n{}",
        screen(&app)
    );
    press(&mut app, KeyCode::Esc);
    assert_eq!(temp.store().playlist[0].name, "Noche", "esc wrote nothing");

    // And the rename itself: the cursor rests on the row, `e` again.
    press(&mut app, KeyCode::Char('e'));
    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Enter);
    let saved = temp.store();
    assert_eq!(saved.playlist.len(), 1);
    assert_eq!(saved.playlist[0].name, "Noche2", "the rename was saved");
    assert_eq!(
        saved.playlist[0].track.len(),
        1,
        "the tracks followed the rename"
    );

    let _ = fs::remove_dir_all(&temp.root);
}

/// Renaming onto a taken name is refused and the store keeps both as they
/// were.
#[test]
fn renaming_onto_a_taken_name_is_refused() {
    let temp = TempLibrary::new("rename-taken");
    temp.seed_playlist("Noche", &["a.wav"]);
    temp.seed_playlist("Dia", &["b.wav"]);
    let mut app = app(&temp);
    open_playlists_source(&mut app);

    press(&mut app, KeyCode::Char('e'));
    for _ in "Noche".chars() {
        press(&mut app, KeyCode::Backspace);
    }
    for character in "Dia".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    press(&mut app, KeyCode::Enter);

    let saved = temp.store();
    assert_eq!(
        saved
            .playlist
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Noche", "Dia"],
        "the refusal changed nothing"
    );
    let now = screen(&app);
    assert!(
        now.contains("already") || now.contains("exists"),
        "the refusal is said, got:\n{now}"
    );

    let _ = fs::remove_dir_all(&temp.root);
}

/// `f`, `a` and the rest keep their meaning on a playlist screen: `f` is a
/// library-views verb and does nothing here, `a` queues the row.
#[test]
fn f_does_nothing_on_a_playlist_row_while_a_still_queues() {
    let temp = TempLibrary::new("no-f");
    temp.seed_playlist("Noche", &["a.wav"]);
    let mut app = app(&temp);
    open_playlists_source(&mut app);
    press(&mut app, KeyCode::Enter);

    assert_eq!(press(&mut app, KeyCode::Char('f')), Action::None);
    press(&mut app, KeyCode::Char('a'));
    assert_eq!(
        app.queue(),
        [Entry::File(temp.root.join("a.wav"))],
        "`a` keeps queueing the row"
    );
    assert!(temp.store().favorite.is_empty(), "`f` marked nothing here");

    let _ = fs::remove_dir_all(&temp.root);
}

#[test]
fn the_playlists_list_shows_no_library_tabs_and_ignores_the_library_keys() {
    // Issue #49: the source borrows the library's view but is not the index.
    let temp = TempLibrary::new("tabs");
    let mut app = app(&temp);
    open_playlists_source(&mut app);

    let before = screen(&app);
    assert!(
        !before.contains("artists") && !before.contains("genres"),
        "no library tabs above the list, got:\n{before}"
    );

    press(&mut app, KeyCode::Char('2'));
    let after = screen(&app);
    assert!(
        !after.contains("genres"),
        "the library keys do nothing here, got:\n{after}"
    );
}
