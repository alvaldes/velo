//! The radio view's data wiring (T6b, S9): lazy directory requests on a
//! worker thread, the preference store, and the country question.
//!
//! No network and no terminal: the directory client runs over a canned,
//! counting transport (the `radio::client::Transport` seam), and the store
//! lives in a temporary directory each test removes afterwards. Every
//! request runs on a worker thread and lands through a channel, polled the
//! way the event loop polls it, so the tests also prove the wiring never
//! blocks and that opening the view costs zero requests.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use velo::radio::client::{Client, Error, Transport};
use velo::ui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use velo::ui::radio::RadioView;
use velo::ui::radio_data::{RadioData, RadioWire};

// ---- the canned, counting transport ----------------------------------------

/// A directory transport with canned bodies and a call log. The bodies are
/// chosen by endpoint; `failing` turns every request into a refusal.
struct Canned {
    calls: Mutex<Vec<String>>,
    countries: String,
    tags: String,
    search: String,
    fail: bool,
}

impl Canned {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            countries: String::from("[]"),
            tags: String::from("[]"),
            search: String::from("[]"),
            fail: false,
        }
    }

    fn countries(mut self, body: &str) -> Self {
        body.clone_into(&mut self.countries);
        self
    }

    fn tags(mut self, body: &str) -> Self {
        body.clone_into(&mut self.tags);
        self
    }

    fn search(mut self, body: &str) -> Self {
        body.clone_into(&mut self.search);
        self
    }

    fn failing(mut self) -> Self {
        self.fail = true;
        self
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("call log").clone()
    }
}

impl Transport for Canned {
    fn get(&self, url: &str) -> Result<String, Error> {
        self.calls.lock().expect("call log").push(url.to_owned());
        if self.fail {
            return Err(Error::Unreachable {
                detail: "connection refused".to_owned(),
            });
        }
        if url.contains("/countries") {
            Ok(self.countries.clone())
        } else if url.contains("/tags") {
            Ok(self.tags.clone())
        } else if url.contains("/stations/search") {
            Ok(self.search.clone())
        } else {
            Ok(String::from("[]"))
        }
    }
}

/// Shares the canned transport with the client: the worker thread fetches
/// through it while the test reads back the same call log.
struct Shared(Arc<Canned>);

impl Transport for Shared {
    fn get(&self, url: &str) -> Result<String, Error> {
        self.0.get(url)
    }
}

/// One directory station as the wire lists it.
fn station_json(index: usize, url: &str) -> String {
    format!(
        r#"{{"name":"S{index}","url_resolved":"{url}","country":"Norway","countrycode":"NO","state":"","tags":"","codec":"MP3","bitrate":128,"votes":{index},"homepage":""}}"#
    )
}

/// A `/stations/search` body of `count` stations, each named `S<n>` with an
/// `https` stream URL.
fn stations_json(count: usize) -> String {
    let rows: Vec<String> = (0..count)
        .map(|index| station_json(index, &format!("https://s{index}.example/stream")))
        .collect();
    format!("[{}]", rows.join(","))
}

/// A temporary directory that removes itself, so no test leaves state
/// behind.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!("velo-radio-data-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&path).expect("the temp directory is created");
        Self(path)
    }

    fn radio_toml(&self) -> PathBuf {
        self.0.join("radio.toml")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The wired loader over a canned transport, with no store file and no
/// country detected.
fn wired(canned: Arc<Canned>) -> RadioData<Shared> {
    RadioData::new(Some(Client::new(Shared(canned))), None, None)
}

fn press(view: &mut RadioView, code: KeyCode) {
    let _ = view.key(KeyEvent::from(code));
}

/// Polls until `done` sees what it waits for, the way the event loop polls
/// the channel between keys. The worker answers in microseconds; the
/// timeout only catches a wiring that never answers.
fn settle(
    data: &mut RadioData<Shared>,
    view: &mut RadioView,
    canned: &Canned,
    done: impl Fn(&RadioView, &Canned) -> bool,
) {
    for _ in 0..400 {
        data.poll(view);
        if done(view, canned) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the wiring never answered");
}

// ---- lazy, not eager --------------------------------------------------------

#[test]
fn opening_the_view_makes_no_request() {
    let canned = Arc::new(Canned::new());
    let mut data = wired(Arc::clone(&canned));
    let mut view = data.view();

    // The sections are there before anything loads, so the view can be
    // entered at all — but nothing has been asked of the directory.
    assert_eq!(
        view.labels(),
        ["Browse all countries", "Browse genres & tags"],
        "the wired view shows the browse rows before their indexes load, got: {:?}",
        view.labels()
    );
    for _ in 0..5 {
        data.poll(&mut view);
    }
    press(&mut view, KeyCode::Char('j'));
    data.poll(&mut view);

    assert!(
        canned.calls().is_empty(),
        "opening the view and polling at the top makes zero requests, got: {:?}",
        canned.calls()
    );
}

#[test]
fn entering_the_country_section_loads_the_index_once() {
    let canned = Arc::new(Canned::new().countries(
        r#"[{"name":"Norway","iso_3166_1":"NO","stationcount":2},
            {"name":"Germany","iso_3166_1":"DE","stationcount":40}]"#,
    ));
    let mut data = wired(Arc::clone(&canned));
    let mut view = data.view();

    press(&mut view, KeyCode::Enter); // Browse all countries
    settle(&mut data, &mut view, &canned, |view, canned| {
        !view.labels().is_empty() && canned.calls().len() == 1
    });

    assert_eq!(
        canned.calls().len(),
        1,
        "entering the section makes exactly one request, got: {:?}",
        canned.calls()
    );
    assert!(canned.calls()[0].contains("/countries"));
    assert_eq!(
        view.labels(),
        ["Norway", "Germany"],
        "the index lands in the view, got: {:?}",
        view.labels()
    );

    // Leaving and re-entering stays cached: no second request.
    press(&mut view, KeyCode::Esc); // back to the top
    press(&mut view, KeyCode::Enter); // in again
    for _ in 0..5 {
        data.poll(&mut view);
    }
    assert_eq!(
        canned.calls().len(),
        1,
        "re-entering a loaded section re-uses the index, got: {:?}",
        canned.calls()
    );
}

#[test]
fn entering_a_place_searches_with_the_country_code() {
    let canned = Arc::new(Canned::new().countries(
        r#"[{"name":"Norway","iso_3166_1":"NO","stationcount":2}]"#,
    ).search(&stations_json(2)));
    let mut data = wired(Arc::clone(&canned));
    let mut view = data.view();

    press(&mut view, KeyCode::Enter); // Browse all countries
    settle(&mut data, &mut view, &canned, |view, _canned| !view.labels().is_empty());
    press(&mut view, KeyCode::Enter); // Norway
    settle(&mut data, &mut view, &canned, |view, canned| {
        !view.labels().is_empty()
            && canned.calls().iter().any(|url| url.contains("/stations/search"))
    });

    let calls = canned.calls();
    let search = calls
        .iter()
        .find(|url| url.contains("/stations/search"))
        .expect("the place loaded its stations");
    assert!(
        search.contains("countrycode=NO"),
        "the place searches with its country code, got: {search}"
    );
    assert!(
        search.contains("limit=200"),
        "at most 200 are asked, got: {search}"
    );
    assert!(
        search.contains("hidebroken=true"),
        "broken stations are hidden, got: {search}"
    );
    assert_eq!(
        view.labels(),
        ["S0 [128k] · NO", "S1 [128k] · NO"],
        "the stations land in the view, got: {:?}",
        view.labels()
    );
}

#[test]
fn entering_a_tag_searches_with_tag_exact() {
    let canned = Arc::new(
        Canned::new()
            .tags(r#"[{"name":"jazz","stationcount":30}]"#)
            .search(&stations_json(1)),
    );
    let mut data = wired(Arc::clone(&canned));
    let mut view = data.view();

    press(&mut view, KeyCode::Char('j')); // Browse genres & tags
    press(&mut view, KeyCode::Enter);
    settle(&mut data, &mut view, &canned, |view, canned| {
        !view.labels().is_empty() && !canned.calls().is_empty()
    });

    press(&mut view, KeyCode::Enter); // jazz
    settle(&mut data, &mut view, &canned, |_view, canned| {
        canned
            .calls()
            .iter()
            .any(|url| url.contains("/stations/search"))
    });

    let calls = canned.calls();
    let search = calls
        .iter()
        .find(|url| url.contains("/stations/search"))
        .expect("the tag loaded its stations");
    assert!(
        search.contains("tag=jazz") && search.contains("tagExact=true"),
        "the tag searches exactly, got: {search}"
    );
}

#[test]
fn ctrl_r_rerequests_only_what_the_current_place_needs() {
    let canned = Arc::new(
        Canned::new()
            .countries(r#"[{"name":"Norway","iso_3166_1":"NO","stationcount":2}]"#)
            .search(&stations_json(1)),
    );
    let mut data = wired(Arc::clone(&canned));
    let mut view = data.view();

    press(&mut view, KeyCode::Enter); // into the countries
    settle(&mut data, &mut view, &canned, |view, canned| {
        !view.labels().is_empty() && canned.calls().len() == 1
    });

    // At the top nothing needs re-requesting.
    press(&mut view, KeyCode::Esc);
    let _ = view.key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    data.poll(&mut view);
    assert_eq!(
        canned.calls().len(),
        1,
        "ctrl-r at the top asks for nothing, got: {:?}",
        canned.calls()
    );

    // Back in the section it re-requests exactly the index.
    press(&mut view, KeyCode::Enter);
    let _ = view.key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    settle(&mut data, &mut view, &canned, |_view, canned| canned.calls().len() == 2);
    assert!(
        canned.calls()[1].contains("/countries"),
        "the refresh re-requests the current place's index, got: {:?}",
        canned.calls()
    );
}

#[test]
fn a_failing_transport_reports_and_leaves_the_view_usable() {
    let canned = Arc::new(Canned::new().failing());
    let mut data = wired(Arc::clone(&canned));
    let mut view = data.view();

    press(&mut view, KeyCode::Enter); // Browse all countries
    settle(&mut data, &mut view, &canned, |_view, canned| {
        !canned.calls().is_empty()
    });

    let message = data
        .take_message()
        .expect("the failure reaches the status line");
    assert!(
        message.contains("could not be reached"),
        "the status names the failure, got: {message}"
    );

    // The view keeps working: it navigates, and it does not retry in a
    // loop behind the user's back.
    press(&mut view, KeyCode::Char('j'));
    assert!(view.back(), "the view still answers navigation");
    assert_eq!(view.place(), &velo::ui::radio::Place::Top, "back at the sections");
    for _ in 0..5 {
        data.poll(&mut view);
    }
    assert_eq!(
        canned.calls().len(),
        1,
        "the failure does not retry, got: {:?}",
        canned.calls()
    );
}

#[test]
fn stations_load_capped_at_200_and_non_http_urls_are_dropped() {
    let mut body = stations_json(250);
    body.pop(); // drop the closing bracket
    body.push(',');
    body.push_str(&station_json(999, "ftp://not-a-stream.example/audio"));
    body.push(']');
    let canned = Arc::new(
        Canned::new()
            .countries(r#"[{"name":"Norway","iso_3166_1":"NO","stationcount":9}]"#)
            .search(&body),
    );
    let mut data = wired(Arc::clone(&canned));
    let mut view = data.view();

    press(&mut view, KeyCode::Enter); // Browse all countries
    settle(&mut data, &mut view, &canned, |view, _canned| !view.labels().is_empty());
    press(&mut view, KeyCode::Enter); // Norway
    settle(&mut data, &mut view, &canned, |view, _canned| view.labels().len() == 200);

    assert_eq!(view.labels().len(), 200, "the cap holds");
    assert!(
        !view.labels().iter().any(|label| label.contains("S999")),
        "the non-http station never loads, got: {:?}",
        view.labels().last()
    );
}

// ---- the preference store ---------------------------------------------------

#[test]
fn a_corrupt_store_is_reported_and_never_overwritten() {
    let dir = TempDir::new("corrupt");
    std::fs::write(dir.radio_toml(), b"this is not toml {{{").expect("the corrupt file is written");
    let before = std::fs::read(dir.radio_toml()).expect("the corrupt file is read");

    let canned = Arc::new(
        Canned::new().countries(r#"[{"name":"Norway","iso_3166_1":"NO","stationcount":2}]"#),
    );
    let mut data = RadioData::new(
        Some(Client::new(Shared(Arc::clone(&canned)))),
        Some(dir.radio_toml()),
        None,
    );
    let mut view = data.view();
    data.poll(&mut view);

    let message = data
        .take_message()
        .expect("the corruption reaches the status line");
    assert!(
        message.contains("corrupt"),
        "the status names the corruption, got: {message}"
    );
    assert_eq!(
        std::fs::read(dir.radio_toml()).expect("the file is still readable"),
        before,
        "the corrupt file is byte-for-byte unchanged"
    );

    // Even a mutation afterwards must not write over what was never read.
    press(&mut view, KeyCode::Enter); // countries load fine
    settle(&mut data, &mut view, &canned, |view, _canned| !view.labels().is_empty());
    press(&mut view, KeyCode::Char('f')); // pin Norway
    data.poll(&mut view);
    assert_eq!(
        std::fs::read(dir.radio_toml()).expect("the file is still readable"),
        before,
        "a mutation never overwrites a store that could not be read"
    );
}

#[test]
fn a_successful_favourite_save_creates_the_file() {
    let dir = TempDir::new("save");
    let canned = Arc::new(
        Canned::new()
            .countries(r#"[{"name":"Norway","iso_3166_1":"NO","stationcount":2}]"#)
            .search(&stations_json(1)),
    );
    let mut data = RadioData::new(
        Some(Client::new(Shared(Arc::clone(&canned)))),
        Some(dir.radio_toml()),
        None,
    );
    let mut view = data.view();

    press(&mut view, KeyCode::Enter); // Browse all countries
    settle(&mut data, &mut view, &canned, |view, _canned| !view.labels().is_empty());
    press(&mut view, KeyCode::Enter); // Norway's stations
    settle(&mut data, &mut view, &canned, |view, _canned| view.labels().len() == 1);

    press(&mut view, KeyCode::Char('f')); // favourite S0
    data.poll(&mut view);

    let saved =
        std::fs::read_to_string(dir.radio_toml()).expect("the favourite save created the file");
    assert!(
        saved.contains("[[favorite]]") && saved.contains("https://s0.example/stream"),
        "the favourite reached the file, got: {saved}"
    );
}

// ---- the country question ---------------------------------------------------

#[test]
fn y_answers_the_country_question_and_n_declines_it() {
    // `y` stores the detected country and the row becomes the near-you row.
    let dir = TempDir::new("ask-yes");
    let canned = Arc::new(Canned::new());
    let mut data = RadioData::new(
        Some(Client::new(Shared(Arc::clone(&canned)))),
        Some(dir.radio_toml()),
        Some(String::from("NO")),
    );
    let mut view = data.view();
    assert_eq!(
        view.labels()[1], "Use my location",
        "the unasked question is one row, got: {:?}",
        view.labels()
    );
    press(&mut view, KeyCode::Char('j')); // onto the question
    press(&mut view, KeyCode::Char('y'));
    data.poll(&mut view);
    assert!(
        view.labels().contains(&"NO (near you)".to_owned()),
        "answering yes turns the row into the near-you row, got: {:?}",
        view.labels()
    );
    let saved = std::fs::read_to_string(dir.radio_toml()).expect("the answer is saved");
    assert!(
        saved.contains(r#"country = "NO""#),
        "the yes lands in the file, got: {saved}"
    );

    // `n` stores the decline and the row goes away for good.
    let dir = TempDir::new("ask-no");
    let mut data = RadioData::new(
        Some(Client::new(Shared(Arc::clone(&canned)))),
        Some(dir.radio_toml()),
        Some(String::from("NO")),
    );
    let mut view = data.view();
    press(&mut view, KeyCode::Char('j'));
    press(&mut view, KeyCode::Char('n'));
    data.poll(&mut view);
    assert!(
        !view
            .labels()
            .iter()
            .any(|label| label.contains("near you") || label == "Use my location"),
        "declining leaves no country row, got: {:?}",
        view.labels()
    );
    let saved = std::fs::read_to_string(dir.radio_toml()).expect("the decline is saved");
    assert!(
        saved.contains(r#"country = "none""#),
        "the decline lands in the file, got: {saved}"
    );
}

#[test]
fn an_answered_or_undetectable_country_shows_no_question() {
    let canned = Arc::new(Canned::new());
    // Already answered with a country: the near-you row, no question. The
    // store is loaded from a real file, as the launch does.
    let dir = TempDir::new("answered");
    std::fs::write(dir.radio_toml(), "country = \"NO\"\n").expect("the store is written");
    let data = RadioData::new(
        Some(Client::new(Shared(Arc::clone(&canned)))),
        Some(dir.radio_toml()),
        Some(String::from("NO")),
    );
    let view = data.view();
    assert!(
        view.labels().contains(&"NO (near you)".to_owned()),
        "the stored country is the near-you row, got: {:?}",
        view.labels()
    );
    assert!(
        !view.labels().contains(&"Use my location".to_owned()),
        "an answered country asks nothing"
    );

    // Declined: no row at all.
    let dir = TempDir::new("declined");
    std::fs::write(dir.radio_toml(), "country = \"none\"\n").expect("the store is written");
    let data = RadioData::new(
        Some(Client::new(Shared(Arc::clone(&canned)))),
        Some(dir.radio_toml()),
        Some(String::from("NO")),
    );
    let view = data.view();
    assert!(
        !view.labels().iter().any(|label| label.contains("near you")),
        "a declined country shows no row, got: {:?}",
        view.labels()
    );

    // Never asked but the locale yields nothing: no row to answer (S5).
    let data = wired(Arc::clone(&canned));
    let view = data.view();
    assert!(
        !view.labels().contains(&"Use my location".to_owned()),
        "no country in the locale, no question, got: {:?}",
        view.labels()
    );
}
