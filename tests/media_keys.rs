//! Media keys (issue #12): the OS's play/pause, next and previous reach the
//! same actions the interface keys already perform, and velo publishes what
//! is playing so the OS can show it.
//!
//! Every test drives the loopback pair — plain channels in place of an OS
//! media session — so no test touches a real media session, and a session
//! that refuses, a headless launch or a missing bus cannot fail a test:
//! the client is best effort by contract (S6).

use std::time::Duration;

use souvlaki::MediaControlEvent;
use velo::audio::output::Entry;
use velo::media_keys::{MediaCommand, MediaKeys, MediaState, NowPlaying, action_of, command_of};
use velo::ui::{Action, App};

type Loopback = (
    App<'static>,
    std::sync::mpsc::Sender<MediaCommand>,
    std::sync::mpsc::Receiver<MediaState>,
    std::sync::mpsc::Sender<Result<(), String>>,
);

/// An app whose media keys are wired to plain channels: the tests inject
/// commands and read what would have been published, without an OS session.
fn app_with_media() -> Loopback {
    let (media, commands, updates, init) = MediaKeys::loopback();
    let mut app = App::new(Vec::new());
    app.set_media_keys(media);
    (app, commands, updates, init)
}

// ---- the mapping (S4) -------------------------------------------------------

#[test]
fn the_media_events_map_onto_the_actions_the_keys_already_perform() {
    // Play, pause and toggle are one command: the space key's action.
    assert_eq!(
        command_of(&MediaControlEvent::Play),
        Some(MediaCommand::PlayPause)
    );
    assert_eq!(
        command_of(&MediaControlEvent::Pause),
        Some(MediaCommand::PlayPause)
    );
    assert_eq!(
        command_of(&MediaControlEvent::Toggle),
        Some(MediaCommand::PlayPause)
    );
    assert_eq!(
        command_of(&MediaControlEvent::Next),
        Some(MediaCommand::Next)
    );
    assert_eq!(
        command_of(&MediaControlEvent::Previous),
        Some(MediaCommand::Previous)
    );
    // Everything else is the OS's business, not velo's: no seek, no volume,
    // no quit from a media key that the keyboard never had.
    assert_eq!(command_of(&MediaControlEvent::Stop), None);
    assert_eq!(command_of(&MediaControlEvent::Quit), None);
    assert_eq!(
        command_of(&MediaControlEvent::OpenUri(String::from("x"))),
        None
    );

    // And each command is the action its keyboard key answers — the same
    // actions `on_key` returns for space, `n` and `p`.
    assert_eq!(action_of(MediaCommand::PlayPause), Action::TogglePause);
    assert_eq!(action_of(MediaCommand::Next), Action::Next);
    assert_eq!(action_of(MediaCommand::Previous), Action::Previous);
}

#[test]
fn a_media_command_reaches_the_same_action_its_keyboard_key_answers() {
    use velo::ui::crossterm::event::KeyCode;
    let key = velo::ui::crossterm::event::KeyEvent::from;
    let mut app = App::new(Vec::new());
    // The keys themselves, as the keymap answers them today.
    assert_eq!(app.on_key(key(KeyCode::Char(' '))), Action::TogglePause);
    assert_eq!(app.on_key(key(KeyCode::Char('n'))), Action::Next);
    assert_eq!(app.on_key(key(KeyCode::Char('p'))), Action::Previous);
    // The media keys take the same route.
    assert_eq!(
        app.media_command(MediaCommand::PlayPause),
        Action::TogglePause
    );
    assert_eq!(app.media_command(MediaCommand::Next), Action::Next);
    assert_eq!(app.media_command(MediaCommand::Previous), Action::Previous);
}

/// S6: commands the OS sent ride a channel the tick drains, so a press
/// lands without a key press and a silent session costs the tick nothing.
/// Next and previous with nothing playing change nothing — the same
/// contract the keyboard's `n` and `p` keep.
#[test]
fn the_tick_drains_the_commands_the_os_sent() {
    let (mut app, commands, _updates, _init) = app_with_media();
    commands.send(MediaCommand::Next).unwrap();
    commands.send(MediaCommand::Previous).unwrap();
    commands.send(MediaCommand::PlayPause).unwrap();
    app.tick();
    // Nothing played, so nothing changed and nothing was said — the keys'
    // own answer with an empty queue.
    assert_eq!(app.status(), "");
    assert_eq!(app.recently_played().len(), 0);
}

// ---- the publishing (S5) ----------------------------------------------------

#[test]
fn a_playing_file_publishes_its_title_and_artist() {
    let (mut app, _commands, updates, _init) = app_with_media();
    app.show_playing_for_tests(
        Entry::File(std::path::PathBuf::from("/music/song.flac")),
        Duration::ZERO,
        Some(Duration::from_secs(180)),
        None,
    );
    app.tick();
    // A cold file the index never saw: its file name is its title, and no
    // artist is invented for it.
    let state = updates.try_recv().unwrap_or_else(|_| {
        panic!("the first tick publishes what is playing");
    });
    assert!(state.playing);
    assert_eq!(
        state.now,
        Some(NowPlaying {
            title: String::from("song.flac"),
            artist: None,
            duration: None
        })
    );
}

#[test]
fn a_live_station_publishes_the_icy_title_or_the_station_name() {
    let (mut app, _commands, updates, _init) = app_with_media();
    app.show_playing_for_tests(
        Entry::Station {
            url: String::from("https://radio.example/nova"),
            name: String::from("Radio Nova"),
        },
        Duration::ZERO,
        None,
        Some(String::from("Artist - Song")),
    );
    app.tick();
    let state = updates.try_recv().unwrap();
    assert_eq!(
        state.now,
        Some(NowPlaying {
            title: String::from("Artist - Song"),
            artist: None,
            duration: None,
        })
    );

    // A station that announces nothing: its own name is what the OS shows.
    let (mut app, _commands, updates, _init) = app_with_media();
    app.show_playing_for_tests(
        Entry::Station {
            url: String::from("https://radio.example/nova"),
            name: String::from("Radio Nova"),
        },
        Duration::ZERO,
        None,
        None,
    );
    app.tick();
    let state = updates.try_recv().unwrap();
    assert_eq!(
        state.now,
        Some(NowPlaying {
            title: String::from("Radio Nova"),
            artist: None,
            duration: None,
        })
    );
}

#[test]
fn nothing_playing_publishes_nothing() {
    let (mut app, _commands, updates, _init) = app_with_media();
    app.tick();
    let state = updates.try_recv().unwrap();
    assert!(!state.playing);
    assert_eq!(state.now, None);
}

#[test]
fn an_unchanged_session_is_published_once() {
    let (mut app, _commands, updates, _init) = app_with_media();
    app.tick();
    app.tick();
    app.tick();
    // One message for the state, nothing for the two ticks that changed
    // nothing: a media session fed a hundred identical answers a minute
    // would be noise, not information.
    updates.try_recv().unwrap();
    assert!(
        updates.try_recv().is_err(),
        "an unchanged session publishes nothing more"
    );
}

#[test]
fn a_command_that_cannot_act_publishes_nothing_new() {
    let (mut app, commands, updates, _init) = app_with_media();
    app.show_playing_for_tests(
        Entry::File(std::path::PathBuf::from("/music/song.flac")),
        Duration::ZERO,
        None,
        None,
    );
    app.tick();
    let first = updates.try_recv().unwrap();
    assert!(first.playing);
    commands.send(MediaCommand::PlayPause).unwrap();
    app.tick();
    // Pause without a player is the space key's own answer: nothing moves,
    // and what did not change is published again by no one.
    assert!(
        updates.try_recv().is_err(),
        "an action that could not act publishes nothing new"
    );
}

// ---- the best-effort contract (S6) ------------------------------------------

#[test]
fn a_media_session_that_refuses_costs_one_status_line_at_most() {
    let (mut app, _commands, _updates, init) = app_with_media();
    init.send(Err(String::from("no session bus"))).unwrap();
    app.tick();
    assert!(
        app.status().contains("no session bus"),
        "the refusal is said once, got: {:?}",
        app.status()
    );
    // And only once: the next tick has nothing new to say.
    app.tick();
    assert_eq!(app.status().matches("no session bus").count(), 1);
}
