//! The cliamp section in the radio view (issue #48): the section row at
//! the top, the channels with their counts, the songs of one channel, and
//! the wiring that loads them.
//!
//! No network and no terminal where a state assertion suffices: the view
//! is built from the structs the client parses, the data wiring runs over
//! a canned, counting transport (the `radio::client::Transport` seam) on
//! its worker thread, and the shell tests drive the [`App`] the event loop
//! drives — the play action returned, never applied, because applying it
//! would open an audio device.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use velo::audio::AudioError;
use velo::audio::output::{Entry, PathResolver, TrackFailure};
use velo::radio::cliamp::{Channel, Listeners, Song};
use velo::radio::client::{Error, Transport};
use velo::ui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use velo::ui::radio::{Place, RadioOutcome, RadioView};
use velo::ui::radio_data::{RadioData, RadioWire};
use velo::ui::theme::Theme;
use velo::ui::{Action, App, PlayState};

// ---- the canned, counting transport -----------------------------------------

/// A transport with one canned body per endpoint and a call log. The
/// cliamp endpoints answer by URL; every other URL — the directory's —
/// answers as an empty index, so the tests can also pin that the directory
/// is untouched.
struct Canned {
    calls: Mutex<Vec<String>>,
    stations: String,
    statistics: String,
    tracks: String,
    fail_stations: bool,
    fail_statistics: bool,
    fail_tracks_once: std::sync::atomic::AtomicBool,
}

impl Canned {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            stations: String::from(r#"{"stations":[]}"#),
            statistics: String::from(r#"{"stations":{}}"#),
            tracks: String::from(r#"{"station":"lofi","name":"","count":0,"tracks":[]}"#),
            fail_stations: false,
            fail_statistics: false,
            fail_tracks_once: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn stations(mut self, body: &str) -> Self {
        body.clone_into(&mut self.stations);
        self
    }

    fn statistics(mut self, body: &str) -> Self {
        body.clone_into(&mut self.statistics);
        self
    }

    fn tracks(mut self, body: &str) -> Self {
        body.clone_into(&mut self.tracks);
        self
    }

    fn failing_stations(mut self) -> Self {
        self.fail_stations = true;
        self
    }

    fn failing_statistics(mut self) -> Self {
        self.fail_statistics = true;
        self
    }

    /// Loses the first `/tracks` answer only: after that refusal the
    /// endpoint answers, so a test can pin that a re-entry asks again.
    fn failing_tracks_once(mut self) -> Self {
        self.fail_tracks_once = true.into();
        self
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("call log").clone()
    }
}

impl Transport for Canned {
    fn get(&self, url: &str) -> Result<String, velo::radio::client::Error> {
        self.calls.lock().expect("call log").push(url.to_owned());
        if url.contains("radio.cliamp.stream/statistics") {
            if self.fail_statistics {
                return Err(Error::Unreachable {
                    detail: "connection refused".to_owned(),
                });
            }
            return Ok(self.statistics.clone());
        }
        if url.contains("radio.cliamp.stream/stations") {
            if self.fail_stations {
                return Err(Error::Unreachable {
                    detail: "connection refused".to_owned(),
                });
            }
            return Ok(self.stations.clone());
        }
        if url.contains("radio.cliamp.stream/") && url.contains("/tracks") {
            if self
                .fail_tracks_once
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return Err(Error::Unreachable {
                    detail: "connection refused".to_owned(),
                });
            }
            return Ok(self.tracks.clone());
        }
        // The directory: an empty index, so a test can pin that a cliamp
        // step never asked it anything.
        Ok(String::from("[]"))
    }
}

/// Shares the canned transport with the clients: the worker threads fetch
/// through it while the test reads back the same call log.
struct Shared(Arc<Canned>);

impl Transport for Shared {
    fn get(&self, url: &str) -> Result<String, velo::radio::client::Error> {
        self.0.get(url)
    }
}

/// The wired loader over a canned transport, with no store file and no
/// country detected.
fn wired(canned: Arc<Canned>) -> RadioData<Shared> {
    RadioData::new(Some(Shared(canned)), None, None)
}

/// Waits for the wiring to go idle with `done` holding, polling the way
/// the event loop does (the `ui_radio_data.rs` helper, copied).
fn settle<T>(
    data: &mut RadioData<T>,
    view: &mut RadioView,
    canned: &Canned,
    done: impl Fn(&RadioView, &Canned) -> bool,
) where
    T: Transport + Send + Sync + 'static,
{
    for _ in 0..400 {
        data.poll(view);
        if !data.loading() && done(view, canned) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the wiring never answered, calls: {:?}", canned.calls());
}

fn press(view: &mut RadioView, code: KeyCode) -> RadioOutcome {
    view.key(KeyEvent::from(code))
}

fn ctrl_r(view: &mut RadioView) -> RadioOutcome {
    view.key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL))
}

// ---- fixtures ---------------------------------------------------------------

/// Two channels as `/stations` lists them, each with its live stream.
fn channels() -> Vec<Channel> {
    vec![
        Channel {
            id: String::from("lofi"),
            name: String::from("LoFi Beats"),
            tracks: 120,
            stream: String::from("https://radio.cliamp.stream/lofi/stream"),
        },
        Channel {
            id: String::from("jazz"),
            name: String::from("Jazz Cafe"),
            tracks: 12,
            stream: String::from("https://radio.cliamp.stream/jazz/stream"),
        },
    ]
}

/// The listeners `/statistics` reports for the fixture's channels.
fn listeners() -> Vec<Listeners> {
    vec![
        Listeners {
            id: String::from("lofi"),
            active: 3,
        },
        Listeners {
            id: String::from("jazz"),
            active: 0,
        },
    ]
}

/// Two songs as `/<id>/tracks` lists them, in the service's order.
fn songs() -> Vec<Song> {
    vec![
        Song {
            id: String::from("t1"),
            title: String::from("Dawn"),
            artist: String::from("Tide"),
            album: String::from("Harbor"),
            filename: String::from("t1.mp3"),
            url: String::from("https://radio.cliamp.stream/lofi/tracks/t1"),
        },
        Song {
            id: String::from("t2"),
            title: String::from("Dusk"),
            artist: String::from("Sable"),
            album: String::from("Views"),
            filename: String::from("t2.mp3"),
            url: String::from("https://radio.cliamp.stream/lofi/tracks/t2"),
        },
    ]
}

/// A bare view over an empty store: the cliamp section shown (S1), the
/// directory's indexes empty.
fn bare() -> RadioView {
    RadioView::new(
        velo::radio::store::RadioStore::default(),
        Vec::new(),
        Vec::new(),
        None,
    )
}

/// A view with the cliamp section shown and its channels loaded, beside a
/// loaded directory: one country in the index and the user's own country
/// known, so the tests can pin what the cliamp rows change and what they
/// leave alone.
fn view() -> RadioView {
    let mut view = RadioView::new(
        velo::radio::store::RadioStore::default(),
        vec![velo::radio::client::Country {
            name: String::from("Norway"),
            iso_3166_1: String::from("NO"),
            stationcount: 12,
        }],
        Vec::new(),
        Some(String::from("NO")),
    );
    view.set_cliamp_channels(channels());
    view.set_cliamp_listeners(listeners());
    view
}

/// The place of the fixture's first channel.
fn channel_place() -> Place {
    Place::CliampChannel {
        id: String::from("lofi"),
        name: String::from("LoFi Beats"),
    }
}

fn screen(view: &RadioView) -> String {
    use velo::ui::ratatui::Terminal;
    use velo::ui::ratatui::backend::TestBackend;
    use velo::ui::ratatui::buffer::{Buffer, Cell};

    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("a test terminal");
    terminal
        .draw(|frame| view.render(frame, frame.area(), &Theme::default()))
        .expect("the frame draws");
    let buffer: &Buffer = terminal.backend().buffer();
    let content: String = buffer.content().iter().map(Cell::symbol).collect();
    content
        .chars()
        .collect::<Vec<_>>()
        .chunks(80)
        .map(|line| line.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// One key press of the shell, its action applied — except a play, which
/// would open an audio device: that one is returned (the `ui_radio.rs`
/// helper, copied).
fn shell(app: &mut App<'_>, code: KeyCode) -> Action {
    let action = app.on_key(KeyEvent::from(code));
    if !matches!(action, Action::PlayQueue { .. }) {
        app.apply(action);
    }
    action
}

/// The whole interface as one screen, the way the user sees it (the
/// `ui_radio.rs` helper, copied).
fn app_screen(app: &App) -> String {
    use velo::ui::ratatui::Terminal;
    use velo::ui::ratatui::backend::TestBackend;
    use velo::ui::ratatui::buffer::{Buffer, Cell};

    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("a test terminal");
    terminal
        .draw(|frame| app.draw(frame))
        .expect("the frame draws");
    let buffer: &Buffer = terminal.backend().buffer();
    let screen: String = buffer.content().iter().map(Cell::symbol).collect();
    let characters: Vec<char> = screen.chars().collect();
    characters
        .chunks(80)
        .map(|line| line.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

// ---- S1: the section at the top ---------------------------------------------

#[test]
fn the_cliamp_section_is_first_at_the_top() {
    let view = view();
    let labels = view.labels();
    assert_eq!(
        labels.first().map(String::as_str),
        Some("cliamp radio"),
        "cliamp radio is the first section, got: {labels:?}"
    );
    let countries = labels
        .iter()
        .position(|label| label == "Browse all countries")
        .expect("the directory's sections keep their place");
    assert!(
        countries > 0,
        "the directory's sections follow cliamp's, got: {labels:?}"
    );
}

#[test]
fn the_cliamp_section_is_numbered_1_and_above_the_directory() {
    let screen = screen(&view());
    let cliamp = screen
        .lines()
        .position(|line| line.contains("1. cliamp radio"))
        .expect("the section's row is numbered 1, got:\n{screen}");
    let countries = screen
        .lines()
        .position(|line| line.contains("2. Browse all countries"))
        .expect("the first directory section is numbered 2, got:\n{screen}");
    assert!(
        cliamp < countries,
        "cliamp radio stands above the directory's sections"
    );
}

#[test]
fn an_empty_directory_still_shows_the_cliamp_section() {
    // The section exists to be entered and loaded: it shows before its
    // channels arrive, the way the browse rows do (T6b).
    let view = bare();
    assert_eq!(
        view.labels(),
        ["cliamp radio"],
        "the section shows before its channels load, got: {:?}",
        view.labels()
    );
}

#[test]
fn cliamp_false_hides_the_section_and_leaves_the_directory_alone() {
    let mut view = view();
    view.set_cliamp_enabled(false);
    let labels = view.labels();
    assert!(
        !labels.iter().any(|label| label.contains("cliamp")),
        "the section is hidden entirely, got: {labels:?}"
    );
    assert!(
        labels.contains(&"Browse all countries".to_owned()),
        "the directory's sections are untouched, got: {labels:?}"
    );
    assert!(
        labels.contains(&"Norway (near you)".to_owned()),
        "the rest of the top is untouched, got: {labels:?}"
    );
}

// ---- S2: the channels and their facts ---------------------------------------

#[test]
fn the_channels_list_counts_its_tracks_and_its_listeners() {
    // The list is where someone scans fifteen channels and picks one, so each
    // row says what it holds and who is on it. The live row inside the channel
    // repeats the count, because that is where `enter` plays (issue #68).
    let mut view = view();
    view.open_place(Place::Cliamp);
    assert_eq!(
        view.labels(),
        ["LoFi Beats", "Jazz Cafe"],
        "the channels list in the service's order, got: {:?}",
        view.labels()
    );
    let rows = view.visible_rows();
    assert_eq!(
        rows[0].fact, "120 tracks · 3 listening now",
        "got: {:?}",
        rows[0]
    );
    assert_eq!(
        rows[1].fact, "12 tracks · 0 listening now",
        "got: {:?}",
        rows[1]
    );
}

#[test]
fn without_statistics_the_counts_drop_and_the_channels_stay() {
    // `/statistics` failed: the `●` counts drop, the channels still list
    // (S4). The data wiring shows the same below; here the view's rule:
    // no listeners known, no listeners shown.
    let mut view = bare();
    view.set_cliamp_channels(channels());
    view.open_place(Place::Cliamp);
    let rows = view.visible_rows();
    assert_eq!(rows[0].fact, "120 tracks", "got: {:?}", rows[0]);
    assert_eq!(rows[1].fact, "12 tracks", "got: {:?}", rows[1]);
}

#[test]
fn a_channel_row_opens_the_songs_place() {
    let mut view = view();
    view.open_place(Place::Cliamp);
    assert_eq!(
        press(&mut view, KeyCode::Enter),
        RadioOutcome::Handled,
        "opening a channel navigates; the songs come from the wiring"
    );
    assert_eq!(
        view.place(),
        &channel_place(),
        "enter opens the channel's songs place"
    );
}

// ---- issue #68: the live row -----------------------------------------------

#[test]
fn the_channel_place_leads_with_the_live_row_above_its_songs() {
    let mut view = bare();
    view.set_cliamp_channels(channels());
    view.open_place(channel_place());
    view.set_cliamp_songs(channel_place(), songs());
    assert_eq!(
        view.labels(),
        ["LoFi Beats — live", "Dawn", "Dusk"],
        "the live row is the first row of the channel, got: {:?}",
        view.labels()
    );
    // The live row takes no number, so the songs keep counting from one,
    // and the header still counts the songs alone.
    let screen = screen(&view);
    assert!(
        screen.contains("LoFi Beats — live"),
        "the live row names the channel, got:\n{screen}"
    );
    assert!(
        screen.contains("1. Dawn"),
        "the first song is numbered one, got:\n{screen}"
    );
    assert!(
        screen.contains("[2 songs]"),
        "the header counts the songs, not the live row (which would make it three), got:\n{screen}"
    );
}

#[test]
fn the_live_row_carries_the_listener_count() {
    // The count moved here from the channel row (issue #68): it belongs
    // to the row that plays the channel live.
    let mut view = bare();
    view.set_cliamp_channels(channels());
    view.set_cliamp_listeners(listeners());
    view.open_place(channel_place());
    let fact = &view.visible_rows()[0].fact;
    assert_eq!(
        fact, "● 3 listening now",
        "the listeners stand on the live row, got: {fact:?}"
    );
}

#[test]
fn without_statistics_the_live_row_stands_without_a_count() {
    // `/statistics` failing must not break the row: the count simply is
    // not there (S4).
    let mut view = bare();
    view.set_cliamp_channels(channels());
    view.open_place(channel_place());
    let row = &view.visible_rows()[0];
    assert_eq!(
        row.label, "LoFi Beats — live",
        "the row stands whatever the statistics did"
    );
    assert_eq!(row.fact, "", "no listeners known, no count shown");
}

#[test]
fn enter_on_the_live_row_asks_to_play_the_stream_not_the_first_song() {
    let mut view = bare();
    view.set_cliamp_channels(channels());
    view.open_place(channel_place());
    view.set_cliamp_songs(channel_place(), songs());
    let outcome = press(&mut view, KeyCode::Enter);
    assert_eq!(
        outcome,
        RadioOutcome::Play {
            url: String::from("https://radio.cliamp.stream/lofi/stream"),
            name: String::from("LoFi Beats"),
            configured: false,
        },
        "enter asks for the channel's own live stream, unexpanded — the
         station path the shell already has — never the first song"
    );
    assert_eq!(view.place(), &channel_place(), "playing does not navigate");
}

#[test]
fn a_channel_without_a_stream_has_no_live_row() {
    let mut without = channels();
    without[0].stream = String::new();
    let mut view = bare();
    view.set_cliamp_channels(without);
    view.open_place(channel_place());
    view.set_cliamp_songs(channel_place(), songs());
    assert_eq!(
        view.labels(),
        ["Dawn", "Dusk"],
        "no stream, no live row: the songs are all the channel plays, got: {:?}",
        view.labels()
    );
}

#[test]
fn the_playing_stream_marks_the_live_row() {
    let mut view = bare();
    view.set_cliamp_channels(channels());
    view.open_place(channel_place());
    view.set_cliamp_songs(channel_place(), songs());
    view.set_playing(Some((
        String::from("https://radio.cliamp.stream/lofi/stream"),
        PlayState::Playing,
    )));
    let screen = screen(&view);
    assert!(
        screen.contains("▶ LoFi Beats — live"),
        "the live row carries the playing marker like any station row, got:\n{screen}"
    );
    assert!(
        !screen.contains("▶ 1. Dawn"),
        "the songs carry no playing marker, got:\n{screen}"
    );
}

// ---- S2: the songs of one channel -------------------------------------------

#[test]
fn a_channel_lists_its_songs_with_artist_album_notes() {
    let mut view = bare();
    view.open_place(channel_place());
    view.set_cliamp_songs(channel_place(), songs());
    let rows = view.visible_rows();
    assert_eq!(rows.len(), 2, "the songs list in the service's order");
    assert_eq!(rows[0].label, "Dawn", "the title is the label");
    assert_eq!(
        rows[0].fact, "Tide · Harbor",
        "artist · album is the dim note, got: {:?}",
        rows[0]
    );
    assert_eq!(rows[1].label, "Dusk");
    assert_eq!(rows[1].fact, "Sable · Views", "got: {:?}", rows[1]);
}

#[test]
fn the_songs_render_only_in_their_own_place() {
    let mut view = view();
    view.open_place(channel_place());
    view.set_cliamp_songs(channel_place(), songs());
    view.open_place(Place::Cliamp);
    assert!(
        !view.labels().iter().any(|label| label == "Dawn"),
        "a stale song list never renders in a sibling place, got: {:?}",
        view.labels()
    );
    assert_eq!(
        view.labels(),
        ["LoFi Beats", "Jazz Cafe"],
        "the channels are still there"
    );
}

// ---- S3: the songs play and queue as files ----------------------------------

#[test]
fn enter_on_a_song_asks_to_queue_the_songs_and_play_from_it() {
    let mut view = bare();
    view.open_place(channel_place());
    view.set_cliamp_songs(channel_place(), songs());
    press(&mut view, KeyCode::Char('j')); // onto the second song
    let outcome = press(&mut view, KeyCode::Enter);
    assert_eq!(
        outcome,
        RadioOutcome::PlaySongs {
            songs: songs(),
            from: 1,
        },
        "enter hands the channel's songs and the selected one to the shell"
    );
    assert_eq!(view.place(), &channel_place(), "playing does not navigate");
}

#[test]
fn a_on_a_song_asks_to_queue_that_one_song() {
    let mut view = bare();
    view.open_place(channel_place());
    view.set_cliamp_songs(channel_place(), songs());
    let outcome = press(&mut view, KeyCode::Char('a'));
    assert_eq!(
        outcome,
        RadioOutcome::QueueSong {
            url: String::from("https://radio.cliamp.stream/lofi/tracks/t1"),
            title: String::from("Dawn"),
        },
        "a hands the song to the shell to queue, not to play"
    );
}

// ---- S3: the shell queues file entries, never stations ----------------------

#[test]
fn the_shell_queues_a_song_as_a_file_entry_whose_path_is_the_url() {
    let mut app = App::new(Vec::new());
    shell(&mut app, KeyCode::Char('R')); // the radio opens over its wiring
    app.show_radio_cliamp_songs(channel_place(), songs());

    let action = shell(&mut app, KeyCode::Char('a'));
    assert_eq!(action, Action::None, "a queues without starting");
    assert_eq!(
        app.queue(),
        [Entry::File(PathBuf::from(
            "https://radio.cliamp.stream/lofi/tracks/t1"
        ))],
        "the song joined the queue as a file whose path is its URL"
    );
}

#[test]
fn the_shell_plays_from_a_song_through_the_queue_as_files() {
    let mut app = App::new(Vec::new());
    shell(&mut app, KeyCode::Char('R'));
    app.show_radio_cliamp_songs(channel_place(), songs());
    shell(&mut app, KeyCode::Char('j')); // onto the second song

    let action = shell(&mut app, KeyCode::Enter);
    assert_eq!(
        action,
        Action::PlayQueue { from: 1 },
        "the player was asked to start at the selected song"
    );
    let expected: Vec<Entry> = songs()
        .into_iter()
        .map(|song| Entry::File(PathBuf::from(song.url)))
        .collect();
    assert_eq!(
        app.queue(),
        expected.as_slice(),
        "the channel's songs joined the queue as file entries, in order"
    );
    assert!(
        !app.queue()
            .iter()
            .any(|entry| matches!(entry, Entry::Station { .. })),
        "a song is never a live station (S3)"
    );
}

// ---- S4: two requests, and Ctrl+R asks again --------------------------------

#[test]
fn opening_the_section_makes_exactly_two_requests() {
    let canned = Arc::new(
        Canned::new()
            .stations(STATIONS_FIXTURE)
            .statistics(STATISTICS_FIXTURE),
    );
    let mut data = wired(Arc::clone(&canned));
    let mut view = data.view();

    press(&mut view, KeyCode::Enter); // the cliamp section
    settle(&mut data, &mut view, &canned, |view, canned| {
        !view.labels().is_empty() && canned.calls().len() == 2
    });

    let calls = canned.calls();
    assert!(
        calls[0].contains("/stations") && calls[1].contains("/statistics"),
        "the section asks for the channels and then the listeners, got: {calls:?}"
    );
    assert_eq!(
        view.labels(),
        ["LoFi Beats", "Jazz Cafe"],
        "the channels land in the view"
    );

    // Leaving and re-entering stays cached: no more requests.
    press(&mut view, KeyCode::Esc);
    press(&mut view, KeyCode::Enter);
    for _ in 0..5 {
        data.poll(&mut view);
    }
    assert_eq!(
        canned.calls().len(),
        2,
        "re-entering a loaded section re-uses the answer, got: {:?}",
        canned.calls()
    );
}

#[test]
fn ctrl_r_refetches_both_requests() {
    let canned = Arc::new(
        Canned::new()
            .stations(STATIONS_FIXTURE)
            .statistics(STATISTICS_FIXTURE)
            .tracks(TRACKS_FIXTURE),
    );
    let mut data = wired(Arc::clone(&canned));
    let mut view = data.view();

    press(&mut view, KeyCode::Enter); // the cliamp section
    settle(&mut data, &mut view, &canned, |_view, canned| {
        canned.calls().len() == 2
    });
    assert_eq!(canned.calls().len(), 2, "the section loaded once");

    ctrl_r(&mut view);
    settle(&mut data, &mut view, &canned, |_view, canned| {
        canned.calls().len() == 4
    });
    let calls = canned.calls();
    assert_eq!(calls.len(), 4, "two more requests, got: {calls:?}");
    assert!(
        calls[2].contains("/stations") && calls[3].contains("/statistics"),
        "ctrl-r re-asks the channels and then the listeners, got: {calls:?}"
    );
}

// ---- S4: the two failures ---------------------------------------------------

#[test]
fn a_failing_stations_leaves_the_section_present_but_empty() {
    let canned = Arc::new(Canned::new().failing_stations());
    let mut data = wired(Arc::clone(&canned));
    let mut view = data.view();

    assert_eq!(
        view.labels().first().map(String::as_str),
        Some("cliamp radio"),
        "the section is present before anything loads"
    );
    press(&mut view, KeyCode::Enter); // the cliamp section
    // Both requests go out even when `/stations` fails: the failure marks
    // its own loaded flag, so the wiring asks `/statistics` anyway (S4),
    // and each failure is one status line.
    settle(&mut data, &mut view, &canned, |_view, canned| {
        canned.calls().len() == 2
    });
    std::thread::sleep(Duration::from_millis(20));
    data.poll(&mut view);

    assert!(
        view.labels().is_empty(),
        "the failed section lists nothing, got: {:?}",
        view.labels()
    );
    let message = data.take_message().expect("the failure is said");
    assert_eq!(
        message, "velo: radio.cliamp.stream could not be reached: connection refused",
        "one status line names the reason"
    );
    assert_eq!(data.take_message(), None, "the failure is said once");
}

#[test]
fn a_failing_statistics_drops_only_the_counts_and_says_so_once() {
    let canned = Arc::new(
        Canned::new()
            .stations(STATIONS_FIXTURE)
            .failing_statistics(),
    );
    let mut data = wired(Arc::clone(&canned));
    let mut view = data.view();

    press(&mut view, KeyCode::Enter); // the cliamp section
    settle(&mut data, &mut view, &canned, |_view, canned| {
        canned.calls().len() == 2
    });
    std::thread::sleep(Duration::from_millis(20));
    data.poll(&mut view);

    assert_eq!(
        view.labels(),
        ["LoFi Beats", "Jazz Cafe"],
        "the channels still list (S4)"
    );
    let rows = view.visible_rows();
    assert_eq!(
        rows[0].fact, "120 tracks",
        "only the ● count dropped, got: {:?}",
        rows[0]
    );
    let message = data.take_message().expect("the failure is said");
    assert!(
        message.contains("radio.cliamp.stream"),
        "the status line names the service, got: {message}"
    );
    assert_eq!(data.take_message(), None, "said once");
}

// ---- issue #68: the live row plays through the station path -----------------

#[test]
fn the_shell_plays_the_channel_stream_as_a_station_from_the_live_row() {
    let canned = Arc::new(
        Canned::new()
            .stations(STATIONS_FIXTURE)
            .statistics(STATISTICS_FIXTURE)
            .tracks(TRACKS_FIXTURE),
    );
    let mut app = App::new(Vec::new());
    app.set_radio_wire(Box::new(wired(Arc::clone(&canned))));
    shell(&mut app, KeyCode::Char('R')); // the radio opens over its wiring
    shell(&mut app, KeyCode::Enter); // into the cliamp section
    // The channels come back on the wiring's worker; the ticks bring them
    // into the view, the way the event loop would.
    for _ in 0..400 {
        app.tick();
        if app_screen(&app).contains("LoFi Beats") {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    shell(&mut app, KeyCode::Enter); // into the first channel's place

    let action = shell(&mut app, KeyCode::Enter); // the live row
    assert_eq!(
        action,
        Action::PlayQueue { from: 0 },
        "the player was asked to start what the queue just took"
    );
    assert_eq!(
        app.queue(),
        [Entry::Station {
            url: String::from("https://radio.cliamp.stream/lofi/stream"),
            name: String::from("LoFi Beats"),
        }],
        "the channel's live stream joined the queue as the station it is —
         never its first song as a file entry"
    );
}

// ---- the songs over the wiring ----------------------------------------------

#[test]
fn entering_a_channel_loads_its_songs_through_the_wiring() {
    let canned = Arc::new(
        Canned::new()
            .stations(STATIONS_FIXTURE)
            .statistics(STATISTICS_FIXTURE)
            .tracks(TRACKS_FIXTURE),
    );
    let mut data = wired(Arc::clone(&canned));
    let mut view = data.view();

    press(&mut view, KeyCode::Enter); // the cliamp section
    settle(&mut data, &mut view, &canned, |_view, canned| {
        canned.calls().len() == 2
    });
    press(&mut view, KeyCode::Enter); // the first channel: LoFi Beats
    settle(&mut data, &mut view, &canned, |view, canned| {
        canned
            .calls()
            .last()
            .is_some_and(|url| url.contains("/lofi/tracks"))
            && !view.labels().is_empty()
    });

    let rows = view.visible_rows();
    assert_eq!(
        rows[1].label, "Dawn",
        "the live row stands first (issue #68); the songs follow it"
    );
    assert_eq!(rows[1].fact, "Tide · Harbor", "got: {:?}", rows[1]);
    let calls = canned.calls();
    assert!(
        calls[2].contains("/lofi/tracks"),
        "entering the channel asked for its songs, got: {calls:?}"
    );
    assert!(
        !calls.iter().any(|url| url.contains("/stations/search")),
        "no directory request was made for a cliamp place (S6), got: {calls:?}"
    );
}

// ---- issue #59: a song's row marks while it plays ---------------------------

#[test]
fn a_playing_song_marks_its_row() {
    // A song queued from a channel plays as a file whose path is its URL
    // (cliamp S3); the shell hands that identity to the view the way it
    // hands a station's URL, and the song's row marks with the same rule.
    let mut view = bare();
    view.open_place(channel_place());
    view.set_cliamp_songs(channel_place(), songs());
    view.set_playing(Some((
        String::from("https://radio.cliamp.stream/lofi/tracks/t1"),
        PlayState::Playing,
    )));
    assert!(
        screen(&view).contains("▶ 1. Dawn"),
        "the song that plays marks its row, got:\n{}",
        screen(&view)
    );
}

#[test]
fn a_song_that_is_not_playing_marks_nothing() {
    // The stopped marker is a live stream's (issue #53): a live stream
    // cannot pause, so stopped is the state it is left in. A song is a
    // finite track — one that is not sounding marks nothing.
    let mut view = bare();
    view.open_place(channel_place());
    view.set_cliamp_songs(channel_place(), songs());
    view.set_playing(Some((
        String::from("https://radio.cliamp.stream/lofi/tracks/t1"),
        PlayState::Stopped,
    )));
    let quiet = screen(&view);
    assert!(
        !quiet.contains("▶ 1. Dawn") && !quiet.contains("■ 1. Dawn"),
        "a song that is not playing marks nothing, got:\n{quiet}"
    );
}

#[test]
fn the_shell_hands_the_playing_song_to_the_radio_view() {
    // The shell-level rule (issue #59): radio_poll tells the view what
    // plays, whatever kind of entry it is — a song reaches the view too,
    // where before only a station did, and the tick marks the row.
    let mut app = App::new(Vec::new());
    shell(&mut app, KeyCode::Char('R')); // the radio opens over its wiring
    app.show_radio_cliamp_songs(channel_place(), songs());
    app.show_playing_for_tests(
        Entry::File(PathBuf::from("https://radio.cliamp.stream/lofi/tracks/t1")),
        Duration::ZERO,
        None,
        None,
    );

    app.tick();
    assert!(
        app_screen(&app).contains("▶ 1. Dawn"),
        "the playing song's row marks through the shell, got:\n{}",
        app_screen(&app)
    );
}

// ---- issue #60: a long fetch says so ----------------------------------------

/// A resolver whose every entry is still a download (issue #60's fixture):
/// resolution passes through, and the cache question always says no.
struct NeverCached;

impl PathResolver for NeverCached {
    fn resolve(&self, path: &Path) -> Result<PathBuf, AudioError> {
        Ok(path.to_path_buf())
    }

    fn cached(&self, _path: &Path) -> bool {
        false
    }
}

/// A resolver whose every entry is already on disk: a cache hit, instant.
struct AlwaysCached;

impl PathResolver for AlwaysCached {
    fn resolve(&self, path: &Path) -> Result<PathBuf, AudioError> {
        Ok(path.to_path_buf())
    }
}

/// A queue standing on one remote song, sounding (position past zero), with
/// the transport fixture's fetching cell set by the caller.
fn fetching_app(fetching: Option<usize>, position: Duration) -> App<'static> {
    let mut app = App::new(Vec::new());
    app.show_playing_for_tests(
        Entry::File(PathBuf::from("https://radio.cliamp.stream/lofi/tracks/t1")),
        position,
        None,
        None,
    );
    app.set_fetching_for_tests(fetching);
    app
}

#[test]
fn an_uncached_remote_entry_says_it_is_fetching() {
    let mut app = fetching_app(Some(0), Duration::ZERO);
    app.set_resolver(Some(Arc::new(NeverCached) as Arc<dyn PathResolver>));

    app.tick();
    assert_eq!(
        app.status(),
        "⟳ fetching t1…",
        "the queue is downloading the song, and the status line says so"
    );
}

#[test]
fn the_fetching_line_clears_when_the_song_starts() {
    let mut app = fetching_app(Some(0), Duration::from_secs(1));
    app.set_resolver(Some(Arc::new(NeverCached) as Arc<dyn PathResolver>));
    app.tick();
    assert_eq!(app.status(), "⟳ fetching t1…", "the fetch is under way");

    // The fetch is over (the transport's cell is back to none) and the
    // samples are being heard.
    app.set_fetching_for_tests(None);
    app.tick();
    assert_eq!(
        app.status(),
        "",
        "the song sounds: the fetching line has said its piece"
    );
}

#[test]
fn a_cached_entry_says_nothing() {
    let mut app = fetching_app(Some(0), Duration::ZERO);
    app.set_resolver(Some(Arc::new(AlwaysCached) as Arc<dyn PathResolver>));

    app.tick();
    assert_eq!(
        app.status(),
        "",
        "a cache hit is instant: no fetching line, no made-up wait"
    );
}

#[test]
fn a_failed_fetch_keeps_its_report_and_clears_the_line() {
    let mut app = fetching_app(Some(0), Duration::from_secs(1));
    app.set_resolver(Some(Arc::new(NeverCached) as Arc<dyn PathResolver>));
    app.tick();
    assert_eq!(app.status(), "⟳ fetching t1…", "the fetch is under way");

    // The download failed: the producer cleared its cell and announced the
    // skip (the announcement's own one-line report).
    app.set_fetching_for_tests(None);
    app.announce_failure(&TrackFailure {
        label: String::from("t1"),
        message: String::from("the server could not be reached"),
    });
    app.tick();
    assert_eq!(
        app.status(),
        "velo: skipped t1: the server could not be reached",
        "the fetching line is gone and the failure's report stands"
    );
}

// ---- issue #60's second half: the event loop never waits -------------------

/// The lock a test holds over the worker's download (issue #60's second
/// half): the resolver waits for it, so the interface's own turn ends long
/// before the "fetch" does.
type Gate = Arc<(Mutex<bool>, Condvar)>;

fn gate() -> Gate {
    Arc::new((Mutex::new(false), Condvar::new()))
}

fn open_gate(gate: &Gate) {
    let (lock, signal) = &**gate;
    *lock.lock().expect("the gate opens") = true;
    signal.notify_all();
}

/// Turns the loop until the worker's answer has been taken (issue #60): the
/// tick polls the slot and never waits on it, so the start lands on the
/// first tick that finds the answer there — bounded, so a broken handoff
/// fails its test instead of hanging it.
fn ticks_until_started(app: &mut App<'_>) {
    for _ in 0..100 {
        app.tick();
        if !app.status().starts_with("⟳ fetching") {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("the worker's answer never reached the loop's ticks");
}

/// A resolver the worker waits on: every remote entry's "download" holds
/// until the test opens the gate, then answers — with the path of a file
/// that does not exist when the fetch succeeded (the start attempt that
/// follows is what the test observes, and that attempt fails before any
/// audio device is touched), or with its failure reason.
struct Gated {
    gate: Gate,
    /// The path a released fetch answers with.
    resolved: PathBuf,
    /// Some(reason) makes the released fetch fail with it instead.
    fail: Option<String>,
}

impl Gated {
    /// A gate held shut, resolving (once released) to `resolved`.
    fn at(gate: Gate, resolved: impl Into<PathBuf>) -> Self {
        Self {
            gate,
            resolved: resolved.into(),
            fail: None,
        }
    }

    /// A gate held shut, failing (once released) with `reason`.
    fn failing(gate: Gate, reason: &str) -> Self {
        Self {
            gate,
            resolved: PathBuf::new(),
            fail: Some(reason.to_owned()),
        }
    }
}

impl PathResolver for Gated {
    fn resolve(&self, _path: &Path) -> Result<PathBuf, AudioError> {
        let (lock, signal) = &*self.gate;
        let mut open = lock.lock().expect("the gate's lock");
        // The wait has a patience, so a turn that resolved on this thread —
        // the old blocking shape — answers instead of hanging its test.
        let deadline = Instant::now() + Duration::from_secs(2);
        while !*open {
            let now = Instant::now();
            if now >= deadline {
                return Err(AudioError::Unsupported(String::from(
                    "the fetch was still running",
                )));
            }
            let (turned, _) = signal
                .wait_timeout(open, deadline - now)
                .expect("the gate answers");
            open = turned;
        }
        if let Some(reason) = &self.fail {
            return Err(AudioError::Unsupported(reason.clone()));
        }
        Ok(self.resolved.clone())
    }

    fn cached(&self, _path: &Path) -> bool {
        false
    }
}

/// A queue standing on two remote songs, with the resolver the test chose:
/// the shape `enter` on a channel's song makes.
fn two_remote_songs(resolver: Arc<dyn PathResolver>) -> App<'static> {
    let mut app = App::new(Vec::new());
    app.queue_song_for_tests("https://radio.cliamp.stream/lofi/tracks/t1");
    app.queue_song_for_tests("https://radio.cliamp.stream/lofi/tracks/t2");
    app.set_resolver(Some(resolver));
    app
}

#[test]
fn pressing_enter_on_an_uncached_remote_entry_says_so_in_the_same_turn() {
    let gate = gate();
    let mut app = two_remote_songs(
        Arc::new(Gated::at(Arc::clone(&gate), "/nowhere/velo-gate.wav")) as Arc<dyn PathResolver>,
    );

    // The event loop's own turn: the play is applied here, and the turn
    // must end while the "download" still holds — the line says so without
    // a tick, and the queue stands where it was.
    app.apply(Action::PlayQueue { from: 0 });
    assert_eq!(
        app.status(),
        "⟳ fetching t1…",
        "the fetch is on the line in the very turn that asked for it"
    );
    assert_eq!(app.queue().len(), 2, "the queue stands where it was");

    app.tick();
    assert_eq!(
        app.status(),
        "⟳ fetching t1…",
        "the line holds for every tick the download runs"
    );
    open_gate(&gate);
}

#[test]
fn a_finished_fetch_starts_the_stream_on_the_next_tick() {
    let gate = gate();
    let mut app = two_remote_songs(Arc::new(Gated::at(
        Arc::clone(&gate),
        "/nowhere/velo-replay.wav",
    )) as Arc<dyn PathResolver>);

    app.apply(Action::PlayQueue { from: 0 });
    assert_eq!(app.status(), "⟳ fetching t1…", "the wait is under way");

    // The file is on disk now: the tick the event loop turns next runs the
    // same start the request asked for. The resolved path fails to open —
    // before any audio device is touched — and the start attempt's own
    // report replaces the fetching line.
    open_gate(&gate);
    ticks_until_started(&mut app);
    assert!(
        app.status().starts_with("velo: "),
        "the start attempt ran on the tick and answered for itself, got: {:?}",
        app.status()
    );
    let settled = app.status().to_owned();
    app.tick();
    assert_eq!(
        app.status(),
        settled,
        "the start happened once; no second start races in behind it"
    );
}

#[test]
fn a_failed_resolve_says_why_once_and_the_queue_stands() {
    let gate = gate();
    let mut app = two_remote_songs(Arc::new(Gated::failing(
        Arc::clone(&gate),
        "the server could not be reached",
    )) as Arc<dyn PathResolver>);

    app.apply(Action::PlayQueue { from: 0 });
    assert_eq!(app.status(), "⟳ fetching t1…", "the wait is under way");

    open_gate(&gate);
    ticks_until_started(&mut app);
    assert_eq!(
        app.status(),
        "velo: unsupported format: the server could not be reached",
        "one line naming the reason, no stream"
    );
    assert_eq!(
        app.queue().len(),
        2,
        "the queue was not advanced behind the user's back"
    );

    app.tick();
    assert_eq!(
        app.status(),
        "velo: unsupported format: the server could not be reached",
        "said once: no retry, no second line"
    );
}

#[test]
fn a_cached_entry_starts_in_the_same_turn_with_no_fetching_line() {
    // The path `AlwaysCached` resolves to is not a file, so the synchronous
    // start answers with its own report — which is the point: the start ran
    // on this turn, and no fetching line ever stood.
    let mut app = App::new(Vec::new());
    app.queue_song_for_tests("https://radio.cliamp.stream/lofi/tracks/t1");
    app.set_resolver(Some(Arc::new(AlwaysCached) as Arc<dyn PathResolver>));

    app.apply(Action::PlayQueue { from: 0 });
    assert!(
        app.status().starts_with("velo: "),
        "the start ran in this turn, got: {:?}",
        app.status()
    );
    assert!(
        !app.status().starts_with("⟳ fetching"),
        "a cache hit is instant: no fetching line"
    );
    app.tick();
    assert!(
        !app.status().starts_with("⟳ fetching"),
        "no worker's answer arrives later either"
    );
}

#[test]
fn a_local_file_starts_synchronously_no_matter_what_the_resolver_says() {
    // `NeverCached` answers false for everything, and a local path is not
    // a fetch: the start still runs on this thread, in this turn.
    let mut app = App::new(Vec::new());
    app.queue_song_for_tests("/music/dawn.wav");
    app.set_resolver(Some(Arc::new(NeverCached) as Arc<dyn PathResolver>));

    app.apply(Action::PlayQueue { from: 0 });
    assert!(
        app.status().starts_with("velo: "),
        "the start ran in this turn, got: {:?}",
        app.status()
    );
    assert!(
        !app.status().starts_with("⟳ fetching"),
        "a local file is already on disk: no fetching line"
    );
}

#[test]
fn a_second_play_supersedes_the_wait_of_the_first() {
    let gate = gate();
    let mut app = two_remote_songs(Arc::new(Gated::at(
        Arc::clone(&gate),
        "/nowhere/velo-supersede.wav",
    )) as Arc<dyn PathResolver>);

    app.apply(Action::PlayQueue { from: 0 });
    assert_eq!(app.status(), "⟳ fetching t1…", "the first ask waits");

    // A second play while the first runs: the slot holds the newer ask.
    app.apply(Action::PlayQueue { from: 1 });
    assert_eq!(
        app.status(),
        "⟳ fetching t2…",
        "the newer ask holds the slot"
    );

    open_gate(&gate);
    ticks_until_started(&mut app);
    assert!(
        app.status().starts_with("velo: "),
        "exactly one start ran — the second ask's — got: {:?}",
        app.status()
    );
    let settled = app.status().to_owned();
    app.tick();
    assert_eq!(
        app.status(),
        settled,
        "the first ask's start never comes in behind it"
    );
}

#[test]
fn a_skip_of_an_uncached_remote_entry_waits_the_same_way() {
    let gate = gate();
    let mut app = two_remote_songs(
        Arc::new(Gated::at(Arc::clone(&gate), "/nowhere/velo-skip.wav")) as Arc<dyn PathResolver>,
    );

    // A skip is the same rebuild a seek is; no test can hold a `Player`, so
    // the rebuild is driven through its hook, the way a skip would land.
    app.start_track_at_for_tests(1);
    assert_eq!(
        app.status(),
        "⟳ fetching t2…",
        "the skip's rebuild is a wait too"
    );
    assert_eq!(app.queue().len(), 2, "the queue stands where it was");

    open_gate(&gate);
    ticks_until_started(&mut app);
    assert!(
        app.status().starts_with("velo: "),
        "the rebuild ran on the tick and answered for itself, got: {:?}",
        app.status()
    );
}

// ---- the canned bodies ------------------------------------------------------

const STATIONS_FIXTURE: &str = r#"{"stations":[
    {"id":"lofi","name":"LoFi Beats","description":"","genre":"",
     "stream":"https://radio.cliamp.stream/lofi/stream","tracks":120,
     "tracks_url":"https://radio.cliamp.stream/lofi/tracks"},
    {"id":"jazz","name":"Jazz Cafe","description":"","genre":"",
     "stream":"https://radio.cliamp.stream/jazz/stream","tracks":12,
     "tracks_url":"https://radio.cliamp.stream/jazz/tracks"}
]}"#;

const STATISTICS_FIXTURE: &str = r#"{"stations":{
    "lofi":{"active_listeners":3},
    "jazz":{"active_listeners":0}
}}"#;

const TRACKS_FIXTURE: &str = r#"{"station":"lofi","name":"LoFi Beats","count":2,"tracks":[
    {"id":"t1","title":"Dawn","artist":"Tide","album":"Harbor",
     "filename":"t1.mp3","url":"https://radio.cliamp.stream/lofi/tracks/t1"},
    {"id":"t2","title":"Dusk","artist":"Sable","album":"Views",
     "filename":"t2.mp3","url":"https://radio.cliamp.stream/lofi/tracks/t2"}
]}"#;

// ---- issue #70: the playing marker reaches the ancestors ---------------------

/// The playing stream marks the channel's own row inside the cliamp
/// section: the section is where the fifteen channels are scanned, and the
/// one that plays must be findable from the list alone — the same tree
/// rule the live row inside the channel already follows.
#[test]
fn a_playing_channel_marks_its_row_inside_the_section() {
    let mut view = view();
    view.open_place(Place::Cliamp);
    view.set_playing(Some((
        String::from("https://radio.cliamp.stream/lofi/stream"),
        PlayState::Playing,
    )));
    let screen = screen(&view);
    assert!(
        screen.contains("▶ 1. LoFi Beats"),
        "the channel that plays marks its row in the section, got:\n{screen}"
    );
    assert!(
        !screen.contains("▶ 2. Jazz Cafe") && !screen.contains("▶ Jazz Cafe"),
        "a channel that is not playing marks nothing, got:\n{screen}"
    );
}

/// The playing stream reaches the top too: the `cliamp radio` section row
/// marks when one of the section's channels plays, so the marker follows
/// the tree outward as well as inward.
#[test]
fn a_playing_channel_marks_the_cliamp_section_row_at_the_top() {
    let mut view = view();
    view.set_playing(Some((
        String::from("https://radio.cliamp.stream/lofi/stream"),
        PlayState::Playing,
    )));
    let marked = screen(&view);
    assert!(
        marked.contains("▶ 1. cliamp radio"),
        "the section whose channel plays marks its row at the top, got:\n{marked}"
    );
    view.set_playing(None);
    let quiet = screen(&view);
    assert!(
        !quiet.contains("▶ 1. cliamp radio"),
        "with nothing playing the section row marks nothing, got:\n{quiet}"
    );
}

// ---- issue #71: the songs over the shell -------------------------------------

/// The author's flow, driven the way the event loop drives it: open the
/// radio, enter the cliamp section, enter the first channel, and let the
/// ticks bring `/tracks` in. The songs must stand under the live row and
/// the header must count them — the header is not allowed to say
/// `0 songs` while the service answered with songs.
#[test]
fn opening_a_channel_from_the_shell_lists_its_songs_and_counts_them() {
    let canned = Arc::new(
        Canned::new()
            .stations(STATIONS_FIXTURE)
            .statistics(STATISTICS_FIXTURE)
            .tracks(TRACKS_FIXTURE),
    );
    let mut app = App::new(Vec::new());
    app.set_radio_wire(Box::new(wired(Arc::clone(&canned))));
    shell(&mut app, KeyCode::Char('R')); // the radio opens over its wiring
    shell(&mut app, KeyCode::Enter); // into the cliamp section
    for _ in 0..400 {
        app.tick();
        if app_screen(&app).contains("LoFi Beats") {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    shell(&mut app, KeyCode::Enter); // into the first channel's place
    for _ in 0..400 {
        app.tick();
        if app_screen(&app).contains("Dawn") {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    let screen = app_screen(&app);
    assert!(
        screen.contains("Dawn") && screen.contains("Dusk"),
        "the channel lists the songs the service answered, got:\n{screen}"
    );
    assert!(
        screen.contains("[2 songs]"),
        "the header counts the song rows, got:\n{screen}"
    );
    assert!(
        !screen.contains("[0 songs]"),
        "the header never claims zero songs while the songs stand on screen, got:\n{screen}"
    );
}

// ---- re-entry flows: the songs must survive leaving and reopening ----------

/// One key press followed by the wiring turn the shell's key handler runs
/// (`radio_poll` runs right after every key); the ticks then keep polling
/// while the test waits.
fn press_and_poll<T>(data: &mut RadioData<T>, view: &mut RadioView, code: KeyCode)
where
    T: Transport + Send + Sync + 'static,
{
    press(view, code);
    data.poll(view);
}

/// Leaving the channel while its request is still in flight and re-entering
/// later: the answer the wiring cached must reach the view, never a blank
/// channel with `[0 songs]`.
#[test]
fn leaving_a_channel_mid_flight_and_reentering_still_shows_the_songs() {
    let canned = Arc::new(
        Canned::new()
            .stations(STATIONS_FIXTURE)
            .statistics(STATISTICS_FIXTURE)
            .tracks(TRACKS_FIXTURE),
    );
    let mut data = wired(Arc::clone(&canned));
    let mut view = data.view();

    press(&mut view, KeyCode::Enter); // the cliamp section
    settle(&mut data, &mut view, &canned, |_view, canned| {
        canned.calls().len() == 2
    });
    press_and_poll(&mut data, &mut view, KeyCode::Enter); // into the channel: the ask goes out
    press_and_poll(&mut data, &mut view, KeyCode::Esc); // out before the answer lands
    settle(&mut data, &mut view, &canned, |view, canned| {
        canned
            .calls()
            .iter()
            .any(|url| url.contains("/lofi/tracks"))
            && view.place() == &Place::Cliamp
    });

    press_and_poll(&mut data, &mut view, KeyCode::Enter); // into the same channel again
    settle(&mut data, &mut view, &canned, |view, _| {
        view.labels().contains(&String::from("Dawn"))
    });
    assert!(
        view.labels().contains(&String::from("Dawn")),
        "re-entering the channel shows the songs the first ask brought, got: {:?}",
        view.labels()
    );
}

/// Closing the radio and opening it again in the same session: the channels
/// come back from the wiring's memory, and entering the channel must still
/// list its songs — the author saw `[0 songs]` after exactly this.
#[test]
fn reopening_the_radio_still_lists_a_channel_songs() {
    let canned = Arc::new(
        Canned::new()
            .stations(STATIONS_FIXTURE)
            .statistics(STATISTICS_FIXTURE)
            .tracks(TRACKS_FIXTURE),
    );
    let mut data = wired(Arc::clone(&canned));
    let mut view = data.view();

    press(&mut view, KeyCode::Enter); // the cliamp section
    settle(&mut data, &mut view, &canned, |_view, canned| {
        canned.calls().len() == 2
    });
    press_and_poll(&mut data, &mut view, KeyCode::Enter); // into the channel; its songs load
    settle(&mut data, &mut view, &canned, |view, canned| {
        canned
            .calls()
            .iter()
            .any(|url| url.contains("/lofi/tracks"))
            && view.labels().contains(&String::from("Dawn"))
    });
    press_and_poll(&mut data, &mut view, KeyCode::Esc); // back to the section
    press_and_poll(&mut data, &mut view, KeyCode::Esc); // back to the top
    press_and_poll(&mut data, &mut view, KeyCode::Esc); // the radio closes

    // The radio reopens over the same wiring: a fresh view, the channels
    // already known and the songs already answered once.
    let mut view = data.view();
    press_and_poll(&mut data, &mut view, KeyCode::Enter); // the cliamp section
    settle(&mut data, &mut view, &canned, |view, _| {
        view.labels().contains(&String::from("LoFi Beats"))
    });
    press_and_poll(&mut data, &mut view, KeyCode::Enter); // into the channel again
    settle(&mut data, &mut view, &canned, |view, canned| {
        view.labels().contains(&String::from("Dawn")) || canned.calls().len() >= 4
    });
    assert!(
        view.labels().contains(&String::from("Dawn")),
        "the reopened radio lists the channel's songs, got: {:?} (calls: {:?})",
        view.labels(),
        canned.calls()
    );
}

// ---- a lost answer must not strand a channel --------------------------------

/// The service's answer can be lost once — a connection refused, a timeout.
/// A failed ask is not an answer: re-entering the channel is the user asking
/// again, and the wiring must ask. Before the fix it remembered the channel
/// forever, so the place stayed blank with a lying `[0 songs]` for the whole
/// session.
#[test]
fn a_failed_songs_ask_is_asked_again_on_reentry() {
    let canned = Arc::new(
        Canned::new()
            .stations(STATIONS_FIXTURE)
            .statistics(STATISTICS_FIXTURE)
            .tracks(TRACKS_FIXTURE)
            .failing_tracks_once(),
    );
    let mut data = wired(Arc::clone(&canned));
    let mut view = data.view();

    press(&mut view, KeyCode::Enter); // the cliamp section
    settle(&mut data, &mut view, &canned, |_view, canned| {
        canned.calls().len() == 2
    });
    press_and_poll(&mut data, &mut view, KeyCode::Enter); // into the channel: the ask goes out and fails
    settle(&mut data, &mut view, &canned, |view, canned| {
        canned
            .calls()
            .iter()
            .any(|url| url.contains("/lofi/tracks"))
            && view.labels().contains(&String::from("LoFi Beats — live"))
    });
    assert!(
        !view.labels().contains(&String::from("Dawn")),
        "the failed ask left no songs, got: {:?}",
        view.labels()
    );

    press_and_poll(&mut data, &mut view, KeyCode::Esc); // back to the section
    press_and_poll(&mut data, &mut view, KeyCode::Enter); // the user asks again by entering
    settle(&mut data, &mut view, &canned, |_, canned| {
        canned
            .calls()
            .iter()
            .filter(|url| url.contains("/lofi/tracks"))
            .count()
            == 2
    });
    settle(&mut data, &mut view, &canned, |view, _| {
        view.labels().contains(&String::from("Dawn"))
    });
    assert!(
        view.labels().contains(&String::from("Dawn")),
        "the re-entry asked again and the songs arrived, got: {:?} (calls: {:?})",
        view.labels(),
        canned.calls()
    );
}

/// The header of a channel whose songs never arrived must not count to
/// zero — `0 songs` is the service's word for a channel it says is empty,
/// never the answer being absent. The honest line names what happened.
#[test]
fn a_channel_without_its_songs_does_not_claim_zero_songs() {
    let mut view = bare();
    view.set_cliamp_channels(channels());
    view.open_place(channel_place());
    let screen = screen(&view);
    assert!(
        screen.contains("[no songs loaded]"),
        "a channel whose songs never arrived says so, got:\n{screen}"
    );
    assert!(
        !screen.contains("[0 songs]"),
        "an unloaded channel never claims the service's own zero, got:\n{screen}"
    );
}

/// And when the answer IS held — even empty — the header counts it: zero
/// songs is then the service's own word about the channel.
#[test]
fn a_channel_holding_an_empty_answer_says_zero_songs() {
    let mut view = bare();
    view.set_cliamp_channels(channels());
    view.open_place(channel_place());
    view.set_cliamp_songs(channel_place(), Vec::new());
    let screen = screen(&view);
    assert!(
        screen.contains("[0 songs]"),
        "a held empty answer is counted honestly, got:\n{screen}"
    );
}

// ---- marking in the cliamp places: `f` reaches every play -------------------

/// A scratch directory the store files live in, outside the repository.
fn marks_dir(tag: &str) -> PathBuf {
    let path =
        std::env::temp_dir().join(format!("velo-ui-cliamp-marks-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("the scratch directory is created");
    path
}

/// `f` on the live row marks the channel through the station store: the
/// channel's stream into `radio.toml`, exactly the way a directory station
/// is marked, and a second `f` unmarks it again.
#[test]
fn f_on_the_live_row_toggles_the_channel_in_the_station_store() {
    let mut view = bare();
    view.set_cliamp_channels(channels());
    view.open_place(channel_place());
    assert_eq!(
        view.selection().map(|row| row.label),
        Some(String::from("LoFi Beats — live")),
        "the cursor opens the place on the live row"
    );

    press(&mut view, KeyCode::Char('f'));
    assert!(
        view.store()
            .is_favorite("https://radio.cliamp.stream/lofi/stream"),
        "the channel's stream is marked in the station store"
    );
    assert!(
        view.take_dirty(),
        "the mark asks for a save, the way every station toggle does"
    );

    press(&mut view, KeyCode::Char('f'));
    assert!(
        !view
            .store()
            .is_favorite("https://radio.cliamp.stream/lofi/stream"),
        "a second `f` unmarks the channel"
    );
    assert!(view.take_dirty(), "the unmark asks for a save too");
}

/// The save ask becomes the file: the wiring writes the channel's stream
/// into `radio.toml`, and the untoggle removes it.
#[test]
fn f_on_the_live_row_writes_the_stream_into_radio_toml() {
    let dir = marks_dir("live-store");
    let store = dir.join("radio.toml");
    let canned = Arc::new(
        Canned::new()
            .stations(STATIONS_FIXTURE)
            .statistics(STATISTICS_FIXTURE),
    );
    let mut data = RadioData::new(Some(Shared(Arc::clone(&canned))), Some(store.clone()), None);
    let mut view = data.view();

    press(&mut view, KeyCode::Enter); // the cliamp section
    settle(&mut data, &mut view, &canned, |view, _| {
        view.labels() == ["LoFi Beats", "Jazz Cafe"]
    });
    press(&mut view, KeyCode::Enter); // onto the channel's place, the live row
    press(&mut view, KeyCode::Char('f'));

    for _ in 0..100 {
        data.poll(&mut view);
        if store.exists() {
            break;
        }
    }
    let saved = std::fs::read_to_string(&store).expect("the toggle wrote radio.toml");
    assert!(
        saved.contains("url = \"https://radio.cliamp.stream/lofi/stream\""),
        "the channel's stream is in the file, got:\n{saved}"
    );

    press(&mut view, KeyCode::Char('f'));
    for _ in 0..100 {
        data.poll(&mut view);
        let Ok(saved) = std::fs::read_to_string(&store) else {
            continue;
        };
        if !saved.contains("lofi/stream") {
            break;
        }
    }
    let saved = std::fs::read_to_string(&store).expect("the file stays readable");
    assert!(
        !saved.contains("[[favorite]]"),
        "the second `f` removed the entry, got:\n{saved}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `f` on a cliamp song marks it as a track: a `[[favorite]]` whose path is
/// the song's URL (its identity everywhere, cliamp S3) and whose metadata is
/// what the service knows, written to `library.toml` at once — and a second
/// `f` removes it again.
#[test]
fn f_marks_a_cliamp_song_in_library_toml_and_shows_the_star() {
    let dir = marks_dir("song-mark");
    let config = dir.join("config.toml");
    let mut app = App::new(Vec::new());
    app.set_config_file(Some(config.clone()));
    shell(&mut app, KeyCode::Char('R')); // the radio opens over its wiring
    app.show_radio_cliamp_songs(channel_place(), songs());

    shell(&mut app, KeyCode::Char('f'));
    let library = dir.join("library.toml");
    let saved = std::fs::read_to_string(&library).expect("the toggle wrote library.toml");
    assert!(
        saved.contains("[[favorite]]"),
        "the song is a track, stored in the favourites table, got:\n{saved}"
    );
    assert!(
        saved.contains("path = \"https://radio.cliamp.stream/lofi/tracks/t1\""),
        "the song's URL is the path, got:\n{saved}"
    );
    assert!(
        saved.contains("artist = \"Tide\"")
            && saved.contains("album = \"Harbor\"")
            && saved.contains("title = \"Dawn\""),
        "the metadata the service knows is copied in, got:\n{saved}"
    );
    assert!(
        app_screen(&app).contains("★ Dawn"),
        "the marked song shows its star in the channel's list at once, got:\n{}",
        app_screen(&app)
    );

    shell(&mut app, KeyCode::Char('f'));
    let saved = std::fs::read_to_string(&library).expect("the file stays readable");
    assert!(
        !saved.contains("[[favorite]]") && !saved.contains("lofi/tracks/t1"),
        "the second `f` removed the entry, got:\n{saved}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The sources screen says where cliamp playback comes from: a song plays
/// as a file whose path is its `https://` URL, so the Radio row carries the
/// marker — never a bare row that says nothing.
#[test]
fn a_playing_cliamp_song_marks_the_radio_row_on_the_sources_screen() {
    let mut app = App::browse(None);
    app.show_playing_for_tests(
        Entry::File(PathBuf::from("https://radio.cliamp.stream/lofi/tracks/t1")),
        Duration::ZERO,
        None,
        None,
    );
    app.open_sources();

    let screen = app_screen(&app);
    let radio_row = screen
        .lines()
        .find(|line| line.contains("Radio"))
        .expect("the Radio row is on the screen");
    assert!(
        radio_row.contains("▶"),
        "a cliamp song came from the radio side, and the marker says so, got: {radio_row:?}"
    );
}

/// A cliamp song marked in `library.toml` still says Radio: the origin is
/// where playback came from, not where the mark lives — the URL check
/// stands before the favourites check.
#[test]
fn a_marked_cliamp_song_still_says_radio_on_the_sources_screen() {
    let dir = marks_dir("song-source");
    let config = dir.join("config.toml");
    std::fs::write(
        dir.join("library.toml"),
        "[[favorite]]\npath = \"https://radio.cliamp.stream/lofi/tracks/t1\"\n",
    )
    .expect("the store is written");
    let mut app = App::browse(None);
    app.set_config_file(Some(config));
    app.show_playing_for_tests(
        Entry::File(PathBuf::from("https://radio.cliamp.stream/lofi/tracks/t1")),
        Duration::ZERO,
        None,
        None,
    );
    app.open_sources();

    let screen = app_screen(&app);
    let radio_row = screen
        .lines()
        .find(|line| line.contains("Radio"))
        .expect("the Radio row is on the screen");
    assert!(
        radio_row.contains("▶"),
        "the song says Radio even though its mark is in the favourites store, got: {radio_row:?}"
    );
    let favourites_row = screen
        .lines()
        .find(|line| line.contains("Favourites"))
        .expect("the Favourites row is on the screen");
    assert!(
        !favourites_row.contains("▶"),
        "the mark never pretends the song plays from the Favourites source, got: {favourites_row:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `f` on a channel row marks the live stream the channel plays — the
/// author asked for exactly this, from the channel list and from the live
/// row alike: "si se guarda en radio.toml pero no se ve en la TUI, ademas
/// tanto desde la lista de canales como desde dentro donde dice live di se
/// presiona f debe de poder agregarlo a fav a ese canal de cliamp." The
/// store holds the channel's stream under the one station key, the mark
/// asks for its save, and the top's favourites block has it without a
/// restart.
#[test]
fn f_on_a_channel_row_marks_its_live_stream() {
    let mut view = view();
    view.open_place(Place::Cliamp);
    assert_eq!(
        view.selection().map(|row| row.label),
        Some(String::from("LoFi Beats")),
        "the cursor opens the section on the first channel"
    );

    press(&mut view, KeyCode::Char('f'));
    assert!(
        view.store()
            .is_favorite("https://radio.cliamp.stream/lofi/stream"),
        "the channel's live stream is marked in the station store"
    );
    assert!(
        view.store()
            .favorite
            .iter()
            .any(|favorite| favorite.name == "LoFi Beats"),
        "the entry keeps the channel's own name"
    );
    assert!(
        view.take_dirty(),
        "the mark asks for a save, the way every station toggle does"
    );

    // The live row inside the channel shows the mark without a restart, and
    // so does the top's favourites block.
    view.set_cliamp_songs(channel_place(), songs());
    view.open_place(channel_place());
    assert_eq!(
        view.visible_rows()[0].label,
        "★ LoFi Beats — live",
        "the live row carries the star the store just put on"
    );
    view.open_place(Place::Top);
    assert!(
        screen(&view).contains("★ LoFi Beats"),
        "the favourites block shows the channel, got:\n{}",
        screen(&view)
    );
}

/// Either row's `f` says what happened on the status line — the line the
/// shell stands. A silent `Handled` is what the author saw and read as
/// `no me deja`; a mark that lands is never mute again.
#[test]
fn the_mark_says_so_on_the_status_line_from_either_row() {
    let mut view = view();
    view.open_place(Place::Cliamp);

    assert_eq!(
        press(&mut view, KeyCode::Char('f')),
        RadioOutcome::Status {
            line: String::from("velo: ★ LoFi Beats"),
        },
        "the channel row's mark is confirmed by name"
    );
    assert_eq!(
        press(&mut view, KeyCode::Char('f')),
        RadioOutcome::Status {
            line: String::from("velo: LoFi Beats — unmarked"),
        },
        "the channel row's unmark is confirmed too"
    );

    view.open_place(channel_place());
    assert_eq!(
        press(&mut view, KeyCode::Char('f')),
        RadioOutcome::Status {
            line: String::from("velo: ★ LoFi Beats"),
        },
        "the live row's mark is confirmed by the same line"
    );
    assert_eq!(
        press(&mut view, KeyCode::Char('f')),
        RadioOutcome::Status {
            line: String::from("velo: LoFi Beats — unmarked"),
        },
        "the live row's unmark is confirmed too"
    );
}

/// The status line through the shell, the way the author saw its absence:
/// `f` on the live row, then on the channel row, each stands its line.
#[test]
fn the_shell_stands_the_mark_line_for_both_rows() {
    let canned = Arc::new(
        Canned::new()
            .stations(STATIONS_FIXTURE)
            .statistics(STATISTICS_FIXTURE)
            .tracks(TRACKS_FIXTURE),
    );
    let mut app = App::new(Vec::new());
    app.set_radio_wire(Box::new(wired(Arc::clone(&canned))));
    shell(&mut app, KeyCode::Char('R')); // the radio opens over its wiring
    shell(&mut app, KeyCode::Enter); // into the cliamp section
    for _ in 0..400 {
        app.tick();
        if app_screen(&app).contains("LoFi Beats") {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    shell(&mut app, KeyCode::Enter); // into the first channel's place

    shell(&mut app, KeyCode::Char('f')); // the live row
    assert_eq!(
        app.status(),
        "velo: ★ LoFi Beats",
        "the shell stands the line the live row's mark asked for, got: {:?}",
        app.status()
    );

    shell(&mut app, KeyCode::Char('f'));
    assert_eq!(
        app.status(),
        "velo: LoFi Beats — unmarked",
        "the unmark says so too, got: {:?}",
        app.status()
    );

    // Back on the channel list, the cursor rests on the channel that opened
    // the place: its `f` asks for the very same line.
    shell(&mut app, KeyCode::Esc);
    shell(&mut app, KeyCode::Char('f'));
    assert_eq!(
        app.status(),
        "velo: ★ LoFi Beats",
        "the channel row's mark stands the same line, got: {:?}",
        app.status()
    );
}

/// A channel the service gave no stream for has no live row and nothing to
/// mark: `f` on its channel row writes nothing and says so — never a junk
/// station key in `radio.toml`.
#[test]
fn f_on_a_streamless_channel_row_writes_nothing_and_says_so() {
    let mut without = channels();
    without[0].stream = String::new();
    let mut view = bare();
    view.set_cliamp_channels(without);
    view.open_place(Place::Cliamp);

    assert_eq!(
        press(&mut view, KeyCode::Char('f')),
        RadioOutcome::Status {
            line: String::from("velo: LoFi Beats has no live stream to mark"),
        },
        "the refusal is said, not silent"
    );
    assert!(
        view.store().favorite.is_empty(),
        "no junk key leaked into the station store"
    );
    assert!(!view.take_dirty(), "nothing was written, nothing to save");
}

/// The star travels with the live row and comes off again: `f` marks the
/// stream and the row reads `★ Name — live`, the same starred form every
/// other marked row uses; a second `f` restores the bare `Name — live`.
/// The unmarked wording itself never changes.
#[test]
fn the_live_row_carries_its_star_while_it_is_marked() {
    let mut view = bare();
    view.set_cliamp_channels(channels());
    view.open_place(channel_place());
    assert_eq!(
        view.visible_rows()[0].label,
        "LoFi Beats — live",
        "an unmarked live row keeps its wording"
    );

    press(&mut view, KeyCode::Char('f'));
    assert_eq!(
        view.visible_rows()[0].label,
        "★ LoFi Beats — live",
        "the star is the store's own marker, the one every marked row wears"
    );

    press(&mut view, KeyCode::Char('f'));
    assert_eq!(
        view.visible_rows()[0].label,
        "LoFi Beats — live",
        "unmarking drops the star again"
    );
}

/// One store, one key: what the channel row wrote the live row shows and
/// can take off, and what the live row takes off the channel row can write
/// again — the two rows and the favourites block never disagree.
#[test]
fn the_two_rows_toggle_one_and_the_same_mark() {
    let mut view = view();
    view.open_place(Place::Cliamp);
    press(&mut view, KeyCode::Char('f')); // the channel row marks

    view.open_place(channel_place());
    assert_eq!(
        view.visible_rows()[0].label,
        "★ LoFi Beats — live",
        "the live row shows what the channel row wrote"
    );
    press(&mut view, KeyCode::Char('f')); // the live row unmarks
    assert!(
        !view
            .store()
            .is_favorite("https://radio.cliamp.stream/lofi/stream"),
        "unmarking from the live row cleared the channel row's mark"
    );
    assert_eq!(
        view.visible_rows()[0].label,
        "LoFi Beats — live",
        "the live row drops the star with the store"
    );

    view.open_place(Place::Cliamp);
    press(&mut view, KeyCode::Char('f')); // the channel row marks again
    assert!(
        view.store()
            .is_favorite("https://radio.cliamp.stream/lofi/stream"),
        "marking from the channel row restores the one mark"
    );
}
