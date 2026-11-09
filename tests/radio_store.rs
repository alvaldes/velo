//! Station preference store tests (spec S5, with the S6 identity): a missing
//! `radio.toml` loads as an empty store, a corrupt one is an error that never
//! leads to a truncating write, saves are atomic and leave no temporary file
//! behind, and every deduplication goes through the normalised station key.
//!
//! All state lives in a unique temporary directory per test (process id plus
//! test name); the real `~/.config` is never touched. No network, no files
//! outside the repo, no new dependencies.

use std::fs;
use std::path::PathBuf;

use velo::radio::store::{
    Favorite, Pinned, RadioStore, Station, StoreError, TEMPLATE, Toggled, station_key,
    write_template_if_missing,
};

// ------------------------------------------------------------- infrastructure

/// A unique temporary directory, removed again when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("velo-radio-store-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    /// The `radio.toml` inside the scratch directory.
    fn file(&self) -> PathBuf {
        self.0.join("radio.toml")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Radio Paradise under whatever URL shape the caller was handed.
fn paradise(url: &str) -> Favorite {
    Favorite {
        name: "Radio Paradise".to_owned(),
        url: url.to_owned(),
        countrycode: "US".to_owned(),
        codec: "AAC".to_owned(),
    }
}

// ------------------------------------------------------------------- loading

#[test]
fn a_missing_file_loads_as_an_empty_store() {
    let scratch = Scratch::new("missing");
    let store = RadioStore::load(&scratch.file()).unwrap();
    assert_eq!(store, RadioStore::default());
    assert_eq!(store.country, None);
    assert_eq!(store.favorite, []);
    assert_eq!(store.pinned, []);
    assert_eq!(store.station, []);
}

#[test]
fn a_round_trip_preserves_favourites_pins_stations_and_country() {
    let scratch = Scratch::new("round-trip");
    let mut store = RadioStore {
        country: Some("NO".to_owned()),
        ..RadioStore::default()
    };
    store.toggle_favorite(paradise("https://stream.radioparadise.com/aac-320"));
    store.toggle_pin(Pinned {
        code: "DE".to_owned(),
        name: "Germany".to_owned(),
        state: String::new(),
    });
    store.add_station(Station {
        name: "My own mix".to_owned(),
        url: "https://example.com/mix.mp3".to_owned(),
    });

    store.save(&scratch.file()).unwrap();
    let loaded = RadioStore::load(&scratch.file()).unwrap();
    assert_eq!(loaded, store);
}

#[test]
fn country_none_round_trips_and_stays_distinguishable_from_absent() {
    let declined = Scratch::new("country-none");
    let store = RadioStore {
        country: Some("none".to_owned()),
        ..RadioStore::default()
    };
    store.save(&declined.file()).unwrap();
    assert_eq!(
        RadioStore::load(&declined.file()).unwrap().country,
        Some("none".to_owned())
    );

    let never_asked = Scratch::new("country-absent");
    RadioStore::default().save(&never_asked.file()).unwrap();
    assert_eq!(RadioStore::load(&never_asked.file()).unwrap().country, None);
    // And the absent one really does not carry the key:
    assert!(
        !fs::read_to_string(never_asked.file())
            .unwrap()
            .contains("country")
    );
}

// ------------------------------------------------------------------ deduping

#[test]
fn favouriting_the_same_station_twice_takes_it_off_however_the_url_is_written() {
    let mut store = RadioStore::default();
    assert_eq!(
        store.toggle_favorite(paradise("https://A.example/stream")),
        Toggled::On
    );
    // The marker is on, so pressing it again takes the station off — even when
    // the URL arrives written differently. It never adds a second record.
    assert_eq!(
        store.toggle_favorite(paradise("https://a.example/stream/")),
        Toggled::Off
    );
    assert_eq!(store.favorite, []);
    // The default port is the same station too:
    assert_eq!(
        store.toggle_favorite(paradise("https://a.example:443/stream")),
        Toggled::On
    );
    assert_eq!(store.favorite.len(), 1);
    // Scheme and host case are the same station; path case is not:
    assert!(store.is_favorite("HTTPS://A.EXAMPLE/stream/"));
    assert!(!store.is_favorite("https://a.example/STREAM"));
}

#[test]
fn adding_a_station_dedups_the_same_way() {
    let mut store = RadioStore::default();
    assert_eq!(
        store.add_station(Station {
            name: "Mix".to_owned(),
            url: "http://Example.org:80/mix.mp3".to_owned(),
        }),
        Toggled::On
    );
    assert_eq!(
        store.add_station(Station {
            name: "Mix again".to_owned(),
            url: "http://example.org/mix.mp3".to_owned(),
        }),
        Toggled::Already
    );
    assert_eq!(store.station.len(), 1);
    // Removal goes through the key as well:
    assert!(store.remove_station("HTTP://EXAMPLE.ORG:80/mix.mp3/"));
    assert_eq!(store.station, []);
}

#[test]
fn removing_an_absent_station_is_a_no_op() {
    let mut store = RadioStore::default();
    store.add_station(Station {
        name: "Mix".to_owned(),
        url: "https://example.com/mix.mp3".to_owned(),
    });
    assert!(!store.remove_station("https://example.com/other.mp3"));
    assert_eq!(store.station.len(), 1);
}

#[test]
fn pinning_dedups_on_country_and_state() {
    let mut store = RadioStore::default();
    assert_eq!(
        store.toggle_pin(Pinned {
            code: "DE".to_owned(),
            name: "Germany".to_owned(),
            state: String::new(),
        }),
        Toggled::On
    );
    assert_eq!(
        store.toggle_pin(Pinned {
            code: "de".to_owned(),
            name: "Germany".to_owned(),
            state: String::new(),
        }),
        Toggled::Off
    );
    assert!(!store.is_pinned("DE", ""));
    assert_eq!(store.pinned, []);
}

// ----------------------------------------------------------------- atomicity

#[test]
fn a_corrupt_file_is_an_error_and_is_never_truncated() {
    let scratch = Scratch::new("corrupt");
    let corrupt = "country = \n[[favorite\nname =";
    fs::write(scratch.file(), corrupt).unwrap();

    let loaded = RadioStore::load(&scratch.file());
    assert!(matches!(loaded, Err(StoreError::Parse(_))));

    // The caller refuses to save over a file it could not read; the store
    // never truncates it behind the caller's back.
    if let Ok(store) = loaded {
        let _ = store.save(&scratch.file());
    }
    assert_eq!(fs::read(scratch.file()).unwrap(), corrupt.as_bytes());
}

#[test]
fn a_successful_save_leaves_no_temporary_file_behind() {
    let scratch = Scratch::new("atomic");
    RadioStore::default().save(&scratch.file()).unwrap();
    let mut entries: Vec<String> = fs::read_dir(&scratch.0)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    entries.sort();
    assert_eq!(entries, ["radio.toml"]);
}

// ------------------------------------------------------------------ leniency

#[test]
fn unknown_keys_are_tolerated_not_rejected() {
    let scratch = Scratch::new("unknown-keys");
    fs::write(
        scratch.file(),
        "\
country = \"NO\"
wiggle = true

[[favorite]]
name = \"Radio Paradise\"
url = \"https://stream.radioparadise.com/aac-320\"
countrycode = \"US\"
codec = \"AAC\"
mood = \"cosy\"

[[station]]
name = \"Mine\"
url = \"https://example.com/mix.mp3\"
era = \"2026\"
",
    )
    .unwrap();
    let store = RadioStore::load(&scratch.file()).unwrap();
    assert_eq!(store.country, Some("NO".to_owned()));
    assert_eq!(store.favorite.len(), 1);
    assert_eq!(
        store.favorite[0].url,
        "https://stream.radioparadise.com/aac-320"
    );
    assert_eq!(store.station.len(), 1);
}

// ------------------------------------------------------------------ identity

#[test]
fn the_station_key_normalises_the_five_shapes() {
    let base = station_key("https://Stream.example/Stream");
    assert_eq!(base, "https://stream.example/Stream");
    assert_eq!(station_key("HTTPS://stream.example/Stream"), base);
    assert_eq!(station_key("https://STREAM.example/Stream"), base);
    assert_eq!(station_key("https://stream.example:443/Stream"), base);
    assert_eq!(station_key("https://stream.example/Stream/"), base);
    // The http default port goes too, but nothing else flattens:
    assert_eq!(
        station_key("http://a.example:80/s"),
        station_key("http://a.example/s")
    );
    assert_ne!(base, station_key("https://stream.example/other"));
    assert_ne!(base, station_key("https://stream.example:8080/Stream"));
    assert_ne!(base, station_key("http://stream.example/Stream"));
    assert_ne!(
        station_key("https://a.example/s?x=1"),
        station_key("https://a.example/s?y=1")
    );
}

// -------------------------------------------- the fresh-install template

#[test]
fn a_fresh_install_finds_a_documented_radio_file_that_holds_nothing() {
    let scratch = Scratch::new("template");
    let file = scratch.file();

    assert!(
        write_template_if_missing(&file).unwrap(),
        "the template is written when nothing is there"
    );
    let text = fs::read_to_string(&file).unwrap();
    assert!(
        text.contains("[[favorite]]") && text.contains("[[station]]"),
        "the format is shown by example: {text}"
    );
    assert_eq!(
        RadioStore::load(&file).expect("the template parses"),
        RadioStore::default(),
        "and it holds nothing, so the first launch has no favourites"
    );
    assert_eq!(TEMPLATE, text, "the file is the template, byte for byte");
}

#[test]
fn the_template_never_touches_a_file_that_is_already_there() {
    let scratch = Scratch::new("template-keep");
    let file = scratch.file();
    let hand = "[[favorite]]\nname = \"Mine\"\nurl = \"https://example.com/a\"\ncountrycode = \"NO\"\ncodec = \"MP3\"\n";
    fs::write(&file, hand).unwrap();

    assert!(
        !write_template_if_missing(&file).unwrap(),
        "nothing is written over an existing file"
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), hand, "byte for byte");
}
