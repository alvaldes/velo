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
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use velo::radio::client::{Client, Error, Transport};
use velo::ui::App;
use velo::ui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use velo::ui::radio::RadioView;
use velo::ui::radio_data::{RadioData, RadioWire};

// ---- the canned, counting transport ----------------------------------------

/// A directory transport with canned bodies and a call log. The bodies are
/// chosen by endpoint; `failing` turns every request into a refusal.
struct Canned {
    calls: Mutex<Vec<String>>,
    countries: String,
    states: String,
    tags: String,
    search: String,
    fail: bool,
    fail_states: bool,
}

impl Canned {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            countries: String::from("[]"),
            states: String::from("[]"),
            tags: String::from("[]"),
            search: String::from("[]"),
            fail: false,
            fail_states: false,
        }
    }

    fn countries(mut self, body: &str) -> Self {
        body.clone_into(&mut self.countries);
        self
    }

    fn states(mut self, body: &str) -> Self {
        body.clone_into(&mut self.states);
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

    /// Fails only `/states/` requests: the other endpoints answer, so a
    /// test can pin what one failing index does to the rest.
    fn failing_states(mut self) -> Self {
        self.fail_states = true;
        self
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("call log").clone()
    }
}

impl Transport for Canned {
    fn get(&self, url: &str) -> Result<String, Error> {
        self.calls.lock().expect("call log").push(url.to_owned());
        if self.fail || (self.fail_states && url.contains("/states/")) {
            return Err(Error::Unreachable {
                detail: "connection refused".to_owned(),
            });
        }
        if url.contains("/countries") {
            Ok(self.countries.clone())
        } else if url.contains("/states/") {
            Ok(self.states.clone())
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

/// A gate the tests hold shut while they look at a request in flight:
/// a `/stations/search` the gate holds waits inside the transport, so the
/// request is genuinely in flight and nothing races the answer.
struct Gate {
    open: Mutex<bool>,
    signal: Condvar,
}

impl Gate {
    fn closed() -> Self {
        Self {
            open: Mutex::new(false),
            signal: Condvar::new(),
        }
    }

    fn open(&self) {
        *self.open.lock().expect("the gate lock") = true;
        self.signal.notify_all();
    }

    fn close(&self) {
        *self.open.lock().expect("the gate lock") = false;
    }

    fn wait(&self) {
        let mut open = self.open.lock().expect("the gate lock");
        while !*open {
            open = self.signal.wait(open).expect("the gate lock");
        }
    }
}

/// The canned transport with the station searches behind the gate; the
/// index endpoints answer freely, so a test reaches a place without
/// touching the gate.
struct Gated {
    canned: Arc<Canned>,
    gate: Arc<Gate>,
}

impl Transport for Gated {
    fn get(&self, url: &str) -> Result<String, Error> {
        if url.contains("/stations/search") {
            self.gate.wait();
        }
        self.canned.get(url)
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
        let path =
            std::env::temp_dir().join(format!("velo-radio-data-{}-{tag}", std::process::id()));
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
fn settle<T>(
    data: &mut RadioData<T>,
    view: &mut RadioView,
    canned: &Canned,
    done: impl Fn(&RadioView, &Canned) -> bool,
) where
    T: Transport + Send + Sync + 'static,
{
    // Waiting only for the request to be recorded races the answer: the
    // transport records the call the moment the worker thread starts, while
    // the reply still has to come back through the channel and be polled in.
    // Under a loaded suite that gap is wide enough to panic on a missing
    // message, so the loader must also be idle before `done` means anything.
    for _ in 0..400 {
        data.poll(view);
        if !data.loading() && done(view, canned) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the wiring never answered, calls: {:?}", canned.calls());
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
    let canned = Arc::new(
        Canned::new()
            .countries(r#"[{"name":"Norway","iso_3166_1":"NO","stationcount":2}]"#)
            .search(&stations_json(2)),
    );
    let mut data = wired(Arc::clone(&canned));
    let mut view = data.view();

    press(&mut view, KeyCode::Enter); // Browse all countries
    settle(&mut data, &mut view, &canned, |view, _canned| {
        !view.labels().is_empty()
    });
    press(&mut view, KeyCode::Enter); // Norway
    settle(&mut data, &mut view, &canned, |view, canned| {
        !view.labels().is_empty()
            && canned
                .calls()
                .iter()
                .any(|url| url.contains("/stations/search"))
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
fn a_name_search_asks_the_directory_across_every_country() {
    let canned = Arc::new(Canned::new().search(&stations_json(2)));
    let mut data = wired(Arc::clone(&canned));
    let mut view = data.view();

    press(&mut view, KeyCode::Char('/'));
    for character in "radio paradise".chars() {
        press(&mut view, KeyCode::Char(character));
    }
    press(&mut view, KeyCode::Enter);
    settle(&mut data, &mut view, &canned, |view, _canned| {
        !view.labels().is_empty()
    });

    let calls = canned.calls();
    let search = calls
        .iter()
        .find(|url| url.contains("/stations/search"))
        .expect("the name reached the directory");
    assert!(
        search.contains("name=radio%20paradise"),
        "the typed name goes on the wire, got: {search}"
    );
    assert!(
        !search.contains("countrycode=") && !search.contains("tag=") && !search.contains("state="),
        "a name search is bound to no country, tag or region, got: {search}"
    );
    assert_eq!(
        view.labels(),
        ["S0 [128k] · NO", "S1 [128k] · NO"],
        "the answer lands in the view, got: {:?}",
        view.labels()
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
    settle(&mut data, &mut view, &canned, |_view, canned| {
        canned.calls().len() == 2
    });
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
    assert_eq!(
        view.place(),
        &velo::ui::radio::Place::Top,
        "back at the sections"
    );
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
    settle(&mut data, &mut view, &canned, |view, _canned| {
        !view.labels().is_empty()
    });
    press(&mut view, KeyCode::Enter); // Norway
    settle(&mut data, &mut view, &canned, |view, _canned| {
        view.labels().len() == 200
    });

    assert_eq!(view.labels().len(), 200, "the cap holds");
    assert!(
        !view.labels().iter().any(|label| label.contains("S999")),
        "the non-http station never loads, got: {:?}",
        view.labels().last()
    );
}

// ---- the home country's regions (T6d, S5) ----------------------------------

/// A wired loader over `dir`, with the store's country already answered
/// `code`: the near-you row exists, so the home country is known.
fn wired_with_country(canned: Arc<Canned>, dir: &TempDir, code: &str) -> RadioData<Shared> {
    std::fs::write(dir.radio_toml(), format!("country = \"{code}\"\n"))
        .expect("the answered store is written");
    RadioData::new(
        Some(Client::new(Shared(canned))),
        Some(dir.radio_toml()),
        None,
    )
}

/// The bodies the region tests start from: two countries and two regions
/// of the home one.
fn region_canned() -> Canned {
    Canned::new()
        .countries(
            r#"[{"name":"Norway","iso_3166_1":"NO","stationcount":9},
                {"name":"Germany","iso_3166_1":"DE","stationcount":40}]"#,
        )
        .states(
            r#"[{"name":"Oslo","country":"Norway","stationcount":4},
                {"name":"Vestland","country":"Norway","stationcount":2}]"#,
        )
        .search(&stations_json(1))
}

#[test]
fn entering_the_country_section_fetches_the_home_states_and_only_those() {
    let dir = TempDir::new("home-states");
    let canned = Arc::new(region_canned());
    let mut data = wired_with_country(Arc::clone(&canned), &dir, "NO");
    let mut view = data.view();

    // Lazy even with a home country: opening the view and polling at the
    // top asks for nothing.
    for _ in 0..5 {
        data.poll(&mut view);
    }
    assert!(
        canned.calls().is_empty(),
        "opening the view with a home country makes zero requests, got: {:?}",
        canned.calls()
    );

    press(&mut view, KeyCode::Enter); // Browse all countries
    settle(&mut data, &mut view, &canned, |_view, canned| {
        canned.calls().iter().any(|url| url.contains("/states/"))
    });

    let calls = canned.calls();
    assert_eq!(
        calls.len(),
        2,
        "entering the section makes exactly two requests, got: {calls:?}"
    );
    assert!(calls[0].contains("/countries"), "got: {calls:?}");
    assert!(
        calls[1].contains("/states/NO/"),
        "the states index is fetched for the home country, got: {calls:?}"
    );

    // Back at the top the regions sit with the places.
    press(&mut view, KeyCode::Esc);
    let labels = view.labels();
    assert!(
        labels.contains(&"Norway › Oslo".to_owned())
            && labels.contains(&"Norway › Vestland".to_owned()),
        "the home country's regions are rows at the top, got: {labels:?}"
    );

    // Another country never asks for its states.
    press(&mut view, KeyCode::Enter); // Browse all countries (cached)
    press(&mut view, KeyCode::Char('j')); // Germany
    press(&mut view, KeyCode::Enter);
    settle(&mut data, &mut view, &canned, |view, canned| {
        !view.labels().is_empty()
            && canned
                .calls()
                .iter()
                .any(|url| url.contains("/stations/search"))
    });
    let search = canned
        .calls()
        .iter()
        .find(|url| url.contains("/stations/search"))
        .cloned()
        .expect("Germany's stations load");
    assert!(search.contains("countrycode=DE"), "got: {search}");
    assert!(
        !search.contains("state="),
        "no region search for another country, got: {search}"
    );
    let calls = canned.calls();
    let states: Vec<_> = calls
        .iter()
        .filter(|url| url.contains("/states/"))
        .collect();
    assert_eq!(
        states.len(),
        1,
        "never a second states index, got: {calls:?}"
    );
    assert!(states[0].contains("/states/NO/"), "got: {calls:?}");
}

#[test]
fn a_region_row_opens_that_region() {
    let dir = TempDir::new("region-open");
    let canned = Arc::new(region_canned());
    let mut data = wired_with_country(Arc::clone(&canned), &dir, "NO");
    let mut view = data.view();

    press(&mut view, KeyCode::Enter); // Browse all countries: index + states
    settle(&mut data, &mut view, &canned, |_view, canned| {
        canned.calls().iter().any(|url| url.contains("/states/"))
    });
    press(&mut view, KeyCode::Esc); // top, the regions now show
    press(&mut view, KeyCode::Char('j')); // past the near-you row
    press(&mut view, KeyCode::Char('j')); // onto Norway › Oslo
    press(&mut view, KeyCode::Enter);
    settle(&mut data, &mut view, &canned, |view, canned| {
        !view.labels().is_empty()
            && canned
                .calls()
                .iter()
                .any(|url| url.contains("/stations/search"))
    });

    assert_eq!(
        view.place(),
        &velo::ui::radio::Place::Country {
            code: String::from("NO"),
            name: String::from("Norway"),
            state: String::from("Oslo"),
        },
        "the region row opens the region, got: {:?}",
        view.place()
    );
    let search = canned
        .calls()
        .iter()
        .find(|url| url.contains("/stations/search"))
        .cloned()
        .expect("the region loads its stations");
    assert!(search.contains("countrycode=NO"), "got: {search}");
    assert!(search.contains("state=Oslo"), "got: {search}");
    assert!(search.contains("stateExact=true"), "got: {search}");
    assert_eq!(
        view.labels(),
        ["S0 [128k] · NO"],
        "the region's stations land in the view, got: {:?}",
        view.labels()
    );
}

#[test]
fn f_on_a_region_pins_it_and_the_pin_survives_a_reload() {
    let dir = TempDir::new("region-pin");
    let canned = Arc::new(region_canned());
    let mut data = wired_with_country(Arc::clone(&canned), &dir, "NO");
    let mut view = data.view();

    press(&mut view, KeyCode::Enter); // Browse all countries: index + states
    settle(&mut data, &mut view, &canned, |_view, canned| {
        canned.calls().iter().any(|url| url.contains("/states/"))
    });
    press(&mut view, KeyCode::Esc); // top
    press(&mut view, KeyCode::Char('j')); // past the near-you row
    press(&mut view, KeyCode::Char('j')); // onto Norway › Oslo
    press(&mut view, KeyCode::Char('f')); // pin the region
    data.poll(&mut view); // the save ask

    let saved = std::fs::read_to_string(dir.radio_toml()).expect("the pin save created the file");
    assert!(
        saved.contains("[[pinned]]")
            && saved.contains(r#"code = "NO""#)
            && saved.contains(r#"name = "Norway""#)
            && saved.contains(r#"state = "Oslo""#),
        "the pin carries the region, got: {saved}"
    );

    // A restart: a fresh wiring over the same file shows the pinned
    // region as its own row, and the row opens the region's stations.
    let reloaded = RadioData::new(
        Some(Client::new(Shared(Arc::clone(&canned)))),
        Some(dir.radio_toml()),
        None,
    );
    let mut view = reloaded.view();
    let labels = view.labels();
    assert!(
        labels.contains(&"★ Norway · Oslo".to_owned()),
        "the pinned region survives the reload, got: {labels:?}"
    );
    press(&mut view, KeyCode::Char('j')); // past the near-you row
    press(&mut view, KeyCode::Char('j')); // onto ★ Norway · Oslo
    press(&mut view, KeyCode::Enter);
    settle(&mut data, &mut view, &canned, |_view, canned| {
        canned
            .calls()
            .iter()
            .any(|url| url.contains("/stations/search"))
    });
    let search = canned
        .calls()
        .iter()
        .find(|url| url.contains("/stations/search"))
        .cloned()
        .expect("the pinned region loads its stations");
    assert!(
        search.contains("countrycode=NO")
            && search.contains("state=Oslo")
            && search.contains("stateExact=true"),
        "the pinned region opens its region, got: {search}"
    );
}

#[test]
fn with_no_home_country_no_states_are_ever_requested() {
    let canned = Arc::new(
        Canned::new().countries(r#"[{"name":"Norway","iso_3166_1":"NO","stationcount":9}]"#),
    );

    // Declined: `country = "none"`.
    let dir = TempDir::new("states-none");
    std::fs::write(dir.radio_toml(), "country = \"none\"\n").expect("the decline is written");
    let mut data = RadioData::new(
        Some(Client::new(Shared(Arc::clone(&canned)))),
        Some(dir.radio_toml()),
        None,
    );
    let mut view = data.view();
    press(&mut view, KeyCode::Enter); // Browse all countries
    settle(&mut data, &mut view, &canned, |view, _canned| {
        !view.labels().is_empty()
    });
    assert!(
        !canned.calls().iter().any(|url| url.contains("/states/")),
        "a declined country never fetches states, got: {:?}",
        canned.calls()
    );
    press(&mut view, KeyCode::Esc);
    assert!(
        !view.labels().iter().any(|label| label.contains('›')),
        "a declined country shows no region rows, got: {:?}",
        view.labels()
    );

    // Never asked and nothing detected: the same silence.
    let mut data = wired(Arc::clone(&canned));
    let mut view = data.view();
    press(&mut view, KeyCode::Enter);
    settle(&mut data, &mut view, &canned, |view, _canned| {
        !view.labels().is_empty()
    });
    assert!(
        !canned.calls().iter().any(|url| url.contains("/states/")),
        "no home country, no states request, got: {:?}",
        canned.calls()
    );
    press(&mut view, KeyCode::Esc);
    assert!(
        !view.labels().iter().any(|label| label.contains('›')),
        "no home country, no region rows, got: {:?}",
        view.labels()
    );
}

#[test]
fn a_failing_states_request_reports_once_and_leaves_the_countries_usable() {
    let dir = TempDir::new("states-fail");
    let canned = Arc::new(
        Canned::new()
            .countries(r#"[{"name":"Norway","iso_3166_1":"NO","stationcount":9}]"#)
            .failing_states(),
    );
    let mut data = wired_with_country(Arc::clone(&canned), &dir, "NO");
    let mut view = data.view();

    press(&mut view, KeyCode::Enter); // Browse all countries
    settle(&mut data, &mut view, &canned, |_view, canned| {
        canned.calls().len() == 2
    });

    let message = data
        .take_message()
        .expect("the states failure reaches the status line");
    assert!(
        message.contains("could not be reached"),
        "the status names the failure, got: {message}"
    );
    assert_eq!(data.take_message(), None, "the states failure reports once");

    assert_eq!(
        view.labels(),
        ["Norway"],
        "the country list is still usable, got: {:?}",
        view.labels()
    );
    for _ in 0..5 {
        data.poll(&mut view);
    }
    assert_eq!(
        canned.calls().len(),
        2,
        "the failure does not retry, got: {:?}",
        canned.calls()
    );
}

// ---- the stations without a country (S4) ------------------------------------

#[test]
fn the_no_country_section_never_requests_the_directory() {
    let dir = TempDir::new("no-country");
    std::fs::write(
        dir.radio_toml(),
        "[[station]]\n\
         name = \"Night FM\"\n\
         url = \"http://night.example/live\"\n\
         \n\
         [[favorite]]\n\
         name = \"Free Radio\"\n\
         url = \"https://free.example/stream\"\n\
         countrycode = \"\"\n\
         codec = \"\"\n",
    )
    .expect("the store is written");
    let canned = Arc::new(Canned::new());
    let mut data = RadioData::new(
        Some(Client::new(Shared(Arc::clone(&canned)))),
        Some(dir.radio_toml()),
        None,
    );
    let mut view = data.view();

    let section = view
        .labels()
        .iter()
        .position(|label| label == "Stations without a country")
        .expect("the no-country section is listed");
    for _ in 0..section {
        press(&mut view, KeyCode::Char('j'));
    }
    press(&mut view, KeyCode::Enter);
    for _ in 0..5 {
        data.poll(&mut view);
    }

    assert_eq!(
        view.place(),
        &velo::ui::radio::Place::NoCountry,
        "the section is its own place, got: {:?}",
        view.place()
    );
    assert_eq!(
        view.labels(),
        ["Night FM", "★ Free Radio"],
        "the store's own rows land in the view, got: {:?}",
        view.labels()
    );
    assert!(
        canned.calls().is_empty(),
        "an empty or a full no-country section asks the directory for nothing, got: {:?}",
        canned.calls()
    );

    // Ctrl+R there re-requests nothing either: there is no request to
    // refresh.
    let _ = view.key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    data.poll(&mut view);
    assert!(
        canned.calls().is_empty(),
        "ctrl-r in the no-country place asks for nothing, got: {:?}",
        canned.calls()
    );
}

// ---- a place shows its own stations, never another's (T6, S9, #20) ---------

#[test]
fn coming_back_to_a_loaded_place_shows_the_cache_without_a_request() {
    let canned = Arc::new(
        Canned::new()
            .countries(r#"[{"name":"Norway","iso_3166_1":"NO","stationcount":2}]"#)
            .search(&stations_json(2)),
    );
    let mut data = wired(Arc::clone(&canned));
    let mut view = data.view();

    press(&mut view, KeyCode::Enter); // Browse all countries
    settle(&mut data, &mut view, &canned, |view, _canned| {
        !view.labels().is_empty()
    });
    press(&mut view, KeyCode::Enter); // Norway
    settle(&mut data, &mut view, &canned, |view, canned| {
        !view.labels().is_empty()
            && canned
                .calls()
                .iter()
                .any(|url| url.contains("/stations/search"))
    });
    assert_eq!(
        view.labels(),
        ["S0 [128k] · NO", "S1 [128k] · NO"],
        "Norway shows its stations, got: {:?}",
        view.labels()
    );

    press(&mut view, KeyCode::Esc); // back to the country index
    assert_eq!(
        view.place(),
        &velo::ui::radio::Place::Countries,
        "one esc is one level, got: {:?}",
        view.place()
    );
    press(&mut view, KeyCode::Esc); // back to the top
    assert_eq!(view.place(), &velo::ui::radio::Place::Top);
    for label in view.labels() {
        assert!(
            !label.contains("S0") && !label.contains("S1"),
            "the top lists sections, never station rows, got: {label:?}"
        );
    }

    // Back into Norway: the cache answers again, and no second request is
    // made for a place already loaded this session.
    press(&mut view, KeyCode::Enter); // Browse all countries (cached)
    press(&mut view, KeyCode::Enter); // Norway
    settle(&mut data, &mut view, &canned, |view, canned| {
        view.labels() == ["S0 [128k] · NO".to_owned(), "S1 [128k] · NO".to_owned()]
            && canned.calls().len() == 2
    });
    assert_eq!(
        canned
            .calls()
            .iter()
            .filter(|url| url.contains("/stations/search"))
            .count(),
        1,
        "coming back re-uses the cached answer, got: {:?}",
        canned.calls()
    );
}

// ---- a wait says so (T7, S10, #21) ------------------------------------------

/// The Norway place the fixtures stand in.
fn norway_place() -> velo::ui::radio::Place {
    velo::ui::radio::Place::Country {
        code: String::from("NO"),
        name: String::from("Norway"),
        state: String::new(),
    }
}

#[test]
fn entering_a_place_shows_the_waiting_fact_until_its_answer_lands() {
    let canned = Arc::new(
        Canned::new()
            .countries(r#"[{"name":"Norway","iso_3166_1":"NO","stationcount":1}]"#)
            .search(&stations_json(1)),
    );
    let gate = Arc::new(Gate::closed());
    let mut data = RadioData::new(
        Some(Client::new(Gated {
            canned: Arc::clone(&canned),
            gate: Arc::clone(&gate),
        })),
        None,
        None,
    );
    let mut view = data.view();

    press(&mut view, KeyCode::Enter); // Browse all countries
    settle(&mut data, &mut view, &canned, |view, canned| {
        !view.labels().is_empty() && canned.calls().len() == 1
    });
    assert_eq!(
        view.pending_place(),
        None,
        "the index answered; nothing waits any more"
    );

    // Norway's search is held by the gate: the wiring says the view is
    // waiting for exactly the place on screen.
    press(&mut view, KeyCode::Enter); // Norway
    data.poll(&mut view);
    assert!(data.loading(), "the search is in flight");
    assert_eq!(
        view.pending_place(),
        Some(&norway_place()),
        "the waiting fact names the place on screen"
    );
    assert!(view.labels().is_empty(), "no answer yet");

    // The gate opens, the answer lands, the waiting fact goes away.
    gate.open();
    settle(&mut data, &mut view, &canned, |view, canned| {
        !view.labels().is_empty()
            && canned
                .calls()
                .iter()
                .any(|url| url.contains("/stations/search"))
    });
    assert_eq!(
        view.pending_place(),
        None,
        "the answer landed; the waiting fact is gone"
    );
    assert!(!data.loading());
}

#[test]
fn ctrl_r_shows_the_waiting_fact_again_and_the_answer_clears_it() {
    let canned = Arc::new(
        Canned::new()
            .countries(r#"[{"name":"Norway","iso_3166_1":"NO","stationcount":1}]"#)
            .search(&stations_json(1)),
    );
    let gate = Arc::new(Gate::closed());
    let mut data = RadioData::new(
        Some(Client::new(Gated {
            canned: Arc::clone(&canned),
            gate: Arc::clone(&gate),
        })),
        None,
        None,
    );
    let mut view = data.view();

    press(&mut view, KeyCode::Enter); // Browse all countries
    settle(&mut data, &mut view, &canned, |view, _canned| {
        !view.labels().is_empty()
    });
    press(&mut view, KeyCode::Enter); // Norway
    gate.open();
    settle(&mut data, &mut view, &canned, |view, _canned| {
        !view.labels().is_empty()
    });
    assert_eq!(view.pending_place(), None, "nothing is in flight");

    // The gate shut again: ctrl-r re-requests the place on screen, and the
    // waiting fact is shown again.
    gate.close();
    let _ = view.key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    data.poll(&mut view);
    assert!(data.loading(), "the refresh is in flight");
    assert_eq!(
        view.pending_place(),
        Some(&norway_place()),
        "ctrl-r shows the waiting fact again"
    );

    gate.open();
    settle(&mut data, &mut view, &canned, |_view, canned| {
        canned
            .calls()
            .iter()
            .filter(|url| url.contains("/stations/search"))
            .count()
            == 2
    });
    assert_eq!(
        view.pending_place(),
        None,
        "the refreshed answer clears the waiting fact"
    );
}

#[test]
fn a_failed_request_clears_the_waiting_fact() {
    let canned = Arc::new(Canned::new().failing());
    let mut data = wired(Arc::clone(&canned));
    let mut view = data.view();

    press(&mut view, KeyCode::Enter); // Browse all countries
    settle(&mut data, &mut view, &canned, |_view, canned| {
        !canned.calls().is_empty()
    });

    assert_eq!(
        view.pending_place(),
        None,
        "the error landed; the waiting fact is gone with it"
    );
    let message = data
        .take_message()
        .expect("the failure reaches the status line");
    assert!(
        message.contains("could not be reached"),
        "the status names the failure, got: {message}"
    );
}

// ---- the footer stops lying about loading (S10, #21) -----------------------

#[test]
fn the_footer_stops_saying_loading_once_the_answer_lands() {
    let canned = Arc::new(
        Canned::new()
            .countries(r#"[{"name":"Norway","iso_3166_1":"NO","stationcount":1}]"#)
            .search(&stations_json(1)),
    );
    let gate = Arc::new(Gate::closed());
    let mut app = App::new(Vec::new());
    app.set_radio_wire(Box::new(RadioData::new(
        Some(Client::new(Gated {
            canned: Arc::clone(&canned),
            gate: Arc::clone(&gate),
        })),
        None,
        None,
    )));

    let _ = app.on_key(KeyEvent::from(KeyCode::Char('R'))); // the radio opens over the wiring
    let _ = app.on_key(KeyEvent::from(KeyCode::Enter)); // Browse all countries: the request starts
    assert_eq!(
        app.status(),
        "⟳ loading…",
        "the footer says loading while the request is in flight"
    );

    // The answer lands through the tick, the way the event loop collects
    // it: the footer must stop saying loading — today it sticks.
    gate.open();
    for _ in 0..400 {
        app.tick();
        if app.status() != "⟳ loading…" {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_ne!(
        app.status(),
        "⟳ loading…",
        "the answer landed; the footer must not keep saying loading"
    );
    assert_eq!(app.status(), "", "nothing replaced it, so it is empty");

    // A place's station search says the same and stops the same way.
    let _ = app.on_key(KeyEvent::from(KeyCode::Enter)); // Norway's stations
    assert_eq!(
        app.status(),
        "⟳ loading…",
        "the search says loading while it is held in flight"
    );
    gate.open();
    for _ in 0..400 {
        app.tick();
        if app.status() != "⟳ loading…" {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        app.status(),
        "",
        "the search's answer lands, the footer clears again"
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
    settle(&mut data, &mut view, &canned, |view, _canned| {
        !view.labels().is_empty()
    });
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
    settle(&mut data, &mut view, &canned, |view, _canned| {
        !view.labels().is_empty()
    });
    press(&mut view, KeyCode::Enter); // Norway's stations
    settle(&mut data, &mut view, &canned, |view, _canned| {
        view.labels().len() == 1
    });

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
        view.labels()[1],
        "Use my location",
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
