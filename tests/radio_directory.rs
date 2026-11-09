//! Radio Browser client and directory tests (specs S1 and S8): the query
//! string is the contract, pinned parameter by parameter; the wire JSON is
//! parsed leniently and never panics; stations that cannot load are skipped
//! before they reach a queue; the codec filter answers from symphonia's
//! compiled-in codecs; and an HLS manifest is unplayable however the codec
//! is spelled.
//!
//! All fixtures are string literals in this file: no network, no files
//! outside the repo, no new dependencies.

use std::cell::RefCell;

use velo::radio::client::{self, Client, Error, Order, StationSearch, Transport};
use velo::radio::directory::{
    is_playable_codec, is_playable_station, loadable_stations, refusal, refusal_for,
};

/// A canned transport: records every URL it is asked to fetch and always
/// answers with one fixed body. Tests never touch the network.
struct Recorder {
    body: String,
    urls: RefCell<Vec<String>>,
}

impl Recorder {
    fn serving(body: &str) -> Self {
        Self {
            body: body.to_owned(),
            urls: RefCell::new(Vec::new()),
        }
    }

    fn last_url(&self) -> String {
        self.urls
            .borrow()
            .last()
            .cloned()
            .expect("the transport was called at least once")
    }
}

impl Transport for Recorder {
    fn get(&self, url: &str) -> Result<String, Error> {
        self.urls.borrow_mut().push(url.to_owned());
        Ok(self.body.clone())
    }
}

const BASE: &str = "https://de1.api.radio-browser.info/json";

/// Two real-shaped stations with every field the contract names plus the
/// unknown fields the API actually sends (ignored, never an error).
const STATIONS_JSON: &str = r#"[
    {
        "stationuuid": "960c1d80-c79d-4d3d-9f6d-cd0dc9a1d6a1",
        "name": "Radio Nova",
        "url": "http://novaplanet.radioca.st/stream",
        "url_resolved": "https://stream.example.com/nova.mp3",
        "homepage": "https://www.radionova.fr",
        "country": "Norway",
        "countrycode": "NO",
        "state": "Oslo",
        "tags": "jazz,eclectic",
        "codec": "MP3",
        "bitrate": 192,
        "votes": 4321,
        "lastcheckok": 1,
        "clickcount": 9876,
        "iso_3166_2": "NO-03",
        "geo_lat": 59.91,
        "geo_long": 10.75
    },
    {
        "stationuuid": "0b1b6d1e-2c3f-4a5b-8c9d-0e1f2a3b4c5d",
        "name": "NCS Chill",
        "url_resolved": "https://radio.cliamp.stream/ncs-chill/stream",
        "country": "",
        "countrycode": "",
        "state": "",
        "tags": "chill",
        "codec": "AAC",
        "bitrate": 128,
        "votes": 12,
        "homepage": ""
    }
]"#;

const COUNTRIES_JSON: &str = r#"[
    {"name": "Norway", "iso_3166_1": "NO", "stationcount": 812, "wikipedia": ""},
    {"name": "Georgia", "iso_3166_1": "GE", "stationcount": 64}
]"#;

const STATES_JSON: &str = r#"[
    {"name": "Oslo", "country": "Norway", "stationcount": 210, "iso_3166_2": "NO-03"},
    {"name": "Vestland", "country": "Norway", "stationcount": 45}
]"#;

const TAGS_JSON: &str = r#"[
    {"name": "jazz", "stationcount": 3021, "description": null},
    {"name": "rock", "stationcount": 2890}
]"#;

fn station_json(name: &str, url_resolved: &str) -> String {
    format!(
        r#"{{"name": "{name}", "url_resolved": "{url_resolved}", "country": "Norway", "countrycode": "NO", "state": "", "tags": "", "codec": "MP3", "bitrate": 128, "votes": 1, "homepage": ""}}"#
    )
}

// --- The query string is the contract ------------------------------------

#[test]
fn default_search_query_is_pinned() {
    let recorder = Recorder::serving("[]");
    let client = Client::new(recorder);
    let _ = client.search_stations(&StationSearch::default());
    assert_eq!(
        client.transport.last_url(),
        format!(
            "{BASE}/stations/search?limit=200&offset=0&order=votes&reverse=true&hidebroken=true"
        )
    );
}

#[test]
fn every_order_reverses_except_name_and_random() {
    let cases = [
        (Order::Votes, "votes", true),
        (Order::ClickCount, "clickcount", true),
        (Order::ClickTrend, "clicktrend", true),
        (Order::Name, "name", false),
        (Order::Random, "random", false),
    ];
    for (order, param, reverse) in cases {
        let recorder = Recorder::serving("[]");
        let client = Client::new(recorder);
        let search = StationSearch {
            order,
            ..StationSearch::default()
        };
        let _ = client.search_stations(&search);
        let expected = if reverse {
            format!(
                "{BASE}/stations/search?limit=200&offset=0&order={param}&reverse=true&hidebroken=true"
            )
        } else {
            format!("{BASE}/stations/search?limit=200&offset=0&order={param}&hidebroken=true")
        };
        assert_eq!(client.transport.last_url(), expected, "order {param}");
    }
}

#[test]
fn filters_carry_the_exact_flags() {
    let recorder = Recorder::serving("[]");
    let client = Client::new(recorder);
    let search = StationSearch {
        name: Some("georgia".to_owned()),
        tag: Some("jazz".to_owned()),
        countrycode: Some("NO".to_owned()),
        state: Some("oslo".to_owned()),
        ..StationSearch::default()
    };
    let _ = client.search_stations(&search);
    assert_eq!(
        client.transport.last_url(),
        format!(
            "{BASE}/stations/search?limit=200&offset=0&order=votes&reverse=true&hidebroken=true\
&name=georgia&tag=jazz&tagExact=true&countrycode=NO&state=oslo&stateExact=true"
        )
    );
}

#[test]
fn query_values_are_percent_encoded() {
    let recorder = Recorder::serving("[]");
    let client = Client::new(recorder);
    let search = StationSearch {
        name: Some("big & blue".to_owned()),
        ..StationSearch::default()
    };
    let _ = client.search_stations(&search);
    assert!(
        client
            .transport
            .last_url()
            .contains("&name=big%20%26%20blue")
    );
}

#[test]
fn offset_and_limit_override_the_defaults() {
    let recorder = Recorder::serving("[]");
    let client = Client::new(recorder);
    let search = StationSearch {
        offset: 400,
        limit: 50,
        ..StationSearch::default()
    };
    let _ = client.search_stations(&search);
    assert!(
        client
            .transport
            .last_url()
            .contains("?limit=50&offset=400&")
    );
}

#[test]
fn the_user_agent_is_velo_and_the_crate_version() {
    assert_eq!(
        client::USER_AGENT,
        concat!("velo/", env!("CARGO_PKG_VERSION"))
    );
}

// --- The index endpoints --------------------------------------------------

#[test]
fn countries_are_parsed_and_their_url_is_pinned() {
    let recorder = Recorder::serving(COUNTRIES_JSON);
    let client = Client::new(recorder);
    let countries = client.countries().expect("countries parse");
    assert_eq!(client.transport.last_url(), format!("{BASE}/countries"));
    assert_eq!(countries.len(), 2);
    assert_eq!(countries[0].name, "Norway");
    assert_eq!(countries[0].iso_3166_1, "NO");
    assert_eq!(countries[0].stationcount, 812);
    assert_eq!(countries[1].iso_3166_1, "GE");
}

#[test]
fn states_are_parsed_from_the_per_country_url() {
    let recorder = Recorder::serving(STATES_JSON);
    let client = Client::new(recorder);
    let states = client.states("NO").expect("states parse");
    assert_eq!(client.transport.last_url(), format!("{BASE}/states/NO/"));
    assert_eq!(states.len(), 2);
    assert_eq!(states[0].name, "Oslo");
    assert_eq!(states[0].country, "Norway");
    assert_eq!(states[0].stationcount, 210);
}

#[test]
fn tags_are_parsed_from_the_pinned_url() {
    let recorder = Recorder::serving(TAGS_JSON);
    let client = Client::new(recorder);
    let tags = client.tags().expect("tags parse");
    assert_eq!(
        client.transport.last_url(),
        format!("{BASE}/tags?order=stationcount&reverse=true&hidebroken=true&limit=100000")
    );
    assert_eq!(tags.len(), 2);
    assert_eq!(tags[0].name, "jazz");
    assert_eq!(tags[0].stationcount, 3021);
}

// --- Parsing is lenient or fails, never panics ----------------------------

#[test]
fn a_station_search_maps_the_wire_fields() {
    let recorder = Recorder::serving(STATIONS_JSON);
    let client = Client::new(recorder);
    let stations = client
        .search_stations(&StationSearch::default())
        .expect("stations parse");
    assert_eq!(stations.len(), 2);
    assert_eq!(stations[0].name, "Radio Nova");
    assert_eq!(
        stations[0].url_resolved,
        "https://stream.example.com/nova.mp3"
    );
    assert_eq!(stations[0].country, "Norway");
    assert_eq!(stations[0].countrycode, "NO");
    assert_eq!(stations[0].state, "Oslo");
    assert_eq!(stations[0].tags, "jazz,eclectic");
    assert_eq!(stations[0].codec, "MP3");
    assert_eq!(stations[0].bitrate, 192);
    assert_eq!(stations[0].votes, 4321);
    assert_eq!(stations[0].homepage, "https://www.radionova.fr");
    assert_eq!(stations[1].name, "NCS Chill");
}

#[test]
fn malformed_json_is_an_error_not_a_panic() {
    let recorder = Recorder::serving("{\"name\": \"truncated");
    let client = Client::new(recorder);
    let result = client.search_stations(&StationSearch::default());
    assert!(matches!(result, Err(Error::Malformed { .. })));
}

#[test]
fn an_empty_array_is_zero_stations() {
    let recorder = Recorder::serving("[]");
    let client = Client::new(recorder);
    let stations = client
        .search_stations(&StationSearch::default())
        .expect("empty array");
    assert_eq!(stations, [] as [velo::radio::client::Station; 0]);
}

// --- Stations that cannot load are skipped --------------------------------

#[test]
fn a_station_with_an_empty_url_resolved_is_skipped() {
    let recorder = Recorder::serving(&format!(
        "[{},{}]",
        station_json("Broken", ""),
        station_json("Good", "https://stream.example.com/good")
    ));
    let client = Client::new(recorder);
    let parsed = client
        .search_stations(&StationSearch::default())
        .expect("parse");
    let loadable = loadable_stations(parsed);
    assert_eq!(loadable.len(), 1);
    assert_eq!(loadable[0].name, "Good");
}

#[test]
fn a_station_with_a_relative_url_is_skipped() {
    let recorder = Recorder::serving(&format!(
        "[{},{}]",
        station_json("Relative", "/stream/live"),
        station_json("Good", "http://stream.example.com/good")
    ));
    let client = Client::new(recorder);
    let parsed = client
        .search_stations(&StationSearch::default())
        .expect("parse");
    let loadable = loadable_stations(parsed);
    assert_eq!(loadable.len(), 1);
    assert_eq!(loadable[0].name, "Good");
}

#[test]
fn a_non_http_scheme_is_skipped() {
    let recorder = Recorder::serving(&format!(
        "[{},{}]",
        station_json("Ftp", "ftp://stream.example.com/good"),
        station_json("Good", "https://stream.example.com/good")
    ));
    let client = Client::new(recorder);
    let parsed = client
        .search_stations(&StationSearch::default())
        .expect("parse");
    let loadable = loadable_stations(parsed);
    assert_eq!(loadable.len(), 1);
    assert_eq!(loadable[0].name, "Good");
}

#[test]
fn at_most_two_hundred_stations_load() {
    let stations: Vec<String> = (0..201)
        .map(|index| {
            station_json(
                &format!("Station {index}"),
                "https://stream.example.com/live",
            )
        })
        .collect();
    let recorder = Recorder::serving(&format!("[{}]", stations.join(",")));
    let client = Client::new(recorder);
    let parsed = client
        .search_stations(&StationSearch::default())
        .expect("parse");
    assert_eq!(parsed.len(), 201);
    let loadable = loadable_stations(parsed);
    assert_eq!(loadable.len(), 200);
    assert_eq!(loadable[0].name, "Station 0");
    assert_eq!(loadable[199].name, "Station 199");
}

// --- The codec filter answers from symphonia, not a hardcoded list --------

#[test]
fn codecs_symphonia_compiled_in_are_playable() {
    assert!(is_playable_codec("mp3"));
    assert!(is_playable_codec("MP3"));
    assert!(is_playable_codec("aac"));
    assert!(is_playable_codec("flac"));
    assert!(is_playable_codec("MP3,H.264"));
}

#[test]
fn the_table_answers_what_the_build_can_decode_and_holds_no_opinion_of_its_own() {
    use symphonia::core::codecs::audio::well_known::CODEC_ID_OPUS;

    // The invariant, not a snapshot: compiling a decoder must flip this table
    // without the table changing. A hardcoded `true` or `false` for opus fails
    // one half of this on the day T14 lands.
    let opus = velo::audio::opus::decodable(CODEC_ID_OPUS);
    assert_eq!(
        is_playable_codec("opus"),
        opus,
        "the opus row follows the build"
    );
    assert_eq!(
        is_playable_codec("OPUS,mp4"),
        opus,
        "a multi-part codec string follows the build too"
    );
}

#[test]
fn an_unrecognised_codec_is_allowed_because_the_directory_often_lies() {
    // Hiding a station that would have played is worse than one decode error
    // that names the codec.
    assert!(is_playable_codec(""));
    assert!(is_playable_codec("unknown"));
    assert!(is_playable_codec("h.264"));
}

// --- An HLS manifest is unplayable, whatever the codec says -----------------

/// One parsed station with codec `MP3` and the resolved URL asked for: the
/// directory JSON is the only way stations exist here.
fn one_station(url_resolved: &str) -> client::Station {
    let recorder = Recorder::serving(&format!("[{}]", station_json("Station", url_resolved)));
    let client = Client::new(recorder);
    let mut stations = client
        .search_stations(&StationSearch::default())
        .expect("parse");
    assert_eq!(stations.len(), 1);
    stations.remove(0)
}

#[test]
fn an_hls_manifest_is_not_playable_even_when_the_codec_is_fine() {
    let station = one_station("https://stream.example/live/index.m3u8");
    assert_eq!(station.codec, "MP3", "the codec alone would play");
    assert!(!is_playable_station(&station));
}

#[test]
fn an_hls_manifest_with_a_query_or_fragment_is_not_playable_either() {
    let token = one_station("https://stream.example/live/index.m3u8?token=abc&exp=1");
    assert!(!is_playable_station(&token));
    let anchored = one_station("https://stream.example/live/index.M3U8#list");
    assert!(!is_playable_station(&anchored));
}

#[test]
fn an_ordinary_stream_url_plays_whatever_the_extension_hints() {
    // The decision is `.m3u8`-shaped, not suspicious-of-everything: an mp3
    // path, an m3u path (a wrapper the stream layer unwraps) and no
    // extension at all all stay playable.
    for url in [
        "https://stream.example/nova.mp3",
        "https://stream.example/live/list.m3u",
        "https://stream.example/live",
    ] {
        assert!(is_playable_station(&one_station(url)), "url {url}");
    }
}

#[test]
fn the_hls_decision_is_per_station_and_leaves_no_trace() {
    // One list, one manifest, one plain stream: each answers for itself,
    // and deciding against the manifest cannot mark anything else — there
    // is no global state to stick.
    let recorder = Recorder::serving(&format!(
        "[{},{}]",
        station_json("Hls One", "https://stream.example/live/index.m3u8"),
        station_json("Nova", "https://stream.example/nova.mp3"),
    ));
    let client = Client::new(recorder);
    let stations = client
        .search_stations(&StationSearch::default())
        .expect("parse");
    assert_eq!(stations.len(), 2);
    assert!(!is_playable_station(&stations[0]), "the manifest");
    assert!(is_playable_station(&stations[1]), "the plain stream");
    // And again, in the other order of asking: the manifest still refuses.
    assert!(!is_playable_station(&stations[0]));
}

#[test]
fn the_directory_rule_composes_the_codec_rule_and_hls() {
    let manifest = one_station("https://stream.example/live/index.m3u8");
    let plain = one_station("https://stream.example/nova.mp3");
    assert_eq!(
        is_playable_station(&plain),
        is_playable_codec(&plain.codec),
        "with no manifest, the codec rule decides alone"
    );
    assert!(
        is_playable_codec(&manifest.codec),
        "the codec alone would play"
    );
    assert!(!is_playable_station(&manifest), "the manifest rule refuses");
}

// --- A refusal says why (S1, issue #19) --------------------------------------

#[test]
fn an_hls_station_refuses_with_the_manifest_reason() {
    let station = one_station("https://stream.example/live/index.m3u8");
    assert_eq!(
        refusal(&station).as_deref(),
        Some("an HLS manifest: its .ts segments cannot be decoded"),
        "the marker never says only `✗`: the URL rule names itself"
    );
    assert_eq!(
        refusal(&one_station("https://stream.example/nova.mp3")),
        None
    );
}

#[test]
fn a_codec_without_a_decoder_refuses_naming_the_codec() {
    // Every codec in the spelling table has a decoder in this build, so no
    // real `Station` can carry the codec refusal here. The decision is split:
    // the helper takes the decodability as an argument, and the public entry
    // point computes exactly that argument — the branch is testable without
    // faking a build, and the public rule is not weakened.
    assert_eq!(
        refusal_for("opus", false, "https://stream.example/live").as_deref(),
        Some("the codec opus is not in this build"),
    );
    assert_eq!(
        refusal_for("opus", true, "https://stream.example/live"),
        None,
        "decodable means no refusal: the helper does not hold an opinion"
    );
}

#[test]
fn the_refusal_agrees_with_playability_in_both_branches() {
    // Plays ⇔ no refusal, for the URL rule through real stations…
    let manifest = one_station("https://stream.example/live/index.m3u8");
    let plain = one_station("https://stream.example/nova.mp3");
    assert_eq!(refusal(&manifest).is_none(), is_playable_station(&manifest));
    assert_eq!(refusal(&plain).is_none(), is_playable_station(&plain));
    // …and for the codec rule through the split helper, with the
    // decodability the public entry point computes.
    for decodable in [true, false] {
        assert_eq!(
            refusal_for("opus", decodable, "https://stream.example/live").is_none(),
            decodable,
        );
    }
}
