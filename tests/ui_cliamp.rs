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

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use velo::audio::output::Entry;
use velo::radio::cliamp::{Channel, Listeners, Song};
use velo::radio::client::{Error, Transport};
use velo::ui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use velo::ui::radio::{Place, RadioOutcome, RadioView};
use velo::ui::radio_data::{RadioData, RadioWire};
use velo::ui::theme::Theme;
use velo::ui::{Action, App};

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

/// Two channels as `/stations` lists them.
fn channels() -> Vec<Channel> {
    vec![
        Channel {
            id: String::from("lofi"),
            name: String::from("LoFi Beats"),
            tracks: 120,
        },
        Channel {
            id: String::from("jazz"),
            name: String::from("Jazz Cafe"),
            tracks: 12,
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
fn the_channels_list_with_their_counts_and_listeners() {
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
        rows[0].fact, "120 tracks · ● 3 listening now",
        "the fact carries the track count and the listeners, got: {:?}",
        rows[0]
    );
    assert_eq!(
        rows[1].fact, "12 tracks · ● 0 listening now",
        "zero listeners is a fact too, got: {:?}",
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
    assert_eq!(rows[0].label, "Dawn");
    assert_eq!(rows[0].fact, "Tide · Harbor", "got: {:?}", rows[0]);
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
