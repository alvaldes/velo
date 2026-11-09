//! The radio view's state machine and one render smoke test (T6a, S9).
//!
//! The state tests need no terminal and no network: the view is built from
//! the same structs the client parses and the store persists, and every key
//! rule is asserted through [`RadioView::key`]. Playing is wired (T6c):
//! `enter` on a station answers [`RadioOutcome::Play`] and `a` answers
//! [`RadioOutcome::Queue`], and the shell tests drive those through the
//! [`App`] the event loop drives, the play action returned but never
//! applied — applying it would open an audio device. The render tests
//! follow the `tests/ui_render.rs` pattern — a ratatui `TestBackend` at a
//! fixed size, asserted on the buffer text.

use std::path::PathBuf;
use std::time::Duration;

use velo::audio::output::Entry;
use velo::radio::client::{Country, Order, Station, Tag};
use velo::radio::store::{self, Favorite, Pinned, RadioStore};
use velo::ui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use velo::ui::radio::{Place, RadioOutcome, RadioView, RowKind};
use velo::ui::theme::Theme;
use velo::ui::{Action, App};

/// One directory station with the fields the view shows.
fn station(name: &str, url: &str, codec: &str, bitrate: u32, code: &str) -> Station {
    Station {
        name: name.to_owned(),
        url_resolved: url.to_owned(),
        country: "Norway".to_owned(),
        countrycode: code.to_owned(),
        state: String::new(),
        tags: String::new(),
        codec: codec.to_owned(),
        bitrate,
        votes: 0,
        homepage: String::new(),
    }
}

/// A store with one pinned place, one favourite and one hand-added station.
fn store() -> RadioStore {
    let mut store = RadioStore::default();
    store.pinned.push(Pinned {
        code: "DE".to_owned(),
        name: "Germany".to_owned(),
        state: String::new(),
    });
    store.favorite.push(Favorite {
        name: "Jazz One".to_owned(),
        url: "https://jazz.example/stream".to_owned(),
        countrycode: "NO".to_owned(),
        codec: "MP3".to_owned(),
    });
    store.station.push(store::Station {
        name: "Night FM".to_owned(),
        url: "http://night.example/live".to_owned(),
    });
    store
}

/// The full top level: every section has content.
fn view() -> RadioView {
    let mut view = RadioView::new(
        store(),
        vec![
            Country {
                name: "Norway".to_owned(),
                iso_3166_1: "NO".to_owned(),
                stationcount: 12,
            },
            Country {
                name: "Germany".to_owned(),
                iso_3166_1: "DE".to_owned(),
                stationcount: 40,
            },
        ],
        vec![
            Tag {
                name: "jazz".to_owned(),
                stationcount: 30,
            },
            Tag {
                name: "lofi".to_owned(),
                stationcount: 5,
            },
        ],
        Some("NO".to_owned()),
    );
    view.set_catalog(catalog());
    view
}

fn catalog() -> Vec<Station> {
    vec![
        station("Nova", "https://stream.example/nova.mp3", "MP3", 128, "NO"),
        station(
            "Hls One",
            "https://stream.example/live/index.m3u8",
            "MP3",
            96,
            "NO",
        ),
    ]
}

fn press(view: &mut RadioView, code: KeyCode) -> RadioOutcome {
    view.key(KeyEvent::from(code))
}

// ---- the section list ------------------------------------------------------

#[test]
fn the_sections_appear_in_order_only_when_they_have_content() {
    let view = view();
    assert_eq!(
        view.labels(),
        [
            "★ Jazz One [MP3] · NO",
            "Browse all countries",
            "Norway (near you)",
            "★ Germany",
            "Stations without a country",
            "Browse genres & tags",
            "Nova [128k] · NO",
            "✗ Hls One [96k] · NO",
        ],
        "the sections keep their order, got: {:?}",
        view.labels()
    );

    // Nothing has content: no section, no row.
    let empty = RadioView::empty();
    assert!(
        empty.labels().is_empty(),
        "an empty view shows no sections, got: {:?}",
        empty.labels()
    );
}

#[test]
fn the_cursor_moves_and_clamps_at_both_ends() {
    let mut view = view();
    let last = view.labels().len() - 1;
    for _ in 0..last {
        press(&mut view, KeyCode::Char('j'));
    }
    assert_eq!(view.cursor(), last, "j walks to the last row");
    press(&mut view, KeyCode::Char('j'));
    assert_eq!(view.cursor(), last, "j clamps at the end");

    for _ in 0..last {
        press(&mut view, KeyCode::Char('k'));
    }
    assert_eq!(view.cursor(), 0, "k walks back to the first row");
    press(&mut view, KeyCode::Char('k'));
    assert_eq!(view.cursor(), 0, "k clamps at the start");

    press(&mut view, KeyCode::Char('G'));
    assert_eq!(view.cursor(), last, "G jumps to the last row");
    press(&mut view, KeyCode::Char('g'));
    assert_eq!(view.cursor(), 0, "g jumps to the first row");
}

#[test]
fn favourites_come_first_at_the_top() {
    let view = view();
    let labels = view.labels();
    assert_eq!(
        labels.first().map(String::as_str),
        Some("★ Jazz One [MP3] · NO"),
        "the favourite is the first row, got: {labels:?}"
    );
    let browse = labels
        .iter()
        .position(|label| label == "Browse all countries")
        .expect("the countries section is listed");
    // A favourite is a station row marked with `★`; pinned countries carry the
    // mark too but no codec, so the `[` is what tells the two apart.
    let favourites = labels
        .iter()
        .filter(|label| label.starts_with("★ ") && label.contains('['))
        .count();
    assert!(
        favourites > 0,
        "the fixture has a favourite, got: {labels:?}"
    );
    assert_eq!(
        favourites, browse,
        "every favourite stands before the browse rows, got: {labels:?}"
    );
}

#[test]
fn enter_on_a_country_opens_its_stations() {
    let mut view = view();
    press(&mut view, KeyCode::Char('j')); // past the favourite
    press(&mut view, KeyCode::Enter); // Browse all countries
    assert_eq!(
        view.place(),
        &Place::Countries,
        "the first section opens the country index"
    );
    assert_eq!(view.labels(), ["Norway", "Germany"], "the index is listed");
    assert!(
        view.labels().iter().all(|label| view
            .visible_rows()
            .iter()
            .any(|row| &row.label == label && row.fact.contains("stations"))),
        "every country says how many stations it has"
    );

    press(&mut view, KeyCode::Char('j')); // onto Germany
    press(&mut view, KeyCode::Enter);
    assert_eq!(
        view.place(),
        &Place::Country {
            code: "DE".to_owned(),
            name: "Germany".to_owned(),
            state: String::new(),
        },
        "enter opens the country's place"
    );

    // The caller loads the place's stations into the one catalog slot.
    view.set_catalog(catalog());
    assert_eq!(
        view.labels(),
        ["Nova [128k] · NO", "✗ Hls One [96k] · NO"],
        "the place lists its stations, got: {:?}",
        view.labels()
    );
}

#[test]
fn back_returns_one_level_then_closes() {
    let mut view = view();
    press(&mut view, KeyCode::Char('j')); // past the favourite
    press(&mut view, KeyCode::Enter); // into the country index
    press(&mut view, KeyCode::Enter); // into Norway
    assert!(matches!(view.place(), Place::Country { .. }));

    assert!(view.back(), "a place goes back one level");
    assert_eq!(view.place(), &Place::Countries);
    assert!(view.back(), "the index goes back to the sections");
    assert_eq!(view.place(), &Place::Top);

    assert_eq!(
        press(&mut view, KeyCode::Esc),
        RadioOutcome::Close,
        "esc at the top closes the radio"
    );
}

// ---- the filter ------------------------------------------------------------

#[test]
fn slash_filters_the_focused_list_and_clearing_restores_it() {
    let mut view = view();
    press(&mut view, KeyCode::Char('/'));
    for character in "many".chars() {
        press(&mut view, KeyCode::Char(character));
    }
    assert!(view.filtering(), "the filter is being typed");
    assert_eq!(
        view.labels(),
        ["★ Germany"],
        "the filter narrows the focused list, got: {:?}",
        view.labels()
    );

    press(&mut view, KeyCode::Enter); // keeps the filter
    assert!(!view.typing(), "enter ends the typing");
    assert!(view.filtering(), "the filter stays applied");

    press(&mut view, KeyCode::Esc); // clears the filter
    assert!(!view.filtering(), "esc clears the filter");
    assert_eq!(view.labels().len(), 8, "clearing restores every row");
}

#[test]
fn the_filter_belongs_to_the_focused_list_alone() {
    let mut view = view();
    press(&mut view, KeyCode::Char('j')); // past the favourite
    press(&mut view, KeyCode::Enter); // country index
    press(&mut view, KeyCode::Char('/'));
    for character in "or".chars() {
        press(&mut view, KeyCode::Char(character));
    }
    assert_eq!(
        view.labels(),
        ["Norway"],
        "the index is filtered, got: {:?}",
        view.labels()
    );

    view.back();
    assert!(
        !view.filtering(),
        "going back one level leaves the filter behind"
    );
    assert_eq!(view.labels().len(), 8, "the top level is unfiltered");
}

// ---- marking stations and places -------------------------------------------

#[test]
fn f_on_a_station_toggles_the_favourite_through_the_station_key() {
    // The store keeps the favourite under a differently written URL; the
    // view must still recognise the same station (S6, `station_key`).
    let mut store = RadioStore::default();
    store.favorite.push(Favorite {
        name: "Nova".to_owned(),
        url: "https://Stream.example:443/stream/".to_owned(),
        countrycode: "NO".to_owned(),
        codec: "MP3".to_owned(),
    });
    let mut view = RadioView::new(store, Vec::new(), Vec::new(), None);
    view.set_catalog(vec![station(
        "Nova",
        "https://stream.example/stream",
        "MP3",
        128,
        "NO",
    )]);

    assert!(
        view.labels()[0].starts_with('★'),
        "the favourite is marked, got: {:?}",
        view.labels()
    );

    press(&mut view, KeyCode::Char('f'));
    assert!(
        !view.store().is_favorite("https://stream.example/stream"),
        "f takes the station off"
    );
    assert!(
        !view.labels()[0].starts_with('★'),
        "the row loses its star, got: {:?}",
        view.labels()
    );

    press(&mut view, KeyCode::Char('f'));
    assert!(
        view.store().is_favorite("https://stream.example/stream"),
        "f puts the station back"
    );
}

#[test]
fn f_on_a_place_pins_it_and_f_on_a_pinned_place_unpins_it() {
    let mut view = view();
    press(&mut view, KeyCode::Char('j')); // past the favourite
    press(&mut view, KeyCode::Enter); // country index
    press(&mut view, KeyCode::Char('f')); // Norway
    assert!(
        view.store().is_pinned("NO", ""),
        "f pins the selected place"
    );

    view.back();
    assert!(
        view.labels().contains(&"★ Norway".to_owned()),
        "the new pin joins the section list, got: {:?}",
        view.labels()
    );

    // The pinned row sits after Germany's; f on it takes the pin off again.
    press(&mut view, KeyCode::Char('j'));
    press(&mut view, KeyCode::Char('j'));
    press(&mut view, KeyCode::Char('j'));
    press(&mut view, KeyCode::Char('j'));
    assert_eq!(view.labels()[view.cursor()], "★ Norway");
    press(&mut view, KeyCode::Char('f'));
    assert!(
        !view.store().is_pinned("NO", ""),
        "f on a pinned place unpins it"
    );
    assert!(
        !view.labels().contains(&"★ Norway".to_owned()),
        "the row goes away with the pin"
    );
}

// ---- the order overlay ------------------------------------------------------

#[test]
fn the_order_overlay_cycles_and_applies() {
    let mut view = view();
    press(&mut view, KeyCode::Char('o'));
    assert!(view.order_open(), "o opens the order overlay");
    assert_eq!(view.order_cursor(), 0, "Most Voted starts focused");

    press(&mut view, KeyCode::Char('j'));
    assert_eq!(view.order_cursor(), 1, "j moves down the five rows");
    press(&mut view, KeyCode::Enter);
    assert!(!view.order_open(), "enter applies and closes");
    assert_eq!(view.order(), Order::ClickCount, "Most Listened applied");

    press(&mut view, KeyCode::Char('o'));
    for _ in 0..6 {
        press(&mut view, KeyCode::Char('j'));
    }
    assert_eq!(view.order_cursor(), 4, "the cursor clamps at Random");
    press(&mut view, KeyCode::Enter);
    assert_eq!(view.order(), Order::Random);

    press(&mut view, KeyCode::Char('o'));
    press(&mut view, KeyCode::Char('k')); // already at the top? no: Random first
    press(&mut view, KeyCode::Char('g'));
    assert_eq!(view.order_cursor(), 0, "g jumps to the first row");
    press(&mut view, KeyCode::Char('k'));
    assert_eq!(view.order_cursor(), 0, "k clamps at the first row");
    press(&mut view, KeyCode::Esc);
    assert!(!view.order_open(), "esc cancels");
    assert_eq!(view.order(), Order::Random, "the order is unchanged");
}

// ---- the other keys ---------------------------------------------------------

#[test]
fn ctrl_r_asks_for_a_refresh_exactly_once() {
    let mut view = view();
    let _ = view.key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    assert!(
        view.take_refresh_request(),
        "ctrl-r asks the caller for a refresh"
    );
    assert!(!view.take_refresh_request(), "the ask is consumed once");
}

#[test]
fn n_opens_the_country_index_from_anywhere() {
    let mut view = view();
    press(&mut view, KeyCode::Char('N'));
    assert_eq!(view.place(), &Place::Countries, "N opens the index");

    press(&mut view, KeyCode::Enter); // into Norway
    press(&mut view, KeyCode::Char('N'));
    assert_eq!(
        view.place(),
        &Place::Countries,
        "N opens the index from a place too"
    );
    // Back walks the places N went through, one level at a time.
    view.back();
    assert!(
        matches!(view.place(), Place::Country { code, .. } if code == "NO"),
        "back from the index returns to the place"
    );
    view.back();
    assert_eq!(view.place(), &Place::Countries);
    view.back();
    assert_eq!(
        view.place(),
        &Place::Top,
        "back at last returns to the start"
    );
}

#[test]
fn enter_and_a_on_an_unplayable_station_never_queue() {
    let mut view = view();
    press(&mut view, KeyCode::Char('G')); // last row: Hls One
    let selection = view.selection().expect("a row is selected");
    let RowKind::CatalogStation { station, playable } = &selection.kind else {
        panic!("the last row is a catalog station, got: {selection:?}");
    };
    assert_eq!(station.name, "Hls One");
    assert!(!playable, "the directory rejected this station");

    let before = view.labels();
    assert_eq!(
        press(&mut view, KeyCode::Enter),
        RadioOutcome::Handled,
        "enter on an unplayable station queues nothing"
    );
    assert_eq!(
        press(&mut view, KeyCode::Char('a')),
        RadioOutcome::Handled,
        "a on an unplayable station queues nothing"
    );
    assert_eq!(view.labels(), before, "the view is unchanged");
    assert_eq!(view.selection(), Some(selection), "the selection is held");
}

// ---- playing a station (T6c, S9, S3) ----------------------------------------

#[test]
fn enter_on_a_playable_station_asks_to_queue_and_play_it() {
    let mut view = view();
    press(&mut view, KeyCode::Char('G')); // last row: Hls One
    press(&mut view, KeyCode::Char('k')); // up onto Nova
    let outcome = press(&mut view, KeyCode::Enter);
    assert_eq!(
        outcome,
        RadioOutcome::Play {
            url: String::from("https://stream.example/nova.mp3"),
            name: String::from("Nova"),
        },
        "enter hands the station to the shell to queue and play"
    );
    assert_eq!(view.place(), &Place::Top, "playing does not navigate");
    assert_eq!(view.cursor(), 6, "the selection is held");
}

#[test]
fn a_appends_a_station_without_starting_it() {
    let mut view = view();
    press(&mut view, KeyCode::Char('G'));
    press(&mut view, KeyCode::Char('k'));
    let outcome = press(&mut view, KeyCode::Char('a'));
    assert_eq!(
        outcome,
        RadioOutcome::Queue {
            url: String::from("https://stream.example/nova.mp3"),
            name: String::from("Nova"),
        },
        "a hands the station to the shell to queue, not to play"
    );
    assert_eq!(view.place(), &Place::Top, "a does not leave the view");
}

#[test]
fn a_hand_added_or_favourite_station_queues_too() {
    let mut view = view();
    // The favourite sits at the top now (S5).
    assert_eq!(
        press(&mut view, KeyCode::Enter),
        RadioOutcome::Play {
            url: String::from("https://jazz.example/stream"),
            name: String::from("Jazz One"),
        },
        "a favourite plays like any station"
    );
    // The hand-added station lives in the no-country section (S4).
    let section = view
        .labels()
        .iter()
        .position(|label| label == "Stations without a country")
        .expect("the no-country section is listed");
    for _ in 0..section {
        press(&mut view, KeyCode::Char('j'));
    }
    press(&mut view, KeyCode::Enter);
    assert_eq!(
        press(&mut view, KeyCode::Char('a')),
        RadioOutcome::Queue {
            url: String::from("http://night.example/live"),
            name: String::from("Night FM"),
        },
        "a hand-added station queues like any station"
    );
}

#[test]
fn enter_on_a_tag_still_opens_its_stations() {
    let mut view = view();
    for _ in 0..5 {
        press(&mut view, KeyCode::Char('j')); // Browse genres & tags
    }
    press(&mut view, KeyCode::Enter); // into the tag index
    assert_eq!(view.place(), &Place::Tags);
    press(&mut view, KeyCode::Enter); // jazz
    assert_eq!(
        view.place(),
        &Place::Tag {
            name: String::from("jazz")
        },
        "enter on a tag still opens its stations"
    );
}

// ---- the shell wiring (T6c): from the view into the queue --------------------

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::from(code)
}

/// One key press of the shell, its action applied — except a play, which
/// would open an audio device: that one is returned, and the returned
/// action is the proof the player was asked to start.
fn shell(app: &mut App<'_>, code: KeyCode) -> Action {
    let action = app.on_key(key(code));
    if !matches!(action, Action::PlayQueue { .. }) {
        app.apply(action);
    }
    action
}

/// The whole interface as one screen, the way the user sees it.
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

/// The station the shell tests play. The transport tests use their own
/// entry, whose name must differ from the ICY title it pretends to carry.
fn nova() -> Entry {
    Entry::Station {
        url: String::from("https://stream.example/nova.mp3"),
        name: String::from("Nova"),
    }
}

#[test]
fn enter_on_a_station_queues_it_and_asks_the_player_to_start() {
    let mut app = App::new(Vec::new());
    shell(&mut app, KeyCode::Char('R')); // the radio opens over its wiring
    app.show_radio_catalog(vec![station(
        "Nova",
        "https://stream.example/nova.mp3",
        "MP3",
        128,
        "NO",
    )]);
    shell(&mut app, KeyCode::Char('G')); // the last row: Nova

    let action = shell(&mut app, KeyCode::Enter);
    assert_eq!(
        action,
        Action::PlayQueue { from: 0 },
        "the player was asked to start at the station"
    );
    assert_eq!(
        app.queue(),
        [nova()],
        "the station went into the queue as an Entry::Station"
    );
}

#[test]
fn a_appends_to_the_queue_without_starting() {
    let mut app = App::new(Vec::new());
    shell(&mut app, KeyCode::Char('R'));
    app.show_radio_catalog(vec![station(
        "Nova",
        "https://stream.example/nova.mp3",
        "MP3",
        128,
        "NO",
    )]);
    shell(&mut app, KeyCode::Char('G'));

    let action = shell(&mut app, KeyCode::Char('a'));
    assert_eq!(action, Action::None, "appending asks nothing of the player");
    assert_eq!(app.queue(), [nova()], "the station joined the queue");
    assert_eq!(
        app.status(),
        "velo: added 1 track",
        "the status line says what happened"
    );
}

#[test]
fn seeking_a_station_says_so_instead_of_moving() {
    let mut app = App::new(Vec::new());
    app.show_playing_for_tests(nova(), Duration::ZERO, None, None);

    let action = app.on_key(key(KeyCode::Right));
    assert_eq!(action, Action::Seek { delta_secs: 5 });
    app.apply(action);
    assert_eq!(
        app.status(),
        "velo: nothing to seek in a live station",
        "the seek is refused, the way the audio layer refuses it"
    );
    assert_eq!(app.queue(), [nova()], "the seek moved nothing");
}

// ---- the transport reads a station as live (T6c, S3) ------------------------

#[test]
fn the_transport_shows_live_for_a_station_and_names_the_icy_title() {
    let mut app = App::new(Vec::new());
    app.show_playing_for_tests(
        Entry::Station {
            url: String::from("https://stream.example/nova.mp3"),
            name: String::from("Radio Nova"),
        },
        Duration::ZERO,
        None,
        Some(String::from("Fleetwood Mac - Dreams")),
    );
    let screen = app_screen(&app);
    assert!(
        screen.contains("● live"),
        "the clock says live where a position and total would be, got:\n{screen}"
    );
    assert!(
        screen.contains("Fleetwood Mac - Dreams"),
        "the ICY title is the now-playing title, got:\n{screen}"
    );
    assert!(
        !screen.contains("Nova"),
        "the station name yields to the ICY title, got:\n{screen}"
    );
}

#[test]
fn without_an_icy_title_the_station_name_is_the_title() {
    let mut app = App::new(Vec::new());
    app.show_playing_for_tests(
        Entry::Station {
            url: String::from("https://stream.example/nova.mp3"),
            name: String::from("Radio Nova"),
        },
        Duration::ZERO,
        None,
        None,
    );
    let screen = app_screen(&app);
    assert!(screen.contains("● live"), "still live, got:\n{screen}");
    assert!(
        screen.contains("Radio Nova"),
        "the station name is the title, got:\n{screen}"
    );
}

#[test]
fn the_transport_shows_a_position_and_total_for_a_file() {
    let mut app = App::new(Vec::new());
    app.show_playing_for_tests(
        Entry::File(PathBuf::from("/music/Tide/01 - Dawn.flac")),
        Duration::from_secs(83),
        Some(Duration::from_secs(296)),
        None,
    );
    let screen = app_screen(&app);
    assert!(
        screen.contains("01:23 / 04:56"),
        "a file shows its position and total, got:\n{screen}"
    );
    assert!(!screen.contains("● live"), "a file is not live");
}

// ---- the stations without a country (S4) ------------------------------------

#[test]
fn the_no_country_section_lists_own_stations_then_no_country_favourites() {
    let mut store = RadioStore::default();
    store.favorite.push(Favorite {
        name: "Jazz One".to_owned(),
        url: "https://jazz.example/stream".to_owned(),
        countrycode: "NO".to_owned(),
        codec: "MP3".to_owned(),
    });
    store.favorite.push(Favorite {
        name: "Free Radio".to_owned(),
        url: "https://free.example/stream".to_owned(),
        countrycode: String::new(),
        codec: String::new(),
    });
    store.station.push(store::Station {
        name: "Night FM".to_owned(),
        url: "http://night.example/live".to_owned(),
    });
    let mut view = RadioView::new(store, Vec::new(), Vec::new(), None);

    // The section sits after the pinned places and before the tags, and the
    // no-country favourite is listed at the top too (S5): once is the
    // shortcut, once is the section.
    let section = view
        .labels()
        .iter()
        .position(|label| label == "Stations without a country")
        .expect("the no-country section is listed");
    assert_eq!(
        view.labels(),
        [
            "★ Jazz One [MP3] · NO",
            "★ Free Radio",
            "Stations without a country",
        ],
        "the section follows the favourites, got: {:?}",
        view.labels()
    );

    for _ in 0..section {
        press(&mut view, KeyCode::Char('j'));
    }
    press(&mut view, KeyCode::Enter);
    assert_eq!(
        view.place(),
        &Place::NoCountry,
        "entering the section opens its place, got: {:?}",
        view.place()
    );
    assert_eq!(
        view.labels(),
        ["Night FM", "★ Free Radio"],
        "own stations first, then the no-country favourites; a favourite with a country keeps out, got: {:?}",
        view.labels()
    );
    assert!(view.back(), "the section goes back one level");
    assert_eq!(view.place(), &Place::Top);
}

#[test]
fn the_no_country_section_exists_only_with_content() {
    let mut store = RadioStore::default();
    store.favorite.push(Favorite {
        name: "Jazz One".to_owned(),
        url: "https://jazz.example/stream".to_owned(),
        countrycode: "NO".to_owned(),
        codec: "MP3".to_owned(),
    });
    let view = RadioView::new(store, Vec::new(), Vec::new(), None);
    assert!(
        !view
            .labels()
            .contains(&"Stations without a country".to_owned()),
        "nothing belongs to no country, no section, got: {:?}",
        view.labels()
    );
}

// ---- the render -------------------------------------------------------------

fn screen(view: &RadioView) -> String {
    use velo::ui::ratatui::Terminal;
    use velo::ui::ratatui::backend::TestBackend;
    use velo::ui::ratatui::buffer::{Buffer, Cell};

    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("a test terminal");
    terminal
        .draw(|frame| view.render(frame, frame.area(), &Theme::default()))
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

#[test]
fn the_radio_list_draws_its_sections_and_lights_the_selected_row() {
    let view = view();
    let screen = screen(&view);

    for expected in [
        "Radio",
        "★ Jazz One",
        "Browse all countries",
        "Norway (near you)",
        "★ Germany",
        "Stations without a country",
        "Browse genres & tags",
        "Nova [128k] · NO",
        "✗ Hls One",
    ] {
        assert!(
            screen.contains(expected),
            "the radio list must show {expected:?}, got:\n{screen}"
        );
    }

    // The selected row is distinguishable: its cells wear the accent.
    let row = screen
        .lines()
        .position(|line| line.contains("1. ★ Jazz One"))
        .expect("the first row is on screen");
    let mut terminal =
        velo::ui::ratatui::Terminal::new(velo::ui::ratatui::backend::TestBackend::new(80, 24))
            .expect("a test terminal");
    terminal
        .draw(|frame| view.render(frame, frame.area(), &Theme::default()))
        .expect("the frame draws");
    let cell = terminal.backend().buffer()[(10, u16::try_from(row).unwrap())].clone();
    assert_eq!(
        cell.bg,
        Theme::default().accent,
        "the selected row is the accent, got: {cell:?}"
    );
}
