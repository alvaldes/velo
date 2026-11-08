//! The radio view's state machine and one render smoke test (T6a, S9).
//!
//! The state tests need no terminal and no network: the view is built from
//! the same structs the client parses and the store persists, and every key
//! rule is asserted through [`RadioView::key`]. The render test follows the
//! `tests/ui_render.rs` pattern — a ratatui `TestBackend` at a fixed size,
//! asserted on the buffer text. No playback is wired here: `enter` on a
//! station only holds the selection.

use velo::radio::client::{Country, Order, Station, Tag};
use velo::radio::store::{self, Favorite, Pinned, RadioStore};
use velo::ui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use velo::ui::radio::{Place, RadioOutcome, RadioView, RowKind};
use velo::ui::theme::Theme;

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
            "Opus One",
            "https://stream.example/opus.opus",
            "OPUS",
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
            "Browse all countries",
            "Norway (near you)",
            "★ Germany",
            "Browse genres & tags",
            "Night FM",
            "★ Jazz One [MP3] · NO",
            "Nova [128k] · NO",
            "✗ Opus One [96k] · NO",
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
fn enter_on_a_country_opens_its_stations() {
    let mut view = view();
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
        ["Nova [128k] · NO", "✗ Opus One [96k] · NO"],
        "the place lists its stations, got: {:?}",
        view.labels()
    );
}

#[test]
fn back_returns_one_level_then_closes() {
    let mut view = view();
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
fn enter_on_a_station_holds_the_selection_and_does_nothing_else() {
    let mut view = view();
    press(&mut view, KeyCode::Char('G')); // last row: Opus One
    let selection = view.selection().expect("a row is selected");
    let RowKind::CatalogStation { station, playable } = &selection.kind else {
        panic!("the last row is a catalog station, got: {selection:?}");
    };
    assert_eq!(station.name, "Opus One");
    assert!(!playable, "the codec filter rejected this station");

    let before = view.labels();
    press(&mut view, KeyCode::Enter);
    assert_eq!(view.labels(), before, "enter on a station changes nothing");
    assert_eq!(view.selection(), Some(selection), "the selection is held");
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
        "Browse all countries",
        "Norway (near you)",
        "★ Germany",
        "Browse genres & tags",
        "Night FM",
        "★ Jazz One",
        "Nova [128k] · NO",
        "✗ Opus One",
    ] {
        assert!(
            screen.contains(expected),
            "the radio list must show {expected:?}, got:\n{screen}"
        );
    }

    // The selected row is distinguishable: its cells wear the accent.
    let row = screen
        .lines()
        .position(|line| line.contains("1. Browse all countries"))
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
