//! The keymap layer, tested without a terminal: every rule of
//! `App::on_key` observed through the returned [`Action`].

use std::fs;
use std::path::{Path, PathBuf};

use lofty::config::WriteOptions;
use lofty::prelude::*;
use lofty::tag::{Tag, TagType};
use velo::audio::output::TrackFailure;
use velo::library::Library;
use velo::ui::crossterm::event::{KeyCode, KeyEvent};
use velo::ui::{Action, App, BrowseBy, Level, View};

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

/// One key press, its action applied.
fn press(app: &mut App<'_>, code: KeyCode) {
    let action = app.on_key(key(code));
    app.apply(action);
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

// ---- shuffle and repeat --------------------------------------------------------

#[test]
fn r_cycles_repeat_through_off_all_and_one() {
    use velo::audio::output::Repeat;
    let mut app = app_with_three_tracks();

    assert_eq!(
        app.on_key(key(KeyCode::Char('r'))),
        Action::SetRepeat(Repeat::All)
    );
    assert_eq!(
        app.on_key(key(KeyCode::Char('r'))),
        Action::SetRepeat(Repeat::One)
    );
    assert_eq!(
        app.on_key(key(KeyCode::Char('r'))),
        Action::SetRepeat(Repeat::Off)
    );
}

/// Five tracks queued with `a`, in their list order.
fn five_queued() -> (App<'static>, Vec<PathBuf>) {
    let paths: Vec<PathBuf> = (1..=5)
        .map(|n| PathBuf::from(format!("/library/artist/{n}.flac")))
        .collect();
    let mut app = App::new(paths.clone());
    for _ in 0..5 {
        let action = app.on_key(key(KeyCode::Char('a')));
        app.apply(action);
        app.on_key(key(KeyCode::Char('j')));
    }
    assert_eq!(app.queue(), paths.as_slice());
    (app, paths)
}

#[test]
fn s_shuffles_the_queue_and_s_again_puts_it_back_in_order() {
    let (mut app, paths) = five_queued();

    let action = app.on_key(key(KeyCode::Char('s')));
    assert_eq!(action, Action::ToggleShuffle);
    app.apply(action);
    let mut shuffled = app.queue().to_vec();
    shuffled.sort();
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(shuffled, sorted, "the same tracks, nothing lost or doubled");

    let action = app.on_key(key(KeyCode::Char('s')));
    app.apply(action);
    assert_eq!(app.queue(), paths.as_slice(), "back in the order they came");
}

#[test]
fn a_track_queued_while_shuffled_follows_the_rest_when_shuffle_ends() {
    let (mut app, mut paths) = five_queued();
    press(&mut app, KeyCode::Char('s'));
    // Back to the top of the list and queue the first track once more.
    app.on_key(key(KeyCode::Char('g')));
    let action = app.on_key(key(KeyCode::Char('a')));
    app.apply(action);
    press(&mut app, KeyCode::Char('s'));

    paths.push(paths[0].clone());
    assert_eq!(app.queue(), paths.as_slice());
}

#[test]
fn n_and_p_ask_for_the_next_and_the_previous_track() {
    let mut app = app_with_three_tracks();

    assert_eq!(app.on_key(key(KeyCode::Char('n'))), Action::Next);
    assert_eq!(app.on_key(key(KeyCode::Char('p'))), Action::Previous);
}

#[test]
fn next_and_previous_with_nothing_playing_change_nothing() {
    let mut app = app_with_three_tracks();

    // Like a seek, a skip moves what plays; with nothing playing there is
    // nothing to move, and nothing starts behind the user's back.
    app.apply(Action::Next);
    app.apply(Action::Previous);
    assert!(app.queue().is_empty(), "no stream was started");
    assert_eq!(app.status(), "");
}

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

// ---- the queue view (T18b, S11) -----------------------------------------

/// Queues the three argv tracks in order without a device: select, append,
/// step down, append, step down, append.
fn app_with_a_queue_of_three() -> App<'static> {
    let mut app = app_with_three_tracks();
    for _ in 0..3 {
        app.on_key(key(KeyCode::Char('a')));
        app.on_key(key(KeyCode::Char('j')));
    }
    app
}

#[test]
fn a_appends_the_selected_track_to_the_queue_without_starting_a_stream() {
    let mut app = app_with_three_tracks();
    app.on_key(key(KeyCode::Char('j'))); // onto two.flac
    assert_eq!(app.queue(), [] as [PathBuf; 0]);

    // `a` answers with no action: appending never interrupts what is playing,
    // and here it does not even start a stream.
    assert_eq!(app.on_key(key(KeyCode::Char('a'))), Action::None);
    assert_eq!(app.queue(), [PathBuf::from("/library/artist/two.flac")]);
    assert_eq!(app.selected(), 1, "the library selection is untouched");
    assert_eq!(app.status(), "velo: added 1 track");

    app.on_key(key(KeyCode::Char('a')));
    assert_eq!(
        app.queue().len(),
        2,
        "appending twice queues the track twice"
    );
}

#[test]
fn a_on_an_album_appends_all_of_its_tracks_in_order() {
    let temp = TempLibrary::new("queue-album");
    temp.write_wav("Tide/2019 - Harbor/01 - Dawn.wav");
    temp.write_wav("Tide/2019 - Harbor/02 - Later.wav");
    temp.write_wav("Tide/2019 - Lantern/01 - Far.wav");
    temp.index();

    let mut app = App::browse(Some(&temp.library));
    app.on_key(key(KeyCode::Enter));
    app.apply(Action::Descend); // the albums of Tide

    // The selection stands on Harbor: `a` queues every track of that album.
    assert_eq!(app.on_key(key(KeyCode::Char('a'))), Action::None);
    let names: Vec<String> = app
        .queue()
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["01 - Dawn.wav", "02 - Later.wav"]);
    assert_eq!(app.status(), "velo: added 2 tracks");
    assert_eq!(app.view(), View::Library, "appending stays in the browser");
}

#[test]
fn d_on_an_empty_queue_refuses_and_says_why() {
    let mut app = app_with_three_tracks();
    app.on_key(key(KeyCode::Tab));

    assert_eq!(app.on_key(key(KeyCode::Char('d'))), Action::None);
    assert_eq!(app.queue(), [] as [PathBuf; 0], "nothing was removed");
    assert_eq!(app.status(), "velo: nothing selected in the queue");
}

#[test]
fn d_removes_a_pending_entry_and_keeps_the_selection_inside_the_queue() {
    let mut app = app_with_a_queue_of_three();
    app.on_key(key(KeyCode::Tab));
    app.on_key(key(KeyCode::Char('j'))); // onto two.flac, the middle entry

    assert_eq!(app.on_key(key(KeyCode::Char('d'))), Action::None);
    let names: Vec<String> = app
        .queue()
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["one.flac", "three.flac"]);
    assert_eq!(app.status(), "velo: removed two.flac");
    assert_eq!(app.queue_selected(), 1, "the selection stays on the screen");
}

#[test]
fn k_then_j_moves_a_pending_entry_and_the_selection_moves_with_it() {
    let mut app = app_with_a_queue_of_three();
    app.on_key(key(KeyCode::Tab));
    app.on_key(key(KeyCode::Char('G'))); // onto three.flac, the last entry
    assert_eq!(app.queue_selected(), 2);

    // `K` lifts the last entry over the one above it…
    assert_eq!(app.on_key(key(KeyCode::Char('K'))), Action::None);
    let lifted: Vec<String> = app
        .queue()
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(lifted, ["one.flac", "three.flac", "two.flac"]);
    assert_eq!(
        app.queue_selected(),
        1,
        "the selection moves with the entry"
    );

    // …and `J` puts it back where it was.
    assert_eq!(app.on_key(key(KeyCode::Char('J'))), Action::None);
    let dropped: Vec<String> = app
        .queue()
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(dropped, ["one.flac", "two.flac", "three.flac"]);
    assert_eq!(app.queue_selected(), 2);
}

#[test]
fn tab_switches_the_view_and_the_status_names_it() {
    let mut app = app_with_a_queue_of_three();
    assert_eq!(app.view(), View::Library);

    assert_eq!(app.on_key(key(KeyCode::Tab)), Action::None);
    assert_eq!(app.view(), View::Queue);
    assert_eq!(app.status(), "velo: queue, 3 tracks");

    assert_eq!(app.on_key(key(KeyCode::Tab)), Action::None);
    assert_eq!(app.view(), View::Library);
    assert_eq!(
        app.status(),
        "",
        "returning to the library clears the status"
    );
}

#[test]
fn enter_in_the_queue_view_plays_from_the_queue_selection() {
    let mut app = app_with_a_queue_of_three();
    app.on_key(key(KeyCode::Tab));
    app.on_key(key(KeyCode::Char('j'))); // onto two.flac

    assert_eq!(
        app.on_key(key(KeyCode::Enter)),
        Action::PlayQueue { from: 1 }
    );
    // Not applying it: `apply` would open a sound device.
}

#[test]
fn enter_on_an_empty_queue_view_does_nothing() {
    let mut app = app_with_three_tracks();
    app.on_key(key(KeyCode::Tab));
    assert_eq!(app.on_key(key(KeyCode::Enter)), Action::None);
}

#[test]
fn the_movement_keys_follow_the_active_view_and_leave_the_other_selection() {
    let mut app = app_with_a_queue_of_three();
    // Queuing three tracks stepped the library selection down to its last row;
    // whatever it is, it must not move while the queue view is active.
    let library_selection = app.selected();
    assert_eq!(library_selection, 2);

    app.on_key(key(KeyCode::Tab));
    app.on_key(key(KeyCode::Char('G')));
    assert_eq!(app.queue_selected(), 2);
    assert_eq!(
        app.selected(),
        library_selection,
        "`G` in the queue leaves the library alone"
    );

    // `j` at the last queue row clamps instead of wrapping…
    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(app.queue_selected(), 2);
    // …`k` walks back up, `g` jumps to the head.
    app.on_key(key(KeyCode::Char('k')));
    assert_eq!(app.queue_selected(), 1);
    app.on_key(key(KeyCode::Char('g')));
    assert_eq!(app.queue_selected(), 0);
    assert_eq!(app.selected(), library_selection);

    // And back in the library, the keys move the library selection again.
    app.on_key(key(KeyCode::Tab));
    app.on_key(key(KeyCode::Char('k')));
    assert_eq!(app.selected(), library_selection - 1);
    assert_eq!(app.queue_selected(), 0, "the queue selection is untouched");
}

// ---- the playing-entry refusals (T18b, S11) -----------------------------
//
// The rules that need a live player — `d` on the playing entry, `J`/`K`
// across it — cannot be driven through `App::on_key` here: marking an entry
// as playing means opening a sound device. The rule itself is pure and lives
// in one place that `remove_from_queue` and `move_in_queue` both read, so it
// is pinned in `src/ui/mod.rs`'s own `#[cfg(test)]` module instead.

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

// ---- skipped tracks (T19) ----------------------------------------------

#[test]
fn a_skipped_track_is_announced_by_file_name_and_stays_until_the_next_update() {
    let mut app = app_with_three_tracks();
    assert_eq!(app.status(), "");

    app.announce_failure(&TrackFailure {
        path: PathBuf::from("/library/Artist/2019 - Harbor/02 - broken.wav"),
        message: "unsupported format: not a container".to_owned(),
    });
    assert_eq!(
        app.status(),
        "velo: skipped 02 - broken.wav: unsupported format: not a container",
        "the message names the file, not its whole path",
    );

    // A skipped track is not a sequence error: ticks that bring no new event
    // must not erase the message.
    app.tick();
    app.tick();
    assert_eq!(
        app.status(),
        "velo: skipped 02 - broken.wav: unsupported format: not a container"
    );

    // The next status update replaces it.
    app.note("indexed: 1 added");
    assert_eq!(app.status(), "indexed: 1 added");
}

#[test]
fn a_newer_failure_replaces_the_previous_one() {
    let mut app = app_with_three_tracks();
    app.announce_failure(&TrackFailure {
        path: PathBuf::from("/library/first.flac"),
        message: "cannot read the file: no such file".to_owned(),
    });
    app.announce_failure(&TrackFailure {
        path: PathBuf::from("/library/second.flac"),
        message: "cannot decode the file: bad packet".to_owned(),
    });
    assert_eq!(
        app.status(),
        "velo: skipped second.flac: cannot decode the file: bad packet"
    );
}

#[test]
fn a_tick_without_a_player_changes_nothing() {
    let mut app = app_with_three_tracks();
    app.tick();
    assert_eq!(app.status(), "");
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

// ---- the library search (T7, S9) -----------------------------------------

#[test]
fn n_and_p_type_into_an_open_search_instead_of_skipping() {
    let temp = TempLibrary::new("search-np");
    temp.write_wav("Tide/2019 - Harbor/01 - Nightfall.wav");
    temp.index();
    let mut app = App::browse(Some(&temp.library));

    app.on_key(key(KeyCode::Char('/')));
    assert_eq!(app.on_key(key(KeyCode::Char('n'))), Action::None);
    assert_eq!(app.on_key(key(KeyCode::Char('p'))), Action::None);
    assert_eq!(app.search_query(), "np");
}

#[test]
fn slash_opens_a_search_and_typing_narrows_the_results() {
    let temp = TempLibrary::new("search-open");
    temp.write_wav("Poldoore/2012 - Lush Life/01 - Moochi.wav");
    temp.write_wav("Tide/2019 - Harbor/01 - Moonlight.wav");
    temp.write_wav("Tide/2019 - Harbor/02 - Nightfall.wav");
    temp.index();

    let mut app = App::browse(Some(&temp.library));
    assert!(!app.search_open());

    assert_eq!(app.on_key(key(KeyCode::Char('/'))), Action::None);
    assert!(app.search_open());
    assert_eq!(app.search_query(), "");
    assert_eq!(app.search_selected(), 0);
    // An empty query lists the whole library, in index order.
    assert_eq!(app.search_matches().len(), 3);

    // Like every result list, `j` and `k` move through it — they do not type
    // themselves into the query.
    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(app.search_selected(), 1);
    app.on_key(key(KeyCode::Char('k')));
    assert_eq!(app.search_selected(), 0);

    for character in "moon".chars() {
        assert_eq!(app.on_key(key(KeyCode::Char(character))), Action::None);
    }
    assert_eq!(app.search_query(), "moon");
    let titles: Vec<&str> = app
        .search_matches()
        .iter()
        .map(|track| track.title.as_str())
        .collect();
    assert_eq!(titles, ["Moonlight"]);

    // Backspace untypes; Esc is still `Esc` — it is handled elsewhere.
    app.on_key(key(KeyCode::Backspace));
    assert_eq!(app.search_query(), "moo");
}

#[test]
fn slash_without_a_library_is_not_a_search() {
    let mut app = app_with_three_tracks();
    assert_eq!(app.on_key(key(KeyCode::Char('/'))), Action::None);
    assert!(!app.search_open(), "argv files are not a library to search");
}

#[test]
fn esc_cancels_the_search_and_restores_the_previous_position_and_selection() {
    let temp = TempLibrary::new("search-esc");
    temp.write_wav("Poldoore/2012 - Lush Life/01 - Moochi.wav");
    temp.write_wav("Tide/2019 - Harbor/01 - Moonlight.wav");
    temp.index();

    let mut app = App::browse(Some(&temp.library));
    app.on_key(key(KeyCode::Char('j'))); // onto the second artist
    let previous_level = app.level();
    let previous_selected = app.selected();

    app.on_key(key(KeyCode::Char('/')));
    app.on_key(key(KeyCode::Char('m')));
    app.on_key(key(KeyCode::Char('j'))); // moves the search selection, not the library one
    assert_eq!(
        app.selected(),
        previous_selected,
        "searching does not browse"
    );

    assert_eq!(app.on_key(key(KeyCode::Esc)), Action::None);
    assert!(!app.search_open());
    assert_eq!(app.level(), previous_level);
    assert_eq!(app.selected(), previous_selected);
    assert_eq!(app.search_query(), "", "the query is gone with the search");
}

#[test]
fn enter_in_the_search_plays_from_the_selected_result() {
    let temp = TempLibrary::new("search-enter");
    temp.write_wav("Tide/2019 - Harbor/01 - Moonlight.wav");
    temp.write_wav("Poldoore/2012 - Lush Life/01 - Moonrise.wav");
    temp.write_wav("Tide/2019 - Harbor/02 - Nightfall.wav");
    temp.index();

    let mut app = App::browse(Some(&temp.library));
    app.on_key(key(KeyCode::Char('/')));
    for character in "moo".chars() {
        app.on_key(key(KeyCode::Char(character)));
    }
    assert_eq!(app.search_matches().len(), 2, "`moo` finds both");
    app.on_key(key(KeyCode::Char('j'))); // onto the second result

    // Not applied here: `apply` would open a sound device. The queue change
    // and the stream start happen in `apply`, from the same state.
    assert_eq!(app.on_key(key(KeyCode::Enter)), Action::Play { from: 1 });
}

#[test]
fn a_search_opened_from_the_queue_view_returns_to_the_queue_on_esc() {
    let temp = TempLibrary::new("search-queue");
    temp.write_wav("Tide/2019 - Harbor/01 - Dawn.wav");
    temp.write_wav("Tide/2019 - Harbor/02 - Later.wav");
    temp.index();

    let mut app = App::browse(Some(&temp.library));
    app.on_key(key(KeyCode::Enter));
    app.apply(Action::Descend); // the albums of Tide
    app.on_key(key(KeyCode::Char('a'))); // queues both tracks of Harbor
    app.on_key(key(KeyCode::Tab));
    assert_eq!(app.view(), View::Queue);
    app.on_key(key(KeyCode::Char('j')));
    let queue_selection = app.queue_selected();

    app.on_key(key(KeyCode::Char('/')));
    assert!(app.search_open());
    // Even `d`, `J` or `Tab` type into the query while it is open: nothing
    // edits the queue from inside a search.
    app.on_key(key(KeyCode::Char('d')));
    assert_eq!(app.search_query(), "d");

    assert_eq!(app.on_key(key(KeyCode::Esc)), Action::None);
    assert_eq!(app.view(), View::Queue);
    assert_eq!(
        app.queue_selected(),
        queue_selection,
        "the queue selection is untouched"
    );
    assert_eq!(app.queue().len(), 2, "the queue is untouched");
}

// ---- the library prompt (T5, S4) ----------------------------------------

/// A throwaway directory outside the worktree for the prompt tests.
fn prompt_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("velo-prompt-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn type_text(app: &mut App<'_>, text: &str) {
    for character in text.chars() {
        press(app, KeyCode::Char(character));
    }
}

#[test]
fn the_prompt_opens_when_no_library_is_configured_and_typing_edits_it() {
    let mut app = App::browse(None);
    assert!(!app.library_prompt_open());
    app.ask_for_library(None);
    assert!(app.library_prompt_open());
    assert_eq!(app.library_prompt_input(), "");

    // Every printable key types, `q` included: it does not quit.
    assert_eq!(app.on_key(key(KeyCode::Char('q'))), Action::None);
    type_text(&mut app, "/mus");
    press(&mut app, KeyCode::Backspace);
    assert_eq!(app.library_prompt_input(), "q/mu");
    assert!(app.library_prompt_open());
}

#[test]
fn a_path_that_is_not_a_directory_keeps_the_prompt_open_and_says_why() {
    let dir = prompt_dir("invalid");
    let config = dir.join("velo").join("config.toml");
    let mut app = App::browse(None);
    app.ask_for_library(Some(config.clone()));
    let missing = dir.join("no-such-folder");
    type_text(&mut app, &missing.display().to_string());

    assert_eq!(app.on_key(key(KeyCode::Enter)), Action::SetLibrary);
    app.apply(Action::SetLibrary);

    assert!(app.library_prompt_open());
    let reason = app.library_prompt_error().unwrap_or_default().to_owned();
    assert!(reason.contains("no-such-folder"), "reason: {reason}");
    assert!(reason.contains("not a directory"), "reason: {reason}");
    assert!(app.take_library_root().is_none());
    assert!(!config.exists(), "nothing is saved for a refused path");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_directory_is_accepted_saved_to_the_config_and_handed_to_indexing() {
    let dir = prompt_dir("valid");
    let music = dir.join("Music");
    fs::create_dir_all(&music).unwrap();
    let config = dir.join("velo").join("config.toml");
    velo::config::write_default_config(&config).unwrap();

    let mut app = App::browse(None);
    app.ask_for_library(Some(config.clone()));
    type_text(&mut app, &music.display().to_string());
    press(&mut app, KeyCode::Enter);

    assert!(!app.library_prompt_open());
    assert_eq!(app.take_library_root(), Some(music.clone()));
    assert_eq!(app.take_library_root(), None, "handed over once");
    let text = fs::read_to_string(&config).unwrap();
    let expected = velo::config::default_config_text().replace(
        "# library = \"~/Music\"",
        &format!("library = \"{}\"", music.display()),
    );
    assert_eq!(text, expected);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn esc_closes_the_prompt_without_saving_anything() {
    let dir = prompt_dir("esc");
    let config = dir.join("velo").join("config.toml");
    let mut app = App::browse(None);
    app.ask_for_library(Some(config.clone()));
    type_text(&mut app, "/tmp");

    assert_eq!(app.on_key(key(KeyCode::Esc)), Action::None);
    assert!(!app.library_prompt_open());
    assert!(app.take_library_root().is_none());
    assert!(!config.exists());
    // Back in the browser, `q` quits again.
    assert_eq!(app.on_key(key(KeyCode::Char('q'))), Action::Quit);
    let _ = fs::remove_dir_all(&dir);
}

// ---- the library views (S3) -----------------------------------------------

/// Two artists, three albums, five tracks; two carry genres, one several.
fn library_for_views(name: &str) -> TempLibrary {
    let temp = TempLibrary::new(name);
    let tag = |relative: &str, genre: Option<&str>| {
        let path = temp.write_wav(relative);
        if let Some(genre) = genre {
            tag_file(&path, |tag| tag.set_genre(genre.to_owned()));
        }
    };
    tag("Tide/2019 - Harbor/01 - Dawn.wav", Some("Lo-Fi / Jazz"));
    tag("Tide/2019 - Harbor/02 - Noon.wav", None);
    tag("Tide/2021 - Lantern/01 - Far.wav", Some("jazz"));
    tag("Baltazar/2001 - Views/01 - Rising.wav", None);
    tag("Baltazar/2001 - Views/02 - Setting.wav", None);
    temp.index();
    temp
}

/// Presses a key and applies what it asks for, as the event loop does —
/// except a play, which would open a sound device; that one is returned.
fn act(app: &mut App<'_>, code: KeyCode) -> Action {
    let action = app.on_key(key(code));
    if !matches!(action, Action::Play { .. } | Action::PlayQueue { .. }) {
        app.apply(action);
    }
    action
}

fn file_names(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn the_number_keys_switch_between_the_four_library_views() {
    let temp = library_for_views("views-switch");
    let mut app = App::browse(Some(&temp.library));
    assert_eq!(app.browse_by(), BrowseBy::Artists);

    assert_eq!(
        app.on_key(key(KeyCode::Char('2'))),
        Action::Browse(BrowseBy::Albums)
    );
    for (code, by, level) in [
        ('2', BrowseBy::Albums, Level::AllAlbums),
        ('3', BrowseBy::Tracks, Level::AllTracks),
        ('4', BrowseBy::Genres, Level::Genres),
        ('1', BrowseBy::Artists, Level::Artists),
    ] {
        act(&mut app, KeyCode::Char(code));
        assert_eq!(app.browse_by(), by);
        assert_eq!(app.level(), Some(level));
        assert_eq!(app.selected(), 0, "a view opens at its first row");
    }

    // From the queue view, a number brings the library back in that view.
    act(&mut app, KeyCode::Tab);
    act(&mut app, KeyCode::Char('3'));
    assert_eq!(app.view(), View::Library);
    assert_eq!(app.level(), Some(Level::AllTracks));
}

#[test]
fn the_albums_view_lists_every_album_and_enter_opens_its_tracks() {
    let temp = library_for_views("views-albums");
    let mut app = App::browse(Some(&temp.library));
    act(&mut app, KeyCode::Char('2'));

    let albums: Vec<(&str, &str, Option<i64>)> = app
        .albums()
        .iter()
        .map(|album| (album.title.as_str(), album.artist.as_str(), album.year))
        .collect();
    assert_eq!(
        albums,
        [
            ("Harbor", "Tide", Some(2019)),
            ("Lantern", "Tide", Some(2021)),
            ("Views", "Baltazar", Some(2001)),
        ]
    );

    act(&mut app, KeyCode::Char('G')); // Views
    assert_eq!(act(&mut app, KeyCode::Enter), Action::Descend);
    assert_eq!(
        app.level(),
        Some(Level::Tracks {
            artist: "Baltazar".to_owned(),
            album: "Views".to_owned()
        })
    );
    act(&mut app, KeyCode::Char('j'));
    assert_eq!(act(&mut app, KeyCode::Enter), Action::Play { from: 1 });
    assert_eq!(file_names(&app.playlist_from(1)), ["02 - Setting.wav"]);
    assert_eq!(
        file_names(&app.playlist_from(0)),
        ["01 - Rising.wav", "02 - Setting.wav"]
    );

    // Back one level lands on every album again, not on one artist's.
    assert_eq!(act(&mut app, KeyCode::Esc), Action::Ascend);
    assert_eq!(app.level(), Some(Level::AllAlbums));
    assert_eq!(app.albums().len(), 3);
}

#[test]
fn the_tracks_view_lists_every_track_and_enter_plays_from_it_through_the_list() {
    let temp = library_for_views("views-tracks");
    let mut app = App::browse(Some(&temp.library));
    act(&mut app, KeyCode::Char('3'));

    let titles: Vec<&str> = app.tracks().iter().map(|t| t.title.as_str()).collect();
    assert_eq!(titles, ["Rising", "Setting", "Dawn", "Noon", "Far"]);

    act(&mut app, KeyCode::Char('j'));
    assert_eq!(act(&mut app, KeyCode::Enter), Action::Play { from: 1 });
    assert_eq!(
        file_names(&app.playlist_from(1)),
        [
            "02 - Setting.wav",
            "01 - Dawn.wav",
            "02 - Noon.wav",
            "01 - Far.wav"
        ],
        "the stream runs on through the whole list, past the album"
    );

    // The top of a view absorbs `Esc`.
    act(&mut app, KeyCode::Esc);
    assert_eq!(app.level(), Some(Level::AllTracks));
}

#[test]
fn the_genres_view_lists_genres_and_enter_opens_then_plays_one() {
    let temp = library_for_views("views-genres");
    let mut app = App::browse(Some(&temp.library));
    act(&mut app, KeyCode::Char('4'));

    let genres: Vec<(&str, u64)> = app
        .genres()
        .iter()
        .map(|genre| (genre.name.as_str(), genre.tracks))
        .collect();
    assert_eq!(genres, [("Jazz", 2), ("Lo-Fi", 1), ("(no genre)", 3)]);

    assert_eq!(act(&mut app, KeyCode::Enter), Action::Descend);
    assert_eq!(
        app.level(),
        Some(Level::Genre {
            genre: "Jazz".to_owned()
        })
    );
    let titles: Vec<&str> = app.tracks().iter().map(|t| t.title.as_str()).collect();
    assert_eq!(titles, ["Dawn", "Far"]);
    assert_eq!(act(&mut app, KeyCode::Enter), Action::Play { from: 0 });
    assert_eq!(
        file_names(&app.playlist_from(0)),
        ["01 - Dawn.wav", "01 - Far.wav"],
        "the genre's list, not the album, follows the track"
    );

    assert_eq!(act(&mut app, KeyCode::Esc), Action::Ascend);
    assert_eq!(app.level(), Some(Level::Genres));
    act(&mut app, KeyCode::Char('G')); // (no genre)
    act(&mut app, KeyCode::Enter);
    let titles: Vec<&str> = app.tracks().iter().map(|t| t.title.as_str()).collect();
    assert_eq!(titles, ["Rising", "Setting", "Noon"]);
}

#[test]
fn a_appends_from_every_view() {
    let temp = library_for_views("views-append");
    let mut app = App::browse(Some(&temp.library));

    act(&mut app, KeyCode::Char('2')); // Harbor first
    act(&mut app, KeyCode::Char('a'));
    assert_eq!(
        file_names(app.queue()),
        ["01 - Dawn.wav", "02 - Noon.wav"],
        "an album appends its tracks"
    );

    act(&mut app, KeyCode::Char('4')); // Jazz first
    act(&mut app, KeyCode::Char('a'));
    assert_eq!(app.queue().len(), 4, "a genre appends its tracks");

    act(&mut app, KeyCode::Char('3')); // Rising first
    act(&mut app, KeyCode::Char('a'));
    assert_eq!(
        app.queue().last().unwrap().file_name().unwrap(),
        "01 - Rising.wav"
    );
}

#[test]
fn the_artists_view_still_drills_down_artist_album_track_as_before() {
    let temp = library_for_views("views-artists");
    let mut app = App::browse(Some(&temp.library));
    // A trip through another view changes nothing about the artists.
    act(&mut app, KeyCode::Char('4'));
    act(&mut app, KeyCode::Char('1'));

    let artists: Vec<&str> = app.artists().iter().map(|a| a.name.as_str()).collect();
    assert_eq!(artists, ["Baltazar", "Tide"]);
    act(&mut app, KeyCode::Char('j')); // Tide
    assert_eq!(act(&mut app, KeyCode::Enter), Action::Descend);
    assert_eq!(
        app.level(),
        Some(Level::Albums {
            artist: "Tide".to_owned()
        })
    );
    assert_eq!(app.selected(), 0);
    assert_eq!(act(&mut app, KeyCode::Enter), Action::Descend); // Harbor
    assert_eq!(act(&mut app, KeyCode::Enter), Action::Play { from: 0 });
    assert_eq!(
        file_names(&app.playlist_from(0)),
        ["01 - Dawn.wav", "02 - Noon.wav"],
        "the album, and only the album, from the selection"
    );
    assert_eq!(act(&mut app, KeyCode::Esc), Action::Ascend);
    assert_eq!(
        app.level(),
        Some(Level::Albums {
            artist: "Tide".to_owned()
        })
    );
    assert_eq!(
        app.selected(),
        0,
        "ascending selects the first row, as before"
    );
    act(&mut app, KeyCode::Esc);
    assert_eq!(app.level(), Some(Level::Artists));
}

#[test]
fn number_keys_type_into_an_open_search_and_the_library_prompt() {
    let temp = library_for_views("views-search");
    let mut app = App::browse(Some(&temp.library));
    act(&mut app, KeyCode::Char('/'));
    for code in ['1', '2', '3', '4'] {
        assert_eq!(act(&mut app, KeyCode::Char(code)), Action::None);
    }
    assert_eq!(app.search_query(), "1234");
    assert_eq!(app.browse_by(), BrowseBy::Artists, "the view did not move");

    let mut app = App::browse(None);
    app.ask_for_library(None);
    for code in ['2', '4'] {
        assert_eq!(act(&mut app, KeyCode::Char(code)), Action::None);
    }
    assert_eq!(app.library_prompt_input(), "24");
    assert_eq!(app.browse_by(), BrowseBy::Artists);
}

#[test]
fn number_keys_do_nothing_over_files_given_on_the_command_line() {
    let mut app = app_with_three_tracks();
    for code in ['1', '2', '3', '4'] {
        assert_eq!(app.on_key(key(KeyCode::Char(code))), Action::None);
    }
    assert_eq!(app.level(), None);
}

/// Reported by reading, confirmed by the user: Enter on the third file given
/// on the command line started the stream at the first one. The stream now
/// starts at the file Enter was pressed on, as an album's does.
#[test]
fn enter_on_a_command_line_file_plays_from_that_file_onwards() {
    let app = app_with_three_tracks();

    assert_eq!(
        app.playlist_from(2),
        [PathBuf::from("/library/artist/three.flac")]
    );
    assert_eq!(
        app.playlist_from(1),
        [
            PathBuf::from("/library/artist/two.flac"),
            PathBuf::from("/library/artist/three.flac"),
        ]
    );
}
