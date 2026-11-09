//! The keymap layer, tested without a terminal: every rule of
//! `App::on_key` observed through the returned [`Action`].

use std::fs;
use std::path::{Path, PathBuf};

use clap::Parser as _;
use lofty::config::WriteOptions;
use lofty::prelude::*;
use lofty::tag::{Tag, TagType};
use velo::audio::output::{Entry, TrackFailure};
use velo::library::Library;
use velo::ui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use velo::ui::indexing::Indexer;
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

/// `j` walks down and wraps from the last row to the first (issue #36):
/// the lists that are for going through wrap at their ends.
#[test]
fn j_moves_down_and_wraps_from_the_last_row_to_the_first() {
    let mut app = app_with_three_tracks();
    assert_eq!(app.selected(), 0);

    assert_eq!(app.on_key(key(KeyCode::Char('j'))), Action::None);
    assert_eq!(app.selected(), 1);
    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(app.selected(), 2);

    // From the last row `j` wraps to the first.
    assert_eq!(app.on_key(key(KeyCode::Char('j'))), Action::None);
    assert_eq!(app.selected(), 0);

    // `Down` is the same movement.
    let mut app = app_with_three_tracks();
    app.on_key(key(KeyCode::Down));
    assert_eq!(app.selected(), 1);
}

/// `k` walks up and wraps from the first row to the last (issue #36).
#[test]
fn k_moves_up_and_wraps_from_the_first_row_to_the_last() {
    let mut app = app_with_three_tracks();
    app.on_key(key(KeyCode::Char('j')));
    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(app.selected(), 2);

    assert_eq!(app.on_key(key(KeyCode::Char('k'))), Action::None);
    assert_eq!(app.selected(), 1);
    app.on_key(key(KeyCode::Char('k')));
    assert_eq!(app.selected(), 0);

    // From the first row `k` wraps to the last.
    assert_eq!(app.on_key(key(KeyCode::Char('k'))), Action::None);
    assert_eq!(app.selected(), 2);

    // `Up` is the same movement.
    let mut app = app_with_three_tracks();
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Up));
    assert_eq!(app.selected(), 0);
}

/// A library list wraps at both ends too (issue #36): the artists of a
/// browsed index behave like every navigation list.
#[test]
fn a_library_list_wraps_at_both_ends() {
    let temp = TempLibrary::new("wrap-library");
    temp.write_wav("Tide/2019 - Harbor/01 - Dawn.wav");
    temp.write_wav("Nujabes/2015 - Modal Soul/01 - Luv.wav");
    temp.index();
    let mut app = App::browse(Some(&temp.library));
    assert_eq!(app.artists().len(), 2);

    app.on_key(key(KeyCode::Char('G')));
    assert_eq!(app.selected(), 1, "G lands on the last artist");
    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(
        app.selected(),
        0,
        "j wraps from the last artist to the first"
    );
    app.on_key(key(KeyCode::Char('k')));
    assert_eq!(
        app.selected(),
        1,
        "k wraps from the first artist to the last"
    );
}

/// The queue never wraps (issue #36): `j` at the last entry and `k` at the
/// first both stay put, because its entries get removed and reordered.
#[test]
fn the_queue_still_clamps_at_both_ends() {
    let mut app = app_with_a_queue_of_three();
    app.on_key(key(KeyCode::Tab));
    app.on_key(key(KeyCode::Char('G')));
    assert_eq!(app.queue_selected(), 2);

    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(app.queue_selected(), 2, "j at the last entry stays put");
    app.on_key(key(KeyCode::Char('g')));
    assert_eq!(app.queue_selected(), 0);
    app.on_key(key(KeyCode::Char('k')));
    assert_eq!(app.queue_selected(), 0, "k at the first entry stays put");
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
        KeyCode::Char('['),
        KeyCode::Char(']'),
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
fn volume_moves_one_point_at_a_time_on_the_fine_keys() {
    let mut app = app_with_three_tracks();
    assert_eq!(app.on_key(key(KeyCode::Char('-'))), Action::SetVolume(95));

    // The fine gear is one point per press (issue #16).
    assert_eq!(app.on_key(key(KeyCode::Char(']'))), Action::SetVolume(96));
    assert_eq!(app.on_key(key(KeyCode::Char('['))), Action::SetVolume(95));

    // The plain keys keep the five-point step they always had.
    assert_eq!(app.on_key(key(KeyCode::Char('+'))), Action::SetVolume(100));
    assert_eq!(app.on_key(key(KeyCode::Char('-'))), Action::SetVolume(95));
}

#[test]
fn the_fine_volume_keys_clamp_at_the_bounds() {
    // At 100 from the start, `]` clamps instead of overflowing by one.
    let mut app = app_with_three_tracks();
    assert_eq!(app.on_key(key(KeyCode::Char(']'))), Action::SetVolume(100));
    assert_eq!(app.volume(), 100);

    for _ in 0..100 {
        app.on_key(key(KeyCode::Char('[')));
    }
    // At 0, `[` clamps instead of underflowing.
    assert_eq!(app.on_key(key(KeyCode::Char('['))), Action::SetVolume(0));
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

/// S13 (issue #25): the keymap never collapses `ctrl+r` into `r` — on the
/// queue it does nothing, and the repeat it left untouched advances by one
/// step only.
#[test]
fn ctrl_r_does_not_cycle_repeat_on_the_queue() {
    use velo::audio::output::Repeat;
    let mut app = app_with_three_tracks();
    assert_eq!(
        app.on_key(key(KeyCode::Char('r'))),
        Action::SetRepeat(Repeat::All)
    );

    assert_eq!(app.on_key(key(KeyCode::Tab)), Action::None); // onto the queue
    let ctrl_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
    assert_eq!(
        app.on_key(ctrl_r),
        Action::None,
        "ctrl+r does nothing on the queue"
    );

    assert_eq!(app.on_key(key(KeyCode::Tab)), Action::None);
    assert_eq!(
        app.on_key(key(KeyCode::Char('r'))),
        Action::SetRepeat(Repeat::One),
        "the repeat the ctrl+r left alone advances one step, not two"
    );
}

/// S13 (issue #25): the radio's own `ctrl+r` keeps reaching the radio when
/// the radio view has focus — no repeat action comes back — and `r` alone
/// still cycles repeat while the radio is open.
#[test]
fn ctrl_r_in_the_radio_view_is_the_radio_s_and_r_alone_still_cycles_repeat() {
    use velo::audio::output::Repeat;
    let mut app = App::browse(None);
    app.set_library_root(Some(PathBuf::from("/music"))); // a configured library opens no prompt
    app.open_sources();
    press(&mut app, KeyCode::Char('j')); // onto Radio
    press(&mut app, KeyCode::Enter);
    assert!(app.radio_open());

    let ctrl_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
    assert_eq!(
        app.on_key(ctrl_r),
        Action::None,
        "the radio keeps its ctrl+r; nothing cycles the repeat"
    );
    assert_eq!(
        app.on_key(key(KeyCode::Char('r'))),
        Action::SetRepeat(Repeat::All),
        "`r` alone still cycles repeat while the radio is open"
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
    assert_eq!(queue_paths(app.queue()), paths);
    (app, paths)
}

#[test]
fn s_shuffles_the_queue_and_s_again_puts_it_back_in_order() {
    let (mut app, paths) = five_queued();

    let action = app.on_key(key(KeyCode::Char('s')));
    assert_eq!(action, Action::ToggleShuffle);
    app.apply(action);
    let mut shuffled = queue_paths(app.queue());
    shuffled.sort();
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(shuffled, sorted, "the same tracks, nothing lost or doubled");

    let action = app.on_key(key(KeyCode::Char('s')));
    app.apply(action);
    assert_eq!(
        queue_paths(app.queue()),
        paths,
        "back in the order they came"
    );
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
    assert_eq!(queue_paths(app.queue()), paths);
}

// ---- reopening the library prompt (library-refresh T1) -------------------------

#[test]
fn shift_l_opens_the_library_prompt_again_after_esc() {
    let mut app = App::browse(None);
    app.set_config_file(Some(PathBuf::from("/nowhere/velo/config.toml")));
    assert!(!app.library_prompt_open());

    press(&mut app, KeyCode::Char('L'));
    assert!(app.library_prompt_open());
    assert_eq!(app.library_prompt_input(), "~/");

    press(&mut app, KeyCode::Esc);
    assert!(!app.library_prompt_open());
    press(&mut app, KeyCode::Char('L'));
    assert!(app.library_prompt_open(), "it opens again whenever asked");
}

#[test]
fn shift_l_types_into_the_search_and_does_nothing_over_files() {
    let mut files = app_with_three_tracks();
    press(&mut files, KeyCode::Char('L'));
    assert!(!files.library_prompt_open(), "files have no library to set");

    let temp = TempLibrary::new("shift-l-search");
    temp.write_wav("Tide/2019 - Harbor/01 - Dawn.wav");
    temp.index();
    let mut app = App::browse(Some(&temp.library));
    press(&mut app, KeyCode::Char('/'));
    press(&mut app, KeyCode::Char('L'));
    assert_eq!(app.search_query(), "L");
    assert!(!app.library_prompt_open());
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
/// step down, append, step down, append — the library selection stays on the
/// last row, so the tests below keep a known library selection.
fn app_with_a_queue_of_three() -> App<'static> {
    let mut app = app_with_three_tracks();
    app.on_key(key(KeyCode::Char('a')));
    app.on_key(key(KeyCode::Char('j')));
    app.on_key(key(KeyCode::Char('a')));
    app.on_key(key(KeyCode::Char('j')));
    app.on_key(key(KeyCode::Char('a')));
    app
}

#[test]
fn a_appends_the_selected_track_to_the_queue_without_starting_a_stream() {
    let mut app = app_with_three_tracks();
    app.on_key(key(KeyCode::Char('j'))); // onto two.flac
    assert_eq!(app.queue(), [] as [Entry; 0]);

    // `a` answers with no action: appending never interrupts what is playing,
    // and here it does not even start a stream.
    assert_eq!(app.on_key(key(KeyCode::Char('a'))), Action::None);
    assert_eq!(
        app.queue(),
        [Entry::File(PathBuf::from("/library/artist/two.flac"))]
    );
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
        .map(|entry| entry.label().into_owned())
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
    assert_eq!(app.queue(), [] as [Entry; 0], "nothing was removed");
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
        .map(|entry| entry.label().into_owned())
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
        .map(|entry| entry.label().into_owned())
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
        .map(|entry| entry.label().into_owned())
        .collect();
    assert_eq!(dropped, ["one.flac", "two.flac", "three.flac"]);
    assert_eq!(app.queue_selected(), 2);
}

/// Issue #17: the playing entry is refused whatever it is — a station that
/// plays has the same protection a track that plays has always had, and
/// nothing moves across it. The fixture stands in for the player, as it does
/// for the transport tests: production sets `playing` and the player
/// together, so the queue edits read the station as the playing entry.
#[test]
fn d_j_and_k_refuse_on_the_playing_station_as_on_a_track() {
    let labels = |app: &App| -> Vec<String> {
        app.queue()
            .iter()
            .map(|entry| entry.label().into_owned())
            .collect()
    };
    let mut app = App::new(vec![PathBuf::from("/library/artist/one.flac")]);
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
    press(&mut app, KeyCode::Tab); // into the queue, on the station

    // `d` refuses the playing station…
    assert_eq!(app.on_key(key(KeyCode::Char('d'))), Action::None);
    assert_eq!(
        app.status(),
        "velo: cannot remove the track that is playing"
    );
    assert_eq!(labels(&app), ["Radio Nova", "one.flac"]);

    // …and `J` and `K` refuse to move it.
    assert_eq!(app.on_key(key(KeyCode::Char('J'))), Action::None);
    assert_eq!(
        app.status(),
        "velo: cannot remove the track that is playing"
    );
    assert_eq!(app.on_key(key(KeyCode::Char('K'))), Action::None);
    assert_eq!(
        app.status(),
        "velo: cannot remove the track that is playing"
    );
    assert_eq!(labels(&app), ["Radio Nova", "one.flac"]);
    assert_eq!(
        app.queue_selected(),
        0,
        "the selection stays on the station"
    );

    // Nothing moves across the playing station either…
    app.on_key(key(KeyCode::Char('j'))); // onto one.flac, the entry after it
    assert_eq!(app.on_key(key(KeyCode::Char('K'))), Action::None);
    assert_eq!(
        app.status(),
        "velo: cannot move past the track that is playing"
    );
    assert_eq!(labels(&app), ["Radio Nova", "one.flac"]);

    // …while the entries after it stay editable, station or not.
    app.on_key(key(KeyCode::Char('d')));
    assert_eq!(app.status(), "velo: removed one.flac");
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

/// S8 (issue #23): the round trip works from inside the library too — `tab`
/// to the queue, `tab` back, and the library is at the same level it was
/// left at. The level itself is the proof of the round trip.
#[test]
fn tab_from_inside_the_library_returns_to_the_same_level() {
    let temp = TempLibrary::new("tab-round-trip");
    temp.write_wav("Tide/2019 - Harbor/01 - Dawn.wav");
    temp.index();

    let mut app = App::browse(Some(&temp.library));
    assert_eq!(app.on_key(key(KeyCode::Enter)), Action::Descend);
    app.apply(Action::Descend); // the albums of Tide

    app.on_key(key(KeyCode::Tab));
    assert_eq!(app.view(), View::Queue);

    app.on_key(key(KeyCode::Tab));
    assert_eq!(app.view(), View::Library);
    assert_eq!(
        app.level(),
        Some(Level::Albums {
            artist: "Tide".to_owned()
        }),
        "the library is at the level it was left at"
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
        label: "02 - broken.wav".to_owned(),
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
        label: "first.flac".to_owned(),
        message: "cannot read the file: no such file".to_owned(),
    });
    app.announce_failure(&TrackFailure {
        label: "second.flac".to_owned(),
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

/// A group- or world-readable `subsonic.key` must be said where the user is
/// looking — the status line, once, when the interface starts — not only
/// when a download happens to fail (the T12c debt).
#[test]
fn a_readable_subsonic_key_file_warns_once_in_the_status_line() {
    let mut app = App::browse(None);
    app.set_subsonic_warnings(&[
        "the subsonic key file /home/demo/.config/velo/subsonic.key is readable by group or \
         others (mode 0644) — restrict it to you alone: chmod 600"
            .to_owned(),
    ]);

    app.tick();
    assert!(
        app.status().contains(".config/velo/subsonic.key"),
        "the warning names the path: {}",
        app.status()
    );
    assert_eq!(
        app.status().matches("subsonic.key").count(),
        1,
        "one message, not one per warning word"
    );

    // The warning is said once: ticks that bring nothing new must not repeat
    // it, the same rule a skipped track's message obeys.
    app.tick();
    app.tick();
    assert_eq!(
        app.status().matches("subsonic.key").count(),
        1,
        "the warning must not repeat on later ticks"
    );
}

/// A private (0600) key file resolves with no warning, so the status line
/// stays empty — silence is the sign that nothing is wrong.
#[test]
fn a_private_key_file_produces_no_status_warning() {
    let mut app = App::browse(None);
    app.set_subsonic_warnings(&[]);

    app.tick();
    assert_eq!(app.status(), "", "no warning, no message");
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

    // …and a third walks back one level more: to the sources screen (S6),
    // the way out of the library is the way in, and the level it keeps.
    assert_eq!(app.on_key(key(KeyCode::Esc)), Action::Ascend);
    app.apply(Action::Ascend);
    assert_eq!(app.view(), View::Sources);
    assert_eq!(app.level(), Some(Level::Artists));

    // On the sources screen `Backspace` is inert (S6): the top of all is
    // already where the view stands.
    assert_eq!(app.on_key(key(KeyCode::Backspace)), Action::None);
    app.apply(Action::None);
    assert_eq!(app.view(), View::Sources);
    assert_eq!(app.level(), Some(Level::Artists));
}

/// `Esc` over the flat command-line list was always absorbed, and still is
/// (issue #3): the files given have no sources screen to walk back to.
#[test]
fn esc_over_the_command_line_files_is_absorbed_and_stays_on_the_list() {
    let mut app = app_with_three_tracks();

    assert_eq!(app.on_key(key(KeyCode::Esc)), Action::None);
    app.apply(Action::None);
    assert_eq!(app.on_key(key(KeyCode::Backspace)), Action::None);
    app.apply(Action::None);
    assert_eq!(app.view(), View::Library);
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
fn the_prompt_opens_on_home_and_its_prefix_cannot_be_deleted() {
    let mut app = App::browse(None);
    assert!(!app.library_prompt_open());
    app.ask_for_library_under(None, Some(prompt_dir("prefix")));
    assert!(app.library_prompt_open());
    assert_eq!(app.library_prompt_input(), "~/", "it starts at home");

    // Every printable key types, `q` included: it does not quit.
    assert_eq!(app.on_key(key(KeyCode::Char('q'))), Action::None);
    type_text(&mut app, "mus");
    press(&mut app, KeyCode::Backspace);
    assert_eq!(app.library_prompt_input(), "~/qmu");
    for _ in 0..10 {
        press(&mut app, KeyCode::Backspace);
    }
    assert_eq!(app.library_prompt_input(), "~/", "`~/` stays put");
    assert!(app.library_prompt_open());
}

#[test]
fn tab_completes_folders_the_way_a_shell_does() {
    let home = prompt_dir("tab");
    fs::create_dir_all(home.join("Music/Library")).unwrap();
    fs::create_dir_all(home.join("Mixes")).unwrap();
    fs::create_dir_all(home.join(".Mhidden")).unwrap();
    fs::write(home.join("Mnotes.txt"), "a file, never a candidate").unwrap();
    let mut app = App::browse(None);
    app.ask_for_library_under(None, Some(home.clone()));

    // Two folders start with M: nothing more in common, both are offered.
    type_text(&mut app, "M");
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.library_prompt_input(), "~/M");
    assert_eq!(app.library_prompt_candidates(), ["Mixes", "Music"]);

    // One left: completed, with the slash a shell adds to a folder.
    type_text(&mut app, "u");
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.library_prompt_input(), "~/Music/");
    assert_eq!(app.library_prompt_candidates(), Vec::<String>::new());

    // And inside it.
    type_text(&mut app, "L");
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.library_prompt_input(), "~/Music/Library/");
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_path_that_is_not_a_directory_keeps_the_prompt_open_and_says_why() {
    let dir = prompt_dir("invalid");
    let config = dir.join("velo").join("config.toml");
    let mut app = App::browse(None);
    app.ask_for_library_under(Some(config.clone()), Some(dir.clone()));
    type_text(&mut app, "no-such-folder");

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
    app.ask_for_library_under(Some(config.clone()), Some(dir.clone()));
    type_text(&mut app, "Music");
    press(&mut app, KeyCode::Enter);

    assert!(!app.library_prompt_open());
    assert_eq!(app.take_library_root(), Some(music.clone()));
    assert_eq!(app.take_library_root(), None, "handed over once");
    let text = fs::read_to_string(&config).unwrap();
    let expected = velo::config::default_config_text()
        .replace("# library = \"~/Music\"", "library = \"~/Music\"");
    assert_eq!(text, expected);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn esc_closes_the_prompt_without_saving_anything() {
    let dir = prompt_dir("esc");
    let config = dir.join("velo").join("config.toml");
    let mut app = App::browse(None);
    app.ask_for_library_under(Some(config.clone()), Some(dir.clone()));
    type_text(&mut app, "tmp");

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

/// The queue's entries as the paths they stand for, so the assertions that
/// read the queue keep talking about files, the way the player resolves them.
fn queue_paths(queue: &[Entry]) -> Vec<PathBuf> {
    queue
        .iter()
        .map(|entry| entry.as_path().expect("queue holds files").to_path_buf())
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
        file_names(&queue_paths(app.queue())),
        ["01 - Dawn.wav", "02 - Noon.wav"],
        "an album appends its tracks"
    );

    act(&mut app, KeyCode::Char('4')); // Jazz first
    act(&mut app, KeyCode::Char('a'));
    assert_eq!(app.queue().len(), 4, "a genre appends its tracks");

    act(&mut app, KeyCode::Char('3')); // Rising first
    act(&mut app, KeyCode::Char('a'));
    assert_eq!(app.queue().last().unwrap().label(), "01 - Rising.wav");
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
    assert_eq!(app.library_prompt_input(), "~/24");
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

// ---- refreshing the index (library-refresh T3, S3) --------------------------

/// Runs one background index of `temp` the way the shell does: on its own
/// connection to the same index file, while `App` keeps the UI connection.
fn index_in_background(temp: &TempLibrary) -> Result<velo::library::IndexReport, String> {
    let database = temp.root.join("index.db");
    Indexer::spawn(temp.root.clone(), move || Library::open_at(&database)).wait()
}

#[test]
fn u_asks_the_shell_to_refresh_the_configured_library() {
    let temp = TempLibrary::new("u-refresh");
    let mut app = App::browse(Some(&temp.library));
    app.set_library_root(Some(temp.root.clone()));

    assert_eq!(app.on_key(key(KeyCode::Char('u'))), Action::Reindex);
    app.apply(Action::Reindex);
    assert_eq!(app.take_library_root(), Some(temp.root.clone()));
}

#[test]
fn u_while_an_index_runs_starts_no_second_one_and_says_so() {
    let temp = TempLibrary::new("u-busy");
    let mut app = App::browse(Some(&temp.library));
    app.set_library_root(Some(temp.root.clone()));
    app.index_started(&temp.root);
    assert_eq!(app.indexing(), Some(temp.root.as_path()));

    assert_eq!(app.on_key(key(KeyCode::Char('u'))), Action::None);
    assert!(
        app.status().contains("already indexing"),
        "{}",
        app.status()
    );
    assert_eq!(app.take_library_root(), None);
}

#[test]
fn u_without_a_library_says_how_to_set_one() {
    let mut app = App::browse(None);
    assert_eq!(app.on_key(key(KeyCode::Char('u'))), Action::None);
    assert!(app.status().contains("no library set"), "{}", app.status());
    assert!(app.status().contains('L'), "{}", app.status());
    assert_eq!(app.take_library_root(), None);
}

#[test]
fn a_library_chosen_in_the_prompt_is_the_one_u_refreshes() {
    let temp = TempLibrary::new("u-after-prompt");
    let home = temp.root.clone();
    fs::create_dir_all(home.join("Music")).unwrap();
    let mut app = App::browse(Some(&temp.library));
    app.ask_for_library_under(None, Some(home.clone()));
    for character in "Music".chars() {
        press(&mut app, KeyCode::Char(character));
    }
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.take_library_root(), Some(home.join("Music")));

    assert_eq!(app.on_key(key(KeyCode::Char('u'))), Action::Reindex);
    app.apply(Action::Reindex);
    assert_eq!(app.take_library_root(), Some(home.join("Music")));
}

#[test]
fn the_launch_index_builds_a_configured_library_on_its_own_connection() {
    let temp = TempLibrary::new("launch-index");
    temp.write_wav("Tide/2019 - Harbor/01 - Dawn.wav");
    temp.write_wav("Tide/2019 - Harbor/02 - Noon.wav");
    let mut app = App::browse(Some(&temp.library));
    app.set_library_root(Some(temp.root.clone()));
    assert!(app.artists().is_empty(), "nothing indexed yet");

    app.index_started(&temp.root);
    app.index_finished(index_in_background(&temp));

    assert_eq!(app.indexing(), None);
    assert_eq!(
        app.status(),
        "indexed: 2 added, 0 updated, 0 unchanged, 0 removed"
    );
    assert_eq!(app.artists().len(), 1, "the lists reload when it ends");
}

#[test]
fn finishing_an_index_reloads_the_lists_and_keeps_the_view_level_and_selection() {
    let temp = TempLibrary::new("finish-reload");
    temp.write_wav("Tide/2019 - Harbor/01 - Dawn.wav");
    temp.write_wav("Tide/2019 - Harbor/02 - Noon.wav");
    temp.index();
    let mut app = App::browse(Some(&temp.library));
    press(&mut app, KeyCode::Char('3'));
    press(&mut app, KeyCode::Char('j'));
    let selected = app.tracks()[app.selected()].path.clone();

    temp.write_wav("Tide/2019 - Harbor/00 - Before.wav");
    app.index_started(&temp.root);
    app.index_finished(index_in_background(&temp));

    assert_eq!(app.level(), Some(Level::AllTracks));
    assert_eq!(app.browse_by(), BrowseBy::Tracks);
    assert_eq!(app.tracks().len(), 3, "the new file is listed");
    assert_eq!(
        app.tracks()[app.selected()].path,
        selected,
        "the same track stays selected"
    );
    assert!(app.status().contains("1 added"), "{}", app.status());
}

#[test]
fn finishing_an_index_reloads_an_open_album_and_the_albums_above_it() {
    let temp = TempLibrary::new("finish-album");
    temp.write_wav("Tide/2019 - Harbor/01 - Dawn.wav");
    temp.index();
    let mut app = App::browse(Some(&temp.library));
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Enter);
    let album = Level::Tracks {
        artist: "Tide".to_owned(),
        album: "Harbor".to_owned(),
    };
    assert_eq!(app.level(), Some(album.clone()));

    temp.write_wav("Tide/2019 - Harbor/02 - Noon.wav");
    temp.write_wav("Tide/2021 - Lantern/01 - Far.wav");
    app.index_started(&temp.root);
    app.index_finished(index_in_background(&temp));

    assert_eq!(app.level(), Some(album));
    assert_eq!(app.tracks().len(), 2);
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.albums().len(), 2, "the parent list is fresh too");
}

#[test]
fn an_album_that_disappeared_returns_to_the_top_of_its_view() {
    let temp = TempLibrary::new("finish-gone");
    let dawn = temp.write_wav("Tide/2019 - Harbor/01 - Dawn.wav");
    temp.write_wav("Sea/2020 - Shore/01 - Low.wav");
    temp.index();
    let mut app = App::browse(Some(&temp.library));
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Enter);
    assert!(matches!(app.level(), Some(Level::Tracks { .. })));

    fs::remove_file(dawn).unwrap();
    app.index_started(&temp.root);
    app.index_finished(index_in_background(&temp));

    assert_eq!(app.level(), Some(Level::Artists));
    assert_eq!(app.artists().len(), 1);
}

/// Found by the T3 verifier: falling back to the top of the view also
/// switched the tab, so a user on the queue was moved off it by an index
/// they did not touch.
#[test]
fn an_index_that_empties_the_open_album_leaves_the_queue_tab_where_it_was() {
    let temp = TempLibrary::new("finish-queue-tab");
    let dawn = temp.write_wav("Tide/2019 - Harbor/01 - Dawn.wav");
    temp.write_wav("Sea/2020 - Shore/01 - Low.wav");
    temp.index();
    let mut app = App::browse(Some(&temp.library));
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.view(), View::Queue);

    fs::remove_file(dawn).unwrap();
    app.index_started(&temp.root);
    app.index_finished(index_in_background(&temp));

    assert_eq!(app.view(), View::Queue, "the tab stays");
    assert_eq!(app.level(), Some(Level::Artists), "the library moved up");
}

#[test]
fn a_failed_index_reports_its_error_and_ends() {
    let temp = TempLibrary::new("finish-error");
    let mut app = App::browse(Some(&temp.library));
    app.index_started(&temp.root);
    app.index_finished(Err("disk on fire".to_owned()));

    assert_eq!(app.indexing(), None);
    assert_eq!(app.status(), "velo: indexing failed: disk on fire");

    // An index that cannot even be opened reaches the same line.
    let outcome = Indexer::spawn(temp.root.clone(), || {
        Library::open_at(Path::new("/dev/null/velo/index.db"))
    })
    .wait();
    let error = outcome.expect_err("/dev/null holds no directory");
    assert!(error.contains("cannot open the index"), "{error}");
}

#[test]
fn an_open_help_owns_the_keys_and_esc_clears_its_filter_before_closing() {
    let mut app = app_with_three_tracks();
    press(&mut app, KeyCode::Char('?'));

    // j/k/G scroll the help, never the list underneath.
    for code in [
        KeyCode::Char('j'),
        KeyCode::Down,
        KeyCode::Char('G'),
        KeyCode::Char('k'),
    ] {
        assert_eq!(app.on_key(key(code)), Action::None);
    }
    assert_eq!(app.selected(), 0, "the library selection never moved");

    // A filter: printable keys type into it, `q` included.
    press(&mut app, KeyCode::Char('/'));
    assert_eq!(app.on_key(key(KeyCode::Char('q'))), Action::None);
    assert!(app.help_open());
    press(&mut app, KeyCode::Enter);

    // The first `Esc` clears the filter, the second closes the help.
    press(&mut app, KeyCode::Esc);
    assert!(app.help_open(), "esc cleared the filter first");
    press(&mut app, KeyCode::Esc);
    assert!(!app.help_open(), "esc with no filter closes the help");
    assert_eq!(app.selected(), 0);

    // `?` closes it too, filter or not.
    press(&mut app, KeyCode::Char('?'));
    press(&mut app, KeyCode::Char('/'));
    press(&mut app, KeyCode::Char('x'));
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('?'));
    assert!(!app.help_open());

    // Closed again, j moves the list as before.
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.selected(), 1);
}

#[test]
fn an_open_help_holds_back_q_and_space_until_it_closes() {
    let mut app = app_with_three_tracks();
    press(&mut app, KeyCode::Char('?'));
    assert!(app.help_open());

    // `q` quits and `space` pauses, but the card owns both while it is open.
    for code in [KeyCode::Char('q'), KeyCode::Char(' ')] {
        assert_eq!(
            app.on_key(key(code)),
            Action::None,
            "{code:?} reached the list under the help"
        );
        assert!(app.help_open(), "the help stays open");
    }
    assert_eq!(app.selected(), 0, "the library selection never moved");

    // Closed again, both keys mean what they meant before.
    press(&mut app, KeyCode::Char('?'));
    assert!(!app.help_open());
    assert_eq!(app.on_key(key(KeyCode::Char('q'))), Action::Quit);
    assert_eq!(app.on_key(key(KeyCode::Char(' '))), Action::TogglePause);
}

// ---- the minor advisories (help-and-advisories T2, S2) ----------------------

#[test]
fn an_open_search_reads_the_new_pool_when_an_index_finishes_and_keeps_its_selection() {
    let temp = TempLibrary::new("search-refresh");
    temp.write_wav("Tide/2019 - Harbor/01 - Dawn.wav");
    temp.write_wav("Tide/2019 - Harbor/02 - Noon.wav");
    temp.index();
    let mut app = App::browse(Some(&temp.library));
    press(&mut app, KeyCode::Char('/'));
    type_text(&mut app, "tide");
    press(&mut app, KeyCode::Char('j'));
    let picked = app.search_matches()[app.search_selected()].path.clone();
    assert!(picked.ends_with("02 - Noon.wav"), "{}", picked.display());

    temp.write_wav("Tide/2019 - Harbor/00 - Before.wav");
    app.index_started(&temp.root);
    app.index_finished(index_in_background(&temp));

    assert!(app.search_open(), "the search stays open");
    assert_eq!(app.search_query(), "tide");
    assert_eq!(
        app.search_matches().len(),
        3,
        "the new track is in the pool"
    );
    assert_eq!(
        app.search_matches()[app.search_selected()].path,
        picked,
        "the selection follows its track"
    );

    // A selected track that is gone leaves the selection inside the list.
    fs::remove_file(&picked).unwrap();
    app.index_finished(index_in_background(&temp));
    assert_eq!(app.search_matches().len(), 2);
    assert!(app.search_selected() < 2);
}

#[test]
fn u_says_the_index_could_not_be_opened_instead_of_asking_for_a_library() {
    let mut app = App::browse(None);
    app.index_unavailable("unable to open database file");

    assert_eq!(app.on_key(key(KeyCode::Char('u'))), Action::None);
    let status = app.status().to_owned();
    assert!(status.contains("cannot open the library index"), "{status}");
    assert!(status.contains("unable to open database file"), "{status}");
    assert!(!status.contains("no library set"), "{status}");
    assert_eq!(app.take_library_root(), None);
}

// ---- what velo saves as it goes, and what it restores at launch (S2, S3) ----

/// A throwaway directory for a config file, outside the worktree.
fn config_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("velo-ui-config-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// An app over three tracks, wired to a config file holding exactly the
/// default text.
fn app_with_default_config(config: &Path) -> App<'static> {
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(config, velo::config::default_config_text()).unwrap();
    let mut app = app_with_three_tracks();
    app.set_config_file(Some(config.to_path_buf()));
    app
}

/// The 80×24 screen velo draws for `app`.
fn screen_of(app: &App<'_>) -> String {
    let mut terminal =
        velo::ui::ratatui::Terminal::new(velo::ui::ratatui::backend::TestBackend::new(80, 24))
            .unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let buffer = terminal.backend().buffer();
    (0..24)
        .map(|y| {
            (0..80)
                .map(move |x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Builds an app the way the launch paths do: config file attached, the
/// volume from the file set, then the session restored from the file.
fn launched_from<'lib>(
    temp: &'lib TempLibrary,
    dir: &Path,
    state: &str,
) -> (PathBuf, String, App<'lib>) {
    let config = dir.join("config.toml");
    fs::write(
        &config,
        format!("bit_perfect = false\nreplay_gain = \"track\"\n\n[state]\n{state}"),
    )
    .unwrap();
    let before = fs::read_to_string(&config).unwrap();
    let mut app = App::browse(Some(&temp.library));
    app.set_config_file(Some(config.clone()));
    let cli = velo::config::Cli::try_parse_from(["velo"]).unwrap();
    let settings = velo::config::Config::load_from(&config)
        .unwrap()
        .merged(&cli);
    app.set_volume(settings.volume);
    app.set_session(settings.session);
    (config, before, app)
}

#[test]
fn volume_is_written_only_when_it_leaves_what_the_file_says() {
    let dir = config_dir("volume-save");
    let config = dir.join("config.toml");
    let mut app = app_with_default_config(&config);
    let default = velo::config::default_config_text();

    // `+` at 100, the volume the file already holds: nothing is written.
    press(&mut app, KeyCode::Char('+'));
    assert_eq!(fs::read_to_string(&config).unwrap(), default);

    // A real change is written per key, under [state], in place.
    press(&mut app, KeyCode::Char('-'));
    assert_eq!(
        fs::read_to_string(&config).unwrap(),
        default.replace("volume = 100", "volume = 95"),
        "one key changed in place"
    );

    // A launch value (`set_volume`, then `set_session`, as the launch paths
    // do) is remembered as what the file says, never written back.
    app.set_volume(100);
    app.set_session(velo::config::Session::default());
    assert_eq!(
        fs::read_to_string(&config).unwrap(),
        default.replace("volume = 100", "volume = 95"),
        "the launch value is not written"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn repeat_and_shuffle_are_written_when_they_change() {
    let dir = config_dir("repeat-shuffle");
    let config = dir.join("config.toml");
    let mut app = app_with_default_config(&config);
    let default = velo::config::default_config_text();

    press(&mut app, KeyCode::Char('r'));
    assert_eq!(
        fs::read_to_string(&config).unwrap(),
        default.replace("repeat = \"off\"", "repeat = \"all\"")
    );
    press(&mut app, KeyCode::Char('s'));
    assert_eq!(
        fs::read_to_string(&config).unwrap(),
        default
            .replace("repeat = \"off\"", "repeat = \"all\"")
            .replace("shuffle = false", "shuffle = true")
    );

    // Off again: `off` and `false` are values too, and they are written.
    press(&mut app, KeyCode::Char('r'));
    press(&mut app, KeyCode::Char('r'));
    press(&mut app, KeyCode::Char('s'));
    assert_eq!(fs::read_to_string(&config).unwrap(), default);

    let _ = fs::remove_dir_all(&dir);
}

/// Low power (issue #11, S1): `P` toggles the session switch and writes
/// `[ui] low_power` the way `s` writes `[state] shuffle` — once per
/// change, and never when the value it would write is already there.
#[test]
fn p_toggles_low_power_and_writes_the_key_the_way_s_writes_shuffle() {
    let dir = config_dir("low-power");
    let config = dir.join("config.toml");
    let mut app = app_with_default_config(&config);
    let default = velo::config::default_config_text();

    press(&mut app, KeyCode::Char('P'));
    assert_eq!(
        fs::read_to_string(&config).unwrap(),
        default.replace("low_power = false", "low_power = true")
    );
    // And back out: `false` is a value too, and it is written.
    press(&mut app, KeyCode::Char('P'));
    assert_eq!(fs::read_to_string(&config).unwrap(), default);

    let _ = fs::remove_dir_all(&dir);
}

/// S14 (decision A): `tab` is a transient round trip, not a setting — moving
/// it to the queue and back writes nothing under `[state]`, and the file
/// stays byte-identical.
#[test]
fn the_tab_round_trip_writes_nothing_under_state() {
    let dir = config_dir("persist-tab-inert");
    let config = dir.join("config.toml");
    fs::write(&config, velo::config::default_config_text()).unwrap();
    let mut app = App::browse(None);
    app.set_config_file(Some(config.clone()));
    let default = velo::config::default_config_text();

    // Moving the tab writes nothing: to the queue…
    press(&mut app, KeyCode::Tab);
    assert_eq!(
        fs::read_to_string(&config).unwrap(),
        default,
        "tab to the queue wrote nothing"
    );

    // …and back. The whole round trip leaves the file byte-identical.
    press(&mut app, KeyCode::Tab);
    assert_eq!(
        fs::read_to_string(&config).unwrap(),
        default,
        "the round trip to the queue and back wrote nothing"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_library_view_and_the_open_node_are_written_as_you_move() {
    let temp = library_for_views("persist-move");
    let dir = config_dir("persist-move");
    let config = dir.join("config.toml");
    fs::write(&config, velo::config::default_config_text()).unwrap();
    let mut app = App::browse(Some(&temp.library));
    app.set_config_file(Some(config.clone()));
    let default = velo::config::default_config_text();
    // New node keys are appended at the end of the `[state]` table, right
    // before the next header — after the comment block that stands there —
    // which is where the writer puts an appended key (T4). Everything else
    // is replaced in place. `view` is retired (S14): it stays exactly what
    // the default file wrote, however much you move.
    let expected = |browse_by: &str, keys: &str| {
        default
            .replace(
                "browse_by = \"artists\"",
                &format!("browse_by = \"{browse_by}\""),
            )
            .replace(
                "# the built-in default wins over the theme named above.\n[theme]",
                &format!("# the built-in default wins over the theme named above.\n{keys}[theme]"),
            )
    };
    let file = || fs::read_to_string(&config).unwrap();

    // A number key changes the view, and the file follows.
    act(&mut app, KeyCode::Char('2'));
    assert_eq!(file(), expected("albums", ""));

    // Opening an album from the albums view writes both of its keys.
    assert_eq!(act(&mut app, KeyCode::Enter), Action::Descend);
    assert_eq!(
        file(),
        expected("albums", "artist = \"Tide\"\nalbum = \"Harbor\"\n")
    );

    // Walking back up removes the keys of the levels left behind.
    assert_eq!(act(&mut app, KeyCode::Esc), Action::Ascend);
    assert_eq!(file(), expected("albums", ""));

    // The artists view drills down one key at a time.
    act(&mut app, KeyCode::Char('1'));
    assert_eq!(file(), expected("artists", ""), "no node, nothing extra");
    act(&mut app, KeyCode::Char('j')); // onto Tide
    assert_eq!(act(&mut app, KeyCode::Enter), Action::Descend);
    assert_eq!(file(), expected("artists", "artist = \"Tide\"\n"));
    assert_eq!(act(&mut app, KeyCode::Enter), Action::Descend);
    assert_eq!(
        file(),
        expected("artists", "artist = \"Tide\"\nalbum = \"Harbor\"\n")
    );
    assert_eq!(act(&mut app, KeyCode::Esc), Action::Ascend);
    assert_eq!(file(), expected("artists", "artist = \"Tide\"\n"));
    assert_eq!(act(&mut app, KeyCode::Esc), Action::Ascend);
    assert_eq!(
        file(),
        expected("artists", ""),
        "back at the top, no node keys"
    );

    // A genre node is its own key.
    act(&mut app, KeyCode::Char('4'));
    assert_eq!(act(&mut app, KeyCode::Enter), Action::Descend);
    assert_eq!(file(), expected("genres", "genre = \"Jazz\"\n"));
    assert_eq!(act(&mut app, KeyCode::Esc), Action::Ascend);
    assert_eq!(file(), expected("genres", ""));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_first_volume_save_moves_a_legacy_top_level_volume_under_state() {
    let dir = config_dir("legacy-volume");
    let config = dir.join("config.toml");
    fs::write(
        &config,
        "# kept comment\nvolume = 80\n\n[state]\nshuffle = false\n",
    )
    .unwrap();
    let mut app = app_with_three_tracks();
    app.set_config_file(Some(config.clone()));

    press(&mut app, KeyCode::Char('-'));

    let text = fs::read_to_string(&config).unwrap();
    let volume_lines: Vec<&str> = text
        .lines()
        .filter(|line| line.starts_with("volume"))
        .collect();
    assert_eq!(
        volume_lines,
        ["volume = 95"],
        "exactly one volume line: {text}"
    );
    let state = text.find("[state]").unwrap();
    let volume = text.find("volume = 95").unwrap();
    assert!(volume > state, "the volume lives under [state]: {text}");
    assert!(
        text.contains("# kept comment"),
        "the user's file survives: {text}"
    );

    let file = velo::config::Config::load_from(&config).unwrap();
    assert_eq!(file.state.volume, Some(95));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_failed_write_says_velo_and_keeps_the_app_running() {
    let dir = config_dir("unwritable");
    let config = dir.join("config.toml");
    // The config path is a directory: every write fails.
    fs::create_dir_all(&config).unwrap();
    let mut app = app_with_three_tracks();
    app.set_config_file(Some(config));
    assert_eq!(app.status(), "");

    press(&mut app, KeyCode::Char('-'));
    assert!(app.status().starts_with("velo: "), "{}", app.status());

    // The app keeps running: the next key still does its work.
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.selected(), 1);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_launch_comes_back_to_what_the_file_says_and_writes_nothing() {
    let temp = library_for_views("persist-launch");
    let dir = config_dir("persist-launch");
    let (config, before, mut app) = launched_from(
        &temp,
        &dir,
        "volume = 60\nshuffle = true\nrepeat = \"all\"\nview = \"queue\"\nbrowse_by = \"albums\"\nartist = \"Tide\"\nalbum = \"Harbor\"\n",
    );

    assert_eq!(app.volume(), 60);
    assert_eq!(
        app.view(),
        View::Library,
        "the recorded tab is retired (S14): it is not applied — the launch \
         itself opens `Sources`"
    );
    assert_eq!(app.browse_by(), BrowseBy::Albums);
    assert_eq!(
        app.level(),
        Some(Level::Tracks {
            artist: "Tide".to_owned(),
            album: "Harbor".to_owned()
        })
    );
    assert_eq!(app.tracks().len(), 2, "the album's tracks are on screen");
    assert_eq!(app.status(), "", "the restore is silent");
    // The screen carries the restored repeat and the restored volume.
    let screen = screen_of(&app);
    assert!(
        screen.contains("repeat all"),
        "the transport names it: {screen}"
    );
    assert!(screen.contains(" 60"), "the slider shows it: {screen}");

    // And the launch itself wrote nothing.
    assert_eq!(fs::read_to_string(&config).unwrap(), before);

    // The restored values are the live ones: the next change moves on from
    // them instead of rewriting them.
    press(&mut app, KeyCode::Char('s'));
    assert!(
        fs::read_to_string(&config)
            .unwrap()
            .contains("shuffle = false")
    );
    press(&mut app, KeyCode::Char('r'));
    assert!(
        fs::read_to_string(&config)
            .unwrap()
            .contains("repeat = \"one\"")
    );

    let _ = fs::remove_dir_all(&dir);
}

/// S14 (decision A): the compatibility proof. A file that still carries
/// `view = "queue"` parses, the launch opens `Sources`, and entering
/// `Local library` lands on the library (issue #27) — the recorded tab
/// steers nothing.
#[test]
fn a_recorded_view_still_parses_and_the_launch_lands_on_sources_then_the_library() {
    let temp = library_for_views("persist-retired-view");
    let dir = config_dir("persist-retired-view");
    let (config, before, mut app) =
        launched_from(&temp, &dir, "view = \"queue\"\nbrowse_by = \"albums\"\n");

    assert_eq!(app.view(), View::Library, "the recorded tab is not applied");

    // The shell's launch path: `Sources` first, the library behind `enter`.
    app.open_sources();
    assert_eq!(app.view(), View::Sources);
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.view(), View::Library);
    assert_eq!(app.level(), Some(Level::AllAlbums));

    // And the launch itself wrote nothing.
    assert_eq!(fs::read_to_string(&config).unwrap(), before);

    let _ = fs::remove_dir_all(&dir);
}

/// S14 (decision A): a file without a `view` key parses and behaves exactly
/// as one with it — the rest of `[state]` is restored, and the launch lands
/// on `Sources` and then the library.
#[test]
fn a_file_without_the_view_key_still_parses_and_restores_the_rest() {
    let temp = library_for_views("persist-no-view");
    let dir = config_dir("persist-no-view");
    let (config, before, mut app) = launched_from(
        &temp,
        &dir,
        "browse_by = \"albums\"\nartist = \"Tide\"\nalbum = \"Harbor\"\n",
    );

    assert_eq!(app.browse_by(), BrowseBy::Albums);
    assert_eq!(app.view(), View::Library, "no view key is no tab either");
    assert_eq!(
        app.level(),
        Some(Level::Tracks {
            artist: "Tide".to_owned(),
            album: "Harbor".to_owned()
        })
    );

    app.open_sources();
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.view(), View::Library);
    assert_eq!(
        app.level(),
        Some(Level::Tracks {
            artist: "Tide".to_owned(),
            album: "Harbor".to_owned()
        }),
        "the open node survives the launch untouched"
    );
    assert_eq!(fs::read_to_string(&config).unwrap(), before);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_node_the_index_no_longer_has_falls_back_silently_to_the_level_above() {
    let temp = library_for_views("persist-gone");
    let dir = config_dir("persist-gone");

    // The artist is gone: the artists list stays, silently.
    let (config, before, app) =
        launched_from(&temp, &dir, "browse_by = \"artists\"\nartist = \"Ghost\"\n");
    assert_eq!(app.level(), Some(Level::Artists));
    assert_eq!(app.status(), "");
    assert_eq!(fs::read_to_string(&config).unwrap(), before);

    // The album is gone but the artist is not: the artist's albums stay.
    let (config, before, app) = launched_from(
        &temp,
        &dir,
        "browse_by = \"artists\"\nartist = \"Tide\"\nalbum = \"Missing\"\n",
    );
    assert_eq!(
        app.level(),
        Some(Level::Albums {
            artist: "Tide".to_owned()
        })
    );
    assert_eq!(app.status(), "");
    assert_eq!(fs::read_to_string(&config).unwrap(), before);

    // A genre that is gone: the genres list stays.
    let (config, before, app) = launched_from(
        &temp,
        &dir,
        "browse_by = \"genres\"\ngenre = \"Chiptune\"\n",
    );
    assert_eq!(app.level(), Some(Level::Genres));
    assert_eq!(app.status(), "");
    assert_eq!(fs::read_to_string(&config).unwrap(), before);

    // A flat view with no node: the flat list.
    let (config, before, app) = launched_from(&temp, &dir, "browse_by = \"tracks\"\n");
    assert_eq!(app.level(), Some(Level::AllTracks));
    assert_eq!(app.status(), "");
    assert_eq!(fs::read_to_string(&config).unwrap(), before);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn u_after_choosing_a_library_without_an_open_index_says_why_nothing_happens() {
    let home = std::env::temp_dir().join(format!("velo-ui-no-index-{}", std::process::id()));
    fs::create_dir_all(home.join("Music")).unwrap();
    for error in [Some("unable to open database file"), None] {
        let mut app = App::browse(None);
        if let Some(error) = error {
            app.index_unavailable(error);
        }
        app.ask_for_library_under(None, Some(home.clone()));
        type_text(&mut app, "Music");
        press(&mut app, KeyCode::Enter);
        assert!(!app.library_prompt_open());

        assert_eq!(app.on_key(key(KeyCode::Char('u'))), Action::None);
        let status = app.status().to_owned();
        assert!(status.contains("index"), "{status}");
        assert!(!status.contains("no library set"), "{status}");
        if let Some(error) = error {
            assert!(status.contains(error), "{status}");
        } else {
            assert!(status.contains("not open"), "{status}");
        }
    }
    let _ = fs::remove_dir_all(&home);
}

// ---- the settings card (S6, S7) ---------------------------------------------

#[test]
fn o_opens_the_settings_card_and_esc_closes_it() {
    let mut app = app_with_three_tracks();
    assert!(!app.settings_open());

    assert_eq!(app.on_key(key(KeyCode::Char('o'))), Action::None);
    assert!(app.settings_open(), "o opens the settings card");

    press(&mut app, KeyCode::Esc);
    assert!(!app.settings_open(), "esc closes the settings card");

    // Closed again, the keys mean what they meant before.
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.selected(), 1);
}

#[test]
fn the_settings_cursor_moves_between_settings_only_and_stops_at_the_ends() {
    let mut app = app_with_three_tracks();
    press(&mut app, KeyCode::Char('o'));
    assert_eq!(
        app.settings_focused(),
        "replay gain",
        "the card opens on the first setting; the volume is not a row"
    );

    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.settings_focused(), "repeat");
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.settings_focused(), "shuffle");
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(
        app.settings_focused(),
        "theme",
        "the card has a theme row (issue #10)"
    );
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(
        app.settings_focused(),
        "cover art",
        "the theme row is a setting and the last of its section: the card has no colour rows (issue #37)"
    );

    press(&mut app, KeyCode::Char('G'));
    assert_eq!(
        app.settings_focused(),
        "folder",
        "G jumps to the last setting"
    );
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(
        app.settings_focused(),
        "folder",
        "j stops at the last setting"
    );

    press(&mut app, KeyCode::Char('g'));
    assert_eq!(
        app.settings_focused(),
        "replay gain",
        "g jumps to the first setting"
    );
    press(&mut app, KeyCode::Char('k'));
    assert_eq!(
        app.settings_focused(),
        "replay gain",
        "k stops at the first setting"
    );
}

#[test]
fn an_open_settings_card_holds_back_q_and_space_until_it_closes() {
    let mut app = app_with_three_tracks();
    press(&mut app, KeyCode::Char('o'));
    assert!(app.settings_open());

    for code in [KeyCode::Char('q'), KeyCode::Char(' ')] {
        assert_eq!(
            app.on_key(key(code)),
            Action::None,
            "{code:?} reached the interface under the card"
        );
        assert!(app.settings_open(), "the card stays open");
    }
    assert_eq!(app.selected(), 0, "the library selection never moved");

    press(&mut app, KeyCode::Esc);
    assert!(!app.settings_open());
    assert_eq!(app.on_key(key(KeyCode::Char('q'))), Action::Quit);
    assert_eq!(app.on_key(key(KeyCode::Char(' '))), Action::TogglePause);
}

#[test]
fn enter_on_a_visibility_row_flips_it_and_writes_cover_art_to_the_file() {
    let dir = config_dir("settings-cover-art");
    let config = dir.join("config.toml");
    let mut app = app_with_default_config(&config);
    let default = velo::config::default_config_text();

    press(&mut app, KeyCode::Char('o'));
    for _ in 0..4 {
        press(&mut app, KeyCode::Char('j'));
    }
    assert_eq!(app.settings_focused(), "cover art");
    press(&mut app, KeyCode::Enter);

    assert_eq!(
        fs::read_to_string(&config).unwrap(),
        default.replace("cover_art = true", "cover_art = false"),
        "the switch is written per key, in place"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn enter_on_the_folder_row_opens_the_library_prompt() {
    let dir = config_dir("settings-folder");
    let config = dir.join("config.toml");
    let mut app = app_with_default_config(&config);

    press(&mut app, KeyCode::Char('o'));
    press(&mut app, KeyCode::Char('G'));
    assert_eq!(app.settings_focused(), "folder");
    press(&mut app, KeyCode::Enter);

    assert!(
        app.library_prompt_open(),
        "the existing library prompt opened"
    );
    assert!(!app.settings_open(), "the card gave the keys to the prompt");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn enter_on_the_advisories_row_writes_it_to_the_file() {
    let dir = config_dir("settings-advisories");
    let config = dir.join("config.toml");
    let mut app = app_with_default_config(&config);
    let default = velo::config::default_config_text();

    press(&mut app, KeyCode::Char('o'));
    for _ in 0..8 {
        press(&mut app, KeyCode::Char('j'));
    }
    assert_eq!(app.settings_focused(), "advisories");
    press(&mut app, KeyCode::Enter);

    assert_eq!(
        fs::read_to_string(&config).unwrap(),
        default.replace("advisories = true", "advisories = false"),
        "the switch is written per key, under [ui]"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn enter_on_the_view_numbers_row_writes_it_to_the_file() {
    let dir = config_dir("settings-view-numbers");
    let config = dir.join("config.toml");
    let mut app = app_with_default_config(&config);
    let default = velo::config::default_config_text();

    press(&mut app, KeyCode::Char('o'));
    for _ in 0..9 {
        press(&mut app, KeyCode::Char('j'));
    }
    assert_eq!(app.settings_focused(), "view numbers");
    press(&mut app, KeyCode::Enter);

    assert_eq!(
        fs::read_to_string(&config).unwrap(),
        default.replace("view_numbers = true", "view_numbers = false"),
        "the switch is written per key, under [ui]"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_question_mark_inside_the_settings_card_does_nothing() {
    let mut app = app_with_three_tracks();
    press(&mut app, KeyCode::Char('o'));

    assert_eq!(app.on_key(key(KeyCode::Char('?'))), Action::None);
    assert!(app.settings_open(), "the card stays open");
    assert!(!app.help_open(), "? does not open the help over the card");
}

// ---- the theme picker (issue #37) ------------------------------------------

use velo::config::{ThemeColor, ThemeColors, ThemeKeys};

/// An app launched over a config file without `[theme]` colour keys, with a
/// `nord` theme file on disk and the theme library wired in the way the
/// launch paths do it.
fn theme_app(name: &str) -> (PathBuf, App<'static>) {
    let dir = config_dir(name);
    let config = dir.join("config.toml");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        &config,
        "bit_perfect = false\nreplay_gain = \"track\"\n\n[state]\nvolume = 100\n",
    )
    .unwrap();
    let themes = dir.join("themes");
    fs::create_dir_all(&themes).unwrap();
    fs::write(
        themes.join("nord.toml"),
        "accent = \"#88c0d0\"\nsecondary = \"#81a1c1\"\n",
    )
    .unwrap();
    // A second file that overrides no built-in: it lands after them.
    fs::write(themes.join("sunset.toml"), "accent = \"#ff5500\"\n").unwrap();

    let keys = ThemeKeys::default();
    let selection = velo::config::select_theme(Some(&themes), &keys);
    let mut app = app_with_three_tracks();
    app.set_config_file(Some(config.clone()));
    app.set_theme_library(selection, keys);
    (config, app)
}

fn open_on_theme_row(app: &mut App<'_>) {
    press(app, KeyCode::Char('o'));
    for _ in 0..3 {
        press(app, KeyCode::Char('j'));
    }
    assert_eq!(app.settings_focused(), "theme", "the theme row is focused");
}

fn open_theme_picker(app: &mut App<'_>) {
    open_on_theme_row(app);
    press(app, KeyCode::Enter);
    assert!(
        app.theme_picker_open(),
        "enter on the theme row opens the picker"
    );
}

/// The picker's rows for `theme_app`: `default`, the eleven built-ins in
/// name order — the `nord.toml` file taking nord's row — and the folder
/// file that overrides nothing, last.
const PICKER_ROWS: [&str; 13] = [
    "default",
    "catppuccin-latte",
    "catppuccin-mocha",
    "dracula",
    "gruvbox-dark",
    "monokai",
    "nord",
    "one-dark",
    "one-light",
    "solarized-dark",
    "solarized-light",
    "tokyo-night",
    "sunset",
];

#[test]
fn the_settings_card_has_one_theme_row_and_no_colour_rows() {
    // The author: the colours are the theme's inside, not settings rows.
    // Walking the card end to end must meet every row that remains and
    // none of the seven colours.
    let mut app = app_with_three_tracks();
    press(&mut app, KeyCode::Char('o'));

    let mut labels = vec![app.settings_focused().to_owned()];
    for _ in 0..20 {
        press(&mut app, KeyCode::Char('j'));
        let label = app.settings_focused();
        if labels.last().is_some_and(|last| last != label) {
            labels.push(label.to_owned());
        }
    }
    assert_eq!(
        labels,
        [
            "replay gain",
            "repeat",
            "shuffle",
            "theme",
            "cover art",
            "spectrum",
            "hints",
            "top bar",
            "advisories",
            "view numbers",
            "folder",
        ],
        "one theme row, no colour rows, folder still last"
    );
}

#[test]
fn enter_on_the_theme_row_opens_the_picker_and_the_card_gives_up_the_keys() {
    let mut app = app_with_three_tracks();
    open_on_theme_row(&mut app);

    press(&mut app, KeyCode::Enter);

    assert!(app.theme_picker_open(), "the picker is open");
    assert!(!app.settings_open(), "the card handed the keys over");
    press(&mut app, KeyCode::Esc);
    assert!(!app.theme_picker_open(), "esc closes the picker");
}

#[test]
fn the_picker_lists_default_then_the_built_ins_then_the_folder_files() {
    let (_config, mut app) = theme_app("picker-rows");
    open_theme_picker(&mut app);

    assert_eq!(app.theme_picker_names(), PICKER_ROWS.to_vec());
    assert_eq!(
        app.theme_picker_cursor(),
        0,
        "the cursor starts on the applied theme"
    );
}

#[test]
fn moving_the_cursor_applies_that_theme_at_once_and_writes_nothing() {
    let (config, mut app) = theme_app("picker-preview");
    let before = fs::read_to_string(&config).unwrap();
    open_theme_picker(&mut app);

    press(&mut app, KeyCode::Char('j'));

    assert_eq!(app.theme_picker_cursor(), 1);
    assert_eq!(
        app.theme_name(),
        "default",
        "the name is not applied by moving"
    );
    assert_eq!(
        app.theme_colors().accent,
        ThemeColor::Rgb {
            red: 0x88,
            green: 0x39,
            blue: 0xef,
        },
        "catppuccin-latte is on screen: the preview is the interface"
    );
    assert_eq!(
        fs::read_to_string(&config).unwrap(),
        before,
        "a preview never writes"
    );
    let _ = fs::remove_dir_all(dir_of(&config));
}

#[test]
fn esc_restores_the_palette_in_force_when_the_picker_opened() {
    let dir = config_dir("picker-esc");
    let config = dir.join("config.toml");
    fs::create_dir_all(&dir).unwrap();
    fs::write(&config, "[theme]\naccent = \"#d70000\"\n").unwrap();
    let themes = dir.join("themes");
    fs::create_dir_all(&themes).unwrap();
    fs::write(themes.join("nord.toml"), "accent = \"#88c0d0\"\n").unwrap();
    let keys = ThemeKeys {
        accent: Some(ThemeColor::Rgb {
            red: 0xd7,
            green: 0x00,
            blue: 0x00,
        }),
        ..ThemeKeys::default()
    };
    let selection = velo::config::select_theme(Some(&themes), &keys);
    let mut app = app_with_three_tracks();
    app.set_config_file(Some(config.clone()));
    app.set_theme_library(selection, keys);
    let before = fs::read_to_string(&config).unwrap();
    open_theme_picker(&mut app);

    press(&mut app, KeyCode::Char('j'));
    assert_eq!(
        app.theme_colors().secondary,
        ThemeColor::Rgb {
            red: 0x1e,
            green: 0x66,
            blue: 0xf5,
        },
        "catppuccin-latte is on screen: the preview is the interface"
    );
    assert_eq!(
        app.theme_colors().accent,
        ThemeColor::Rgb {
            red: 0xd7,
            green: 0x00,
            blue: 0x00,
        },
        "the [theme] override rides on top of every preview, as the layering says"
    );
    press(&mut app, KeyCode::Esc);

    assert!(!app.theme_picker_open(), "esc closes the picker");
    assert_eq!(
        app.theme_colors().accent,
        ThemeColor::Rgb {
            red: 0xd7,
            green: 0x00,
            blue: 0x00,
        },
        "the palette at open, [theme] override included, is back"
    );
    assert_eq!(
        app.theme_colors().secondary,
        ThemeColors::default().secondary,
        "and so is everything the preview had changed"
    );
    assert_eq!(
        fs::read_to_string(&config).unwrap(),
        before,
        "esc writes nothing"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn enter_applies_the_focused_theme_and_writes_the_name_once() {
    let (config, mut app) = theme_app("picker-enter");
    let before = fs::read_to_string(&config).unwrap();
    open_theme_picker(&mut app);

    let nord = app
        .theme_picker_names()
        .iter()
        .position(|name| name == "nord")
        .expect("nord is a row");
    for _ in 0..nord {
        press(&mut app, KeyCode::Char('j'));
    }
    press(&mut app, KeyCode::Enter);

    assert!(!app.theme_picker_open(), "enter applies and closes");
    assert_eq!(app.theme_name(), "nord");
    assert_eq!(
        app.theme_colors().accent,
        ThemeColor::Rgb {
            red: 0x88,
            green: 0xc0,
            blue: 0xd0,
        }
    );
    assert_eq!(
        fs::read_to_string(&config).unwrap(),
        format!("{before}[theme]\nname = \"nord\"\n"),
        "the name is written once and the file is otherwise untouched"
    );
    let _ = fs::remove_dir_all(dir_of(&config));
}

#[test]
fn enter_on_the_applied_theme_closes_without_writing() {
    let (config, mut app) = theme_app("picker-enter-same");
    let before = fs::read_to_string(&config).unwrap();
    open_theme_picker(&mut app);

    press(&mut app, KeyCode::Enter);

    assert!(!app.theme_picker_open());
    assert_eq!(app.theme_name(), "default");
    assert_eq!(
        fs::read_to_string(&config).unwrap(),
        before,
        "the write contract: only when it changed"
    );
    let _ = fs::remove_dir_all(dir_of(&config));
}

#[test]
fn the_picker_navigation_wraps_at_the_ends() {
    let (config, mut app) = theme_app("picker-wrap");
    open_theme_picker(&mut app);

    press(&mut app, KeyCode::Char('k'));
    assert_eq!(
        app.theme_picker_cursor(),
        app.theme_picker_names().len() - 1,
        "up from the first row wraps to the last"
    );
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(
        app.theme_picker_cursor(),
        0,
        "down from the last row wraps to the first"
    );
    let _ = fs::remove_dir_all(dir_of(&config));
}

#[test]
fn every_other_key_waits_while_the_picker_is_open() {
    let mut app = app_with_three_tracks();
    open_theme_picker(&mut app);

    for code in [
        KeyCode::Char('q'),
        KeyCode::Char(' '),
        KeyCode::Char('?'),
        KeyCode::Char('o'),
    ] {
        assert_eq!(
            app.on_key(key(code)),
            Action::None,
            "{code:?} reached the interface under the picker"
        );
        assert!(app.theme_picker_open(), "the picker stays open");
        assert!(!app.help_open(), "? never opens the help over the picker");
    }
    press(&mut app, KeyCode::Esc);
    assert_eq!(
        app.on_key(key(KeyCode::Char('q'))),
        Action::Quit,
        "the keys come back when the picker closes"
    );
}

#[test]
fn theme_notices_are_said_once_in_the_status_line() {
    let dir = config_dir("theme-notice");
    let mut app = app_with_three_tracks();
    let selection = velo::config::ThemeSelection {
        palette: ThemeColors::default(),
        applied: "default".to_owned(),
        themes: Vec::new(),
        notices: vec![format!(
            "theme \"nord\" is not in {} — using the built-in",
            dir.display()
        )],
    };
    app.set_theme_library(selection, ThemeKeys::default());

    app.tick();
    assert!(
        app.status().contains("nord"),
        "the notice is on the status line: {}",
        app.status()
    );
    app.tick();
    app.tick();
    assert!(
        app.status().matches("nord").count() == 1,
        "the notice is said once, not per tick: {}",
        app.status()
    );
    let _ = fs::remove_dir_all(&dir);
}

fn dir_of(config: &Path) -> PathBuf {
    config.parent().unwrap().to_path_buf()
}

// --------------------------------------------------- a launch with no library

#[test]
fn a_launch_without_library_or_server_says_what_to_do_instead_of_asking_for_a_folder() {
    let mut app = App::browse(None);
    app.launch_without_library(false);
    assert!(
        !app.library_prompt_open(),
        "no prompt: the sentence replaces it"
    );
    // One sentence, naming both ways to configure.
    assert_eq!(
        app.status(),
        "velo: no library and no server configured — set `library` in the \
         config file (or run velo --library <PATH>), or add a [subsonic] \
         server and run velo --sync"
    );
}

#[test]
fn a_launch_with_only_a_subsonic_server_asks_for_no_folder() {
    let mut app = App::browse(None);
    app.launch_without_library(true);
    assert!(
        !app.library_prompt_open(),
        "the synced index is the library"
    );
    assert_eq!(app.status(), "");
}

#[test]
fn the_second_seek_gear_moves_thirty_seconds_and_the_plain_keys_keep_five() {
    let mut app = app_with_three_tracks();

    assert_eq!(
        app.on_key(key(KeyCode::Left)),
        Action::Seek { delta_secs: -5 }
    );
    assert_eq!(
        app.on_key(key(KeyCode::Right)),
        Action::Seek { delta_secs: 5 }
    );
    assert_eq!(
        app.on_key(key(KeyCode::Char(','))),
        Action::Seek { delta_secs: -5 }
    );
    assert_eq!(
        app.on_key(key(KeyCode::Char('.'))),
        Action::Seek { delta_secs: 5 }
    );

    // Shift on an arrow is a modifier a terminal reports, so the coarse gear
    // rides on the same key.
    let shift_left = KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT);
    let shift_right = KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT);
    assert_eq!(app.on_key(shift_left), Action::Seek { delta_secs: -30 });
    assert_eq!(app.on_key(shift_right), Action::Seek { delta_secs: 30 });

    // A character key arrives already shifted, so the pair shift produces
    // carries the coarse gear for the comma keys.
    assert_eq!(
        app.on_key(key(KeyCode::Char('<'))),
        Action::Seek { delta_secs: -30 }
    );
    assert_eq!(
        app.on_key(key(KeyCode::Char('>'))),
        Action::Seek { delta_secs: 30 }
    );
}

// ---- the seek prompt (issue #6) ------------------------------------------

#[test]
fn parse_seek_reads_percentages_clock_positions_and_bare_seconds() {
    use velo::ui::{SeekPosition, parse_seek};

    assert_eq!(parse_seek("70%"), Ok(SeekPosition::Percent(70)));
    assert_eq!(parse_seek(" 100% "), Ok(SeekPosition::Percent(100)));
    assert_eq!(parse_seek("1:20"), Ok(SeekPosition::Secs(80)));
    assert_eq!(parse_seek("1:20:30"), Ok(SeekPosition::Secs(4830)));
    assert_eq!(parse_seek("90"), Ok(SeekPosition::Secs(90)));
    assert_eq!(parse_seek("0:00"), Ok(SeekPosition::Secs(0)));

    for refused in ["", "   ", "101%", "%", "1:99", "1:2:3:4", "abc", "1:20m"] {
        assert!(parse_seek(refused).is_err(), "`{refused}` must be refused");
    }
}

#[test]
fn colon_opens_the_seek_prompt_and_enter_jumps() {
    let mut app = app_with_three_tracks();
    assert!(!app.seek_prompt_open());

    // `:` opens it, and what is typed goes into the prompt, not the list.
    assert_eq!(app.on_key(key(KeyCode::Char(':'))), Action::None);
    assert!(app.seek_prompt_open());
    for typed in ['1', ':', '2', '0'] {
        assert_eq!(app.on_key(key(KeyCode::Char(typed))), Action::None);
    }
    assert_eq!(app.seek_prompt_input(), "1:20");
    assert_eq!(app.selected(), 0, "the list never moved while typing");

    assert_eq!(app.on_key(key(KeyCode::Enter)), Action::SeekTo { secs: 80 });
    assert!(!app.seek_prompt_open(), "an accepted answer closes it");
}

#[test]
fn esc_closes_the_seek_prompt_and_the_list_moves_again() {
    let mut app = app_with_three_tracks();
    app.on_key(key(KeyCode::Char(':')));
    app.on_key(key(KeyCode::Char('9')));
    assert!(app.seek_prompt_open());

    assert_eq!(app.on_key(key(KeyCode::Esc)), Action::None);
    assert!(!app.seek_prompt_open());

    assert_eq!(app.on_key(key(KeyCode::Char('j'))), Action::None);
    assert_eq!(app.selected(), 1, "`j` moves the list again");
}

#[test]
fn a_refused_answer_leaves_the_seek_prompt_open() {
    let mut app = app_with_three_tracks();
    app.on_key(key(KeyCode::Char(':')));
    for typed in ['7', '0', '%'] {
        app.on_key(key(KeyCode::Char(typed)));
    }

    // Nothing is playing, so a percentage has no track length to become a
    // position from: the answer is refused and the prompt stays open.
    assert_eq!(app.on_key(key(KeyCode::Enter)), Action::None);
    assert!(app.seek_prompt_open());

    // `j` afterwards types into the prompt instead of moving the selection.
    assert_eq!(app.on_key(key(KeyCode::Char('j'))), Action::None);
    assert_eq!(app.selected(), 0, "`j` never moved the list");
    assert_eq!(
        app.seek_prompt_input(),
        "70%j",
        "`j` was typed into the prompt"
    );
}

// ---- the recently played write hook (issue #5) --------------------------------

#[test]
fn the_history_tick_records_one_beginning_per_change_of_path() {
    // A temp directory of the same make as `TempLibrary`, for a config file
    // whose side the history is written on. Plain paths that need not exist.
    let root = std::env::temp_dir().join(format!("velo-ui-history-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    let config = root.join("velo").join("config.toml");
    let mut app = App::browse(None);
    app.set_config_file(Some(config.clone()));

    let a = PathBuf::from("/music/Tide/01 - Nightfall.flac");
    let b = PathBuf::from("/music/Tide/02 - Later.flac");
    let file_a = || Entry::File(a.clone());
    let file_b = || Entry::File(b.clone());

    // The same path twice is one beginning: the tick sees the same entry on
    // every frame, and only a change counts.
    app.history_tick(Some(&file_a()));
    app.history_tick(Some(&file_a()));
    assert_eq!(app.recently_played().len(), 1);

    // A different path is a new beginning, on top.
    app.history_tick(Some(&file_b()));
    assert_eq!(app.recently_played().len(), 2);
    assert_eq!(app.recently_played()[0].path, b.to_string_lossy());
    assert_eq!(app.recently_played()[1].path, a.to_string_lossy());

    // Playing the first again moves it up instead of duplicating it, with a
    // fresh moment.
    let earlier = app.recently_played()[1].played_at;
    app.history_tick(Some(&file_a()));
    assert_eq!(app.recently_played().len(), 2);
    assert_eq!(app.recently_played()[0].path, a.to_string_lossy());
    assert!(app.recently_played()[0].played_at >= earlier);

    // Nothing playing resets the seen identity, so the next beginning of
    // the same path counts again — but it still dedupes to one entry.
    app.history_tick(None);
    app.history_tick(Some(&file_a()));
    assert_eq!(app.recently_played().len(), 2, "no duplicate row");
    assert_eq!(app.recently_played()[0].path, a.to_string_lossy());

    // The file on disk is the list in memory.
    let file = velo::history::path_beside(&config);
    let from_disk = velo::history::History::load(&file).unwrap();
    assert_eq!(from_disk.entry, app.recently_played());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_station_beginning_records_one_entry_with_the_station_name() {
    // A temp directory of the same make as the history test above: a config
    // file whose side the history is written on.
    let root = std::env::temp_dir().join(format!("velo-ui-history-station-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    let config = root.join("velo").join("config.toml");
    let mut app = App::browse(None);
    app.set_config_file(Some(config));

    let nova = Entry::Station {
        url: String::from("https://stream.example/nova"),
        name: String::from("Nova"),
    };

    // A station beginning records one entry: the stream URL as the path,
    // the station's own name as the title, kind `station` — no library
    // lookup, there is nothing to look up.
    app.history_tick(Some(&nova));
    assert_eq!(app.recently_played().len(), 1);
    let top = &app.recently_played()[0];
    assert_eq!(top.kind, velo::history::Kind::Station);
    assert_eq!(top.path, "https://stream.example/nova");
    assert_eq!(top.title.as_deref(), Some("Nova"));
    assert_eq!(top.artist, None);
    assert_eq!(top.album, None);

    // The same station twice in a row is one beginning.
    app.history_tick(Some(&nova));
    assert_eq!(app.recently_played().len(), 1, "no duplicate row");

    // A file whose path is the same string is a different entry: a station
    // never collapses into a file.
    app.history_tick(Some(&Entry::File(PathBuf::from(
        "https://stream.example/nova",
    ))));
    assert_eq!(app.recently_played().len(), 2);
    assert_eq!(app.recently_played()[0].kind, velo::history::Kind::File);
    assert_eq!(app.recently_played()[1].kind, velo::history::Kind::Station);

    let _ = fs::remove_dir_all(&root);
}

// ---------------------------------------------------------- mono (issue #8)

#[test]
fn m_toggles_mono_for_the_session() {
    let mut app = app_with_three_tracks();
    assert!(!app.mono(), "a session starts in stereo");

    assert_eq!(app.on_key(key(KeyCode::Char('M'))), Action::ToggleMono);
    app.apply(Action::ToggleMono);
    assert!(app.mono());

    app.apply(Action::ToggleMono);
    assert!(!app.mono(), "M toggles it back off");
}

#[test]
fn m_with_bit_perfect_is_refused_in_the_status_line() {
    let mut app = app_with_three_tracks();
    app.set_bit_perfect(true);

    assert_eq!(app.on_key(key(KeyCode::Char('M'))), Action::ToggleMono);
    app.apply(Action::ToggleMono);
    assert!(!app.mono(), "bit-perfect leaves mono off");
    assert_eq!(
        app.status(),
        "velo: --mono cannot be combined with bit_perfect"
    );
}
