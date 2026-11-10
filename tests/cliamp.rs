//! The cliamp client (issue #48): the three request shapes parsed over the
//! directory's `Transport` seam.
//!
//! No network and no new JSON crate: the client runs over a canned
//! transport, the way `tests/radio_directory.rs` tests the Radio Browser
//! client, and `miniserde` parses the shapes.

use velo::radio::cliamp::{Client, HOST};
use velo::radio::client::{Error, Transport};

/// A transport with one canned body per endpoint and a call log.
struct Canned {
    urls: std::cell::RefCell<Vec<String>>,
    stations: String,
    statistics: String,
    tracks: String,
}

impl Canned {
    fn new() -> Self {
        Self {
            urls: std::cell::RefCell::new(Vec::new()),
            stations: String::from(r#"{"stations":[]}"#),
            statistics: String::from(r#"{"stations":{}}"#),
            tracks: String::from(r#"{"station":"lofi","name":"","count":0,"tracks":[]}"#),
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

    fn urls(&self) -> Vec<String> {
        self.urls.borrow().clone()
    }
}

impl Transport for Canned {
    fn get(&self, url: &str) -> Result<String, Error> {
        self.urls.borrow_mut().push(url.to_owned());
        if url.contains("/statistics") {
            Ok(self.statistics.clone())
        } else if url.contains("/tracks") {
            Ok(self.tracks.clone())
        } else {
            Ok(self.stations.clone())
        }
    }
}

/// A transport that never answers: the failure's one-line text is what the
/// tests read back. Tests never touch the network.
struct Refusing;

impl Transport for Refusing {
    fn get(&self, _url: &str) -> Result<String, Error> {
        Err(Error::Unreachable {
            detail: "connection refused".to_owned(),
        })
    }
}

/// The channel list the live service answers with: two of the fifteen.
const STATIONS: &str = r#"{"stations":[
    {"id":"lofi","name":"LoFi Beats","description":"","genre":"",
     "stream":"https://radio.cliamp.stream/lofi/stream","tracks":120,
     "tracks_url":"https://radio.cliamp.stream/lofi/tracks"},
    {"id":"jazz","name":"Jazz Cafe","description":"","genre":"",
     "stream":"https://radio.cliamp.stream/jazz/stream","tracks":12,
     "tracks_url":"https://radio.cliamp.stream/jazz/tracks"}
]}"#;

/// The statistics answer: listeners per station, more fields beside the
/// one parsed.
const STATISTICS: &str = r#"{"stations":{
    "lofi":{"active_listeners":3,"genre":"lofi"},
    "jazz":{"active_listeners":0},
    "solo":{"genre":"no count at all"}
}}"#;

/// The songs answer of one channel, in the service's order.
const TRACKS: &str = r#"{"station":"lofi","name":"LoFi Beats","count":2,"tracks":[
    {"id":"t1","title":"Dawn","artist":"Tide","album":"Harbor",
     "filename":"t1.mp3","url":"https://radio.cliamp.stream/lofi/tracks/t1"},
    {"id":"t2","title":"Dusk","artist":"Sable","album":"Views",
     "filename":"t2.mp3","url":"https://radio.cliamp.stream/lofi/tracks/t2"}
]}"#;

#[test]
fn the_channels_parse_with_their_track_counts() {
    let client = Client::new(Canned::new().stations(STATIONS));
    let channels = client.stations().expect("the channels parse");
    assert_eq!(channels.len(), 2, "got: {channels:?}");
    assert_eq!(channels[0].id, "lofi");
    assert_eq!(channels[0].name, "LoFi Beats");
    assert_eq!(channels[0].tracks, 120);
    assert_eq!(channels[1].id, "jazz");
    assert_eq!(channels[1].tracks, 12);
}

#[test]
fn the_stations_url_is_the_constant_host() {
    let client = Client::new(Canned::new().stations(STATIONS));
    let _ = client.stations();
    assert_eq!(
        client.transport.urls(),
        [format!("https://{HOST}/stations")],
        "one GET of /stations at the one host, got: {:?}",
        client.transport.urls()
    );
}

#[test]
fn the_statistics_parse_the_listeners_per_station() {
    let client = Client::new(Canned::new().statistics(STATISTICS));
    let listeners = client.statistics().expect("the statistics parse");
    // The rows come back by id (the answer's map is walked in key order);
    // the order is not meaningful, the consumers look rows up by id.
    let mut sorted = listeners.clone();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(sorted, listeners, "the rows are keyed by id");
    let by_id = |id: &str| {
        listeners
            .iter()
            .find(|row| row.id == id)
            .unwrap_or_else(|| panic!("{id} in the answer"))
            .active
    };
    assert_eq!(by_id("lofi"), 3);
    assert_eq!(by_id("jazz"), 0);
    // A station without a count counts nobody, and its row still
    // exists: the absence of a number is zero, never a failure.
    assert_eq!(by_id("solo"), 0);
    assert_eq!(listeners.len(), 3, "every station the answer named");
}

#[test]
fn the_songs_parse_in_the_service_order() {
    let client = Client::new(Canned::new().tracks(TRACKS));
    let songs = client.songs("lofi").expect("the songs parse");
    assert_eq!(songs.len(), 2, "got: {songs:?}");
    assert_eq!(songs[0].title, "Dawn");
    assert_eq!(songs[0].artist, "Tide");
    assert_eq!(songs[0].album, "Harbor");
    assert_eq!(songs[0].url, "https://radio.cliamp.stream/lofi/tracks/t1");
    assert_eq!(songs[1].title, "Dusk", "the service's order is kept");
}

#[test]
fn the_songs_url_names_the_channel() {
    let client = Client::new(Canned::new().tracks(TRACKS));
    let _ = client.songs("lofi");
    assert_eq!(
        client.transport.urls(),
        [format!("https://{HOST}/lofi/tracks")],
        "one GET of the channel's tracks, got: {:?}",
        client.transport.urls()
    );
}

#[test]
fn a_failing_request_names_the_host() {
    let client = Client::new(Refusing);
    let error = client.stations().expect_err("the transport refused");
    assert_eq!(
        error,
        Error::Unreachable {
            detail: format!("{HOST} could not be reached: connection refused"),
        },
        "the one-line reason names where the request went"
    );
}

#[test]
fn a_malformed_body_is_a_malformed_error() {
    let client = Client::new(Canned::new().stations("not json"));
    let error = client.stations().expect_err("the body is not JSON");
    assert!(matches!(error, Error::Malformed { .. }), "got: {error:?}");
}

#[test]
fn a_song_without_an_artist_or_an_album_parses() {
    // The service omits `artist`, `album` and `filename` for songs it has no
    // metadata for — dozens per channel (issue #55). A missing field is an
    // empty one, never a malformed answer.
    let canned = Canned::new().tracks(
        r#"{"station":"lofi","name":"Lofi","count":2,"tracks":[
            {"id":"a","title":"City Lofi","filename":"a.mp3","url":"https://x/a.mp3"},
            {"id":"b","title":"Summer","artist":"AvapXia","album":"summer lofi","url":"https://x/b.mp3"}]}"#,
    );
    let client = Client::new(canned);

    let songs = client.songs("lofi").expect("the answer parses");

    assert_eq!(songs.len(), 2, "both songs arrive");
    assert_eq!(songs[0].title, "City Lofi");
    assert_eq!(songs[0].artist, "", "an absent artist is empty");
    assert_eq!(songs[0].album, "");
    assert_eq!(songs[0].url, "https://x/a.mp3", "the URL is the identity");
    assert_eq!(songs[1].artist, "AvapXia");
    assert_eq!(songs[1].album, "summer lofi");
}
