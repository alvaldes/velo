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

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use velo::audio::output::Entry;
use velo::radio::client::{Country, HttpTransport, Order, Station, Tag};
use velo::radio::store::{self, Favorite, Pinned, RadioStore};
use velo::ui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use velo::ui::radio::{Place, RadioOutcome, RadioView, RowKind};
use velo::ui::radio_data::RadioData;
use velo::ui::theme::Theme;
use velo::ui::{Action, App, View};

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
    view.set_catalog(norway(), catalog());
    view
}

/// The place the fixture's catalog belongs to: Norway itself.
fn norway() -> Place {
    Place::Country {
        code: String::from("NO"),
        name: String::from("Norway"),
        state: String::new(),
    }
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
            "Browse all countries",
            "Norway (near you)",
            "★ Germany",
            "Stations without a country",
            "Browse genres & tags",
            "", // the dotted rule that closes the sections above
            "favourites",
            "★ Jazz One [MP3] · NO",
        ],
        "the sections come first and the favourites close the list (S5, #22), got: {:?}",
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

/// `★ Name [codec]`: what a favourite station row's label starts and ends
/// with. Pinned countries carry the mark too but no codec, so the `[` is
/// what tells the two apart.
fn is_favorite_station_label(label: &str) -> bool {
    label.starts_with("★ ") && label.contains('[')
}

#[test]
fn favourites_sit_below_the_sections_behind_a_rule_and_a_header() {
    let view = view();
    let labels = view.labels();
    let rule = labels
        .iter()
        .position(String::is_empty)
        .expect("the dotted rule is listed");
    let header = labels
        .iter()
        .position(|label| label == "favourites")
        .expect("the favourites header is listed");
    let first_favorite = labels
        .iter()
        .position(|label| is_favorite_station_label(label))
        .expect("the fixture has a favourite");
    assert_eq!(
        rule + 1,
        header,
        "the rule closes the sections right above the header, got: {labels:?}"
    );
    assert_eq!(
        header + 1,
        first_favorite,
        "the favourites start right under their header, got: {labels:?}"
    );
    assert!(
        labels[..rule]
            .iter()
            .all(|label| !is_favorite_station_label(label)),
        "no favourite stands above the rule, got: {labels:?}"
    );
    assert!(
        labels[first_favorite..]
            .iter()
            .all(|label| is_favorite_station_label(label)),
        "only favourites stand under the header, got: {labels:?}"
    );
}

#[test]
fn with_no_favourites_there_is_no_rule_and_no_header() {
    let mut store = RadioStore::default();
    store.pinned.push(Pinned {
        code: "DE".to_owned(),
        name: "Germany".to_owned(),
        state: String::new(),
    });
    let mut view = RadioView::new(store, Vec::new(), Vec::new(), None);
    view.show_browse_rows();
    assert_eq!(
        view.labels(),
        ["Browse all countries", "★ Germany", "Browse genres & tags"],
        "an empty section is not drawn — the favourites' neither, got: {:?}",
        view.labels()
    );
}

#[test]
fn j_k_g_and_g_never_rest_on_the_rule_or_the_header() {
    let mut view = view();
    // Down from the last normal row lands on the first favourite, never on
    // the rule between them.
    for _ in 0..4 {
        press(&mut view, KeyCode::Char('j')); // onto Browse genres & tags
    }
    assert_eq!(view.labels()[view.cursor()], "Browse genres & tags");
    press(&mut view, KeyCode::Char('j'));
    assert_eq!(
        view.labels()[view.cursor()],
        "★ Jazz One [MP3] · NO",
        "j skips the rule and the header"
    );

    // Walking the list in both directions rests only on rows a person can
    // act on.
    press(&mut view, KeyCode::Char('G'));
    assert_eq!(view.labels()[view.cursor()], "★ Jazz One [MP3] · NO");
    for _ in 0..8 {
        press(&mut view, KeyCode::Char('k'));
        assert!(
            view.selection().is_some(),
            "k never rests on the rule or the header, at {:?}",
            view.labels()[view.cursor()]
        );
    }
    assert_eq!(view.cursor(), 0, "k walks back to the first row");
    press(&mut view, KeyCode::Char('G'));
    assert_eq!(
        view.cursor(),
        view.labels().len() - 1,
        "G jumps to the last row"
    );
    press(&mut view, KeyCode::Char('g'));
    assert_eq!(view.cursor(), 0, "g jumps to the first row");
    for _ in 0..12 {
        press(&mut view, KeyCode::Char('j'));
        assert!(
            view.selection().is_some(),
            "j never rests on the rule or the header, at {:?}",
            view.labels()[view.cursor()]
        );
    }
}

#[test]
fn enter_f_and_a_on_the_rule_or_the_header_change_nothing() {
    // A store with only favourites: the list opens on the rule — the one
    // place the cursor can start on, since there is no row above it.
    let mut store = RadioStore::default();
    store.favorite.push(Favorite {
        name: "Jazz One".to_owned(),
        url: "https://jazz.example/stream".to_owned(),
        countrycode: "NO".to_owned(),
        codec: "MP3".to_owned(),
    });
    let mut rule_view = RadioView::new(store, Vec::new(), Vec::new(), None);
    assert_eq!(
        rule_view.labels(),
        ["", "favourites", "★ Jazz One [MP3] · NO"],
        "got: {:?}",
        rule_view.labels()
    );
    assert_eq!(
        rule_view.selection(),
        None,
        "the cursor stands on the rule: no row to act on"
    );
    let before = rule_view.labels();
    assert_eq!(
        press(&mut rule_view, KeyCode::Enter),
        RadioOutcome::Handled,
        "enter on the rule opens nothing"
    );
    assert_eq!(
        press(&mut rule_view, KeyCode::Char('a')),
        RadioOutcome::Handled,
        "a on the rule queues nothing"
    );
    press(&mut rule_view, KeyCode::Char('f'));
    assert_eq!(rule_view.labels(), before, "f on the rule marks nothing");
    assert!(
        rule_view.store().is_favorite("https://jazz.example/stream"),
        "the store keeps its favourite"
    );
    press(&mut rule_view, KeyCode::Char('j'));
    assert_eq!(
        rule_view.labels()[rule_view.cursor()],
        "★ Jazz One [MP3] · NO",
        "j skips the header too"
    );

    // The header: a filter only the header matches leaves the cursor on it.
    let mut filtered = view();
    press(&mut filtered, KeyCode::Char('/'));
    for character in "fav".chars() {
        press(&mut filtered, KeyCode::Char(character));
    }
    press(&mut filtered, KeyCode::Enter); // keeps the filter
    assert_eq!(
        filtered.labels(),
        ["favourites"],
        "only the header survives this filter, got: {:?}",
        filtered.labels()
    );
    assert_eq!(
        filtered.selection(),
        None,
        "the cursor stands on the header: no row to act on"
    );
    assert_eq!(
        press(&mut filtered, KeyCode::Enter),
        RadioOutcome::Handled,
        "enter on the header opens nothing"
    );
    assert_eq!(
        press(&mut filtered, KeyCode::Char('a')),
        RadioOutcome::Handled,
        "a on the header queues nothing"
    );
    press(&mut filtered, KeyCode::Char('f'));
    assert!(
        filtered.store().is_favorite("https://jazz.example/stream"),
        "f on the header marks nothing"
    );
    assert_eq!(filtered.labels(), ["favourites"], "the list is unchanged");
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

    // The caller loads the place's stations into the one catalog slot,
    // naming the place the answer belongs to (S9).
    let germany = Place::Country {
        code: "DE".to_owned(),
        name: "Germany".to_owned(),
        state: String::new(),
    };
    view.set_catalog(germany, catalog());
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

// ---- a place lists its own stations, never another's (T6, S9, #20) ----------

#[test]
fn the_top_lists_sections_only_never_station_rows() {
    let view = view(); // the fixture holds a loaded catalog
    for label in view.labels() {
        assert!(
            !label.contains("Nova") && !label.contains("Hls One"),
            "the top is a section list: no station rows, got: {label:?}"
        );
    }
}

#[test]
fn back_from_a_place_leaves_no_station_rows_behind() {
    let mut view = view();
    press(&mut view, KeyCode::Enter); // country index
    press(&mut view, KeyCode::Enter); // Norway, the index's first row
    view.set_catalog(norway(), catalog()); // the answer lands for Norway
    assert_eq!(
        view.labels(),
        ["Nova [128k] · NO", "✗ Hls One [96k] · NO"],
        "the place lists its own stations"
    );

    assert!(view.back(), "a place goes back one level");
    assert_eq!(view.place(), &Place::Countries);
    assert!(
        view.labels().iter().all(|label| !label.contains("Nova")),
        "the index lists countries only, got: {:?}",
        view.labels()
    );
    assert!(view.back(), "the index goes back to the sections");
    assert_eq!(view.place(), &Place::Top);
    for label in view.labels() {
        assert!(
            !label.contains("Nova") && !label.contains("Hls One"),
            "the top lists no station rows, got: {label:?}"
        );
    }
}

#[test]
fn a_second_place_shows_nothing_from_the_first_before_its_own_answer() {
    let mut view = view();
    press(&mut view, KeyCode::Enter); // country index
    press(&mut view, KeyCode::Enter); // Norway
    view.set_catalog(norway(), catalog()); // Norway's answer lands
    assert!(
        view.labels().contains(&"Nova [128k] · NO".to_owned()),
        "Norway shows its stations, got: {:?}",
        view.labels()
    );

    view.back();
    view.back();
    press(&mut view, KeyCode::Enter); // country index
    press(&mut view, KeyCode::Char('j')); // Norway
    press(&mut view, KeyCode::Char('j')); // Germany
    press(&mut view, KeyCode::Enter);
    assert!(
        matches!(view.place(), Place::Country { code, .. } if code == "DE"),
        "the second place is Germany, got: {:?}",
        view.place()
    );
    assert!(
        view.labels().is_empty(),
        "no answer yet, and nothing from Norway, got: {:?}",
        view.labels()
    );
}

// ---- a wait says so (T7, S10, #21) ------------------------------------------

#[test]
fn an_empty_waiting_place_says_it_is_loading_until_the_answer_lands() {
    let mut view = view();
    press(&mut view, KeyCode::Enter); // country index
    press(&mut view, KeyCode::Enter); // Norway, no answer yet
    assert!(
        view.labels().is_empty(),
        "no answer yet, the place lists nothing"
    );
    assert_eq!(
        view.pending_place(),
        None,
        "nothing is in flight before the wiring says so"
    );
    assert!(screen(&view).contains("no stations here"));

    view.set_loading(Some(norway()));
    assert_eq!(
        view.pending_place(),
        Some(&norway()),
        "the waiting fact is readable while the request runs"
    );
    let loading = screen(&view);
    assert!(
        loading.contains("⟳ loading…"),
        "the list area says it is loading, got:\n{loading}"
    );
    assert!(
        !loading.contains("no stations here"),
        "the empty wording yields to the waiting one, got:\n{loading}"
    );

    // The answer lands: the waiting fact and its wording both go away.
    view.set_loading(None);
    assert_eq!(view.pending_place(), None);
    let answered = screen(&view);
    assert!(
        answered.contains("no stations here") && !answered.contains("⟳ loading…"),
        "the message goes away with the answer, got:\n{answered}"
    );

    // A tag place says it too (S10).
    let jazz = Place::Tag {
        name: String::from("jazz"),
    };
    view.open_place(jazz.clone());
    view.set_loading(Some(jazz));
    assert!(
        screen(&view).contains("⟳ loading…"),
        "a tag place waiting says so, got:\n{}",
        screen(&view)
    );
}

#[test]
fn a_place_with_rows_keeps_them_while_its_request_is_in_flight() {
    let mut view = view();
    press(&mut view, KeyCode::Enter); // country index
    press(&mut view, KeyCode::Enter); // Norway
    view.set_catalog(norway(), catalog()); // the answer is in
    view.set_loading(Some(norway())); // ctrl-r asked for it again

    assert_eq!(
        view.labels(),
        ["Nova [128k] · NO", "✗ Hls One [96k] · NO"],
        "the rows stay while the request runs, got: {:?}",
        view.labels()
    );
    assert!(
        !screen(&view).contains("⟳ loading…"),
        "a loaded place shows its rows, not the waiting wording, got:\n{}",
        screen(&view)
    );
}

#[test]
fn the_top_the_indexes_and_no_country_never_say_loading() {
    // The top is a section list; it never requests and never waits.
    let mut view = view();
    view.set_loading(Some(Place::Top));
    let top = screen(&view);
    assert!(
        !top.contains("⟳ loading…"),
        "the top never says loading, got:\n{top}"
    );

    // The two indexes do request, but their empty lists never say loading
    // (S10): the footer carries the wait, the list keeps its own wording.
    let mut bare = RadioView::new(store(), Vec::new(), Vec::new(), None);
    bare.open_place(Place::Countries);
    bare.set_loading(Some(Place::Countries));
    let countries = screen(&bare);
    assert!(
        countries.contains("no stations here") && !countries.contains("⟳ loading…"),
        "the country index never says loading, got:\n{countries}"
    );
    bare.open_place(Place::Tags);
    bare.set_loading(Some(Place::Tags));
    let tags = screen(&bare);
    assert!(
        tags.contains("no stations here") && !tags.contains("⟳ loading…"),
        "the tag index never says loading, got:\n{tags}"
    );

    // The no-country place is the user's own data: never a request, never
    // a wait (S4, S10).
    bare.open_place(Place::NoCountry);
    bare.set_loading(Some(Place::NoCountry));
    let no_country = screen(&bare);
    assert!(
        no_country.contains("Night FM") && !no_country.contains("⟳ loading…"),
        "the no-country place never says loading, got:\n{no_country}"
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
    assert_eq!(
        view.labels().len(),
        8,
        "clearing restores every section row and the favourites' rule and header"
    );
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
    assert_eq!(
        view.labels().len(),
        8,
        "the top level is unfiltered and shows sections only"
    );
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
    view.open_place(norway());
    view.set_catalog(
        norway(),
        vec![station(
            "Nova",
            "https://stream.example/stream",
            "MP3",
            128,
            "NO",
        )],
    );

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
fn enter_and_a_on_an_unplayable_station_never_queue() {
    let mut view = view();
    press(&mut view, KeyCode::Enter); // country index
    press(&mut view, KeyCode::Enter); // Norway
    view.set_catalog(norway(), catalog());
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
    press(&mut view, KeyCode::Enter); // country index
    press(&mut view, KeyCode::Enter); // Norway
    view.set_catalog(norway(), catalog());
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
    assert_eq!(view.place(), &norway(), "playing does not navigate");
    assert_eq!(view.cursor(), 0, "the selection is held");
}

#[test]
fn a_appends_a_station_without_starting_it() {
    let mut view = view();
    press(&mut view, KeyCode::Enter); // country index
    press(&mut view, KeyCode::Enter); // Norway
    view.set_catalog(norway(), catalog());
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
    assert_eq!(view.place(), &norway(), "a does not leave the view");
}

#[test]
fn a_hand_added_or_favourite_station_queues_too() {
    let mut view = view();
    // The favourite sits below the sections, behind the rule and its header
    // (S5, #22): the last row a person can act on.
    press(&mut view, KeyCode::Char('G'));
    assert_eq!(
        press(&mut view, KeyCode::Enter),
        RadioOutcome::Play {
            url: String::from("https://jazz.example/stream"),
            name: String::from("Jazz One"),
        },
        "a favourite plays like any station"
    );
    // The hand-added station lives in the no-country section (S4).
    press(&mut view, KeyCode::Char('g')); // back to the first row
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
    for _ in 0..4 {
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

// ---- the shell wiring (T6c): from the view into the queue --------------------

#[test]
fn a_country_stations_show_in_the_country_and_nowhere_else() {
    let mut app = App::new(Vec::new());
    shell(&mut app, KeyCode::Char('R'));
    app.show_radio_catalog(
        norway(),
        vec![station(
            "Nova",
            "https://stream.example/nova.mp3",
            "MP3",
            128,
            "NO",
        )],
    );
    assert!(
        app_screen(&app).contains("Nova [128k] · NO"),
        "the country lists its own stations"
    );

    // Back out: the top lists sections only, never the place's stations.
    shell(&mut app, KeyCode::Esc);
    let screen = app_screen(&app);
    assert!(
        !screen.contains("Nova"),
        "the top screen lists no station rows, got:\n{screen}"
    );
}

#[test]
fn the_tag_place_behaves_the_same_way() {
    let jazz = Place::Tag {
        name: String::from("jazz"),
    };
    let mut view = view();
    for _ in 0..4 {
        press(&mut view, KeyCode::Char('j')); // Browse genres & tags
    }
    press(&mut view, KeyCode::Enter); // the tag index
    press(&mut view, KeyCode::Enter); // jazz
    assert_eq!(view.place(), &jazz);
    assert!(
        view.labels().is_empty(),
        "no answer yet: the tag place lists nothing, got: {:?}",
        view.labels()
    );
    // A country's answer never renders inside a tag.
    view.set_catalog(norway(), catalog());
    assert!(
        view.labels().is_empty(),
        "a country's answer is not the tag's, got: {:?}",
        view.labels()
    );
    view.set_catalog(jazz.clone(), catalog());
    assert_eq!(
        view.labels(),
        ["Nova [128k] · NO", "✗ Hls One [96k] · NO"],
        "the tag's own answer renders, got: {:?}",
        view.labels()
    );

    // Back out: the tag index lists tags, not stations.
    assert!(view.back());
    assert_eq!(view.place(), &Place::Tags);
    assert_eq!(
        view.labels(),
        ["jazz", "lofi"],
        "the tag index lists tags, got: {:?}",
        view.labels()
    );
}

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

#[test]
fn enter_on_a_station_says_it_is_opening_before_the_player_answers() {
    let mut app = App::new(Vec::new());
    shell(&mut app, KeyCode::Char('R')); // the radio opens over its wiring
    app.show_radio_catalog(
        norway(),
        vec![station(
            "Nova",
            "https://stream.example/nova.mp3",
            "MP3",
            128,
            "NO",
        )],
    );
    shell(&mut app, KeyCode::Char('G')); // the last row: Nova

    let action = shell(&mut app, KeyCode::Enter);
    assert_eq!(
        action,
        Action::PlayQueue { from: 0 },
        "the player was asked to start at the station"
    );
    assert_eq!(
        app.status(),
        "velo: opening Nova…",
        "the status names the station while the player is opening it"
    );
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
    app.show_radio_catalog(
        norway(),
        vec![station(
            "Nova",
            "https://stream.example/nova.mp3",
            "MP3",
            128,
            "NO",
        )],
    );
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
    app.show_radio_catalog(
        norway(),
        vec![station(
            "Nova",
            "https://stream.example/nova.mp3",
            "MP3",
            128,
            "NO",
        )],
    );
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

/// S8 (issue #23): `tab` inside the radio is the round trip: the queue
/// shows, the radio stays alive underneath, and `tab` again lands back in
/// the radio at the same place with the same loaded rows.
#[test]
fn tab_from_the_radio_shows_the_queue_and_tab_returns_to_the_same_place() {
    let mut app = App::new(Vec::new());
    shell(&mut app, KeyCode::Char('R')); // the radio opens over its wiring
    app.show_radio_catalog(
        norway(),
        vec![station(
            "Nova",
            "https://stream.example/nova.mp3",
            "MP3",
            128,
            "NO",
        )],
    );
    shell(&mut app, KeyCode::Char('G')); // the last row: Nova

    shell(&mut app, KeyCode::Tab);
    assert_eq!(app.view(), View::Queue);
    assert!(app.radio_open(), "the round trip keeps the radio alive");

    shell(&mut app, KeyCode::Tab);
    assert_eq!(
        app.view(),
        View::Library,
        "tab returns to the screen the radio was opened from"
    );
    let screen = app_screen(&app);
    assert!(
        screen.contains("Nova [128k] · NO"),
        "the loaded list is still the radio's, got:\n{screen}"
    );

    // The place and the selection are held: enter still plays the station
    // the cursor was left on.
    assert_eq!(
        shell(&mut app, KeyCode::Enter),
        Action::PlayQueue { from: 0 },
        "the player was asked to start the station the cursor held"
    );
    assert_eq!(app.queue(), [nova()]);
}

/// S8 (issue #23): while the round trip has the queue on screen, the radio's
/// own keys are asleep — `f` never touches the store, `N` never moves the
/// place, and the queue is untouched.
#[test]
fn the_radio_keys_do_nothing_while_the_queue_is_on_screen() {
    let mut app = App::new(Vec::new());
    shell(&mut app, KeyCode::Char('R'));
    app.show_radio_catalog(
        norway(),
        vec![station(
            "Nova",
            "https://stream.example/nova.mp3",
            "MP3",
            128,
            "NO",
        )],
    );
    shell(&mut app, KeyCode::Char('G')); // the last row: Nova

    shell(&mut app, KeyCode::Tab);
    assert_eq!(app.view(), View::Queue);

    // `f` would toggle the favourite, `N` would jump to the country index:
    // both are the radio's keys, and both are inert while the queue is
    // what the keys act on.
    assert_eq!(shell(&mut app, KeyCode::Char('f')), Action::None);
    assert_eq!(shell(&mut app, KeyCode::Char('N')), Action::None);
    assert_eq!(app.queue(), [] as [Entry; 0], "the queue is untouched");

    shell(&mut app, KeyCode::Tab);
    let screen = app_screen(&app);
    assert!(
        !screen.contains("★ Nova"),
        "f never reached the store, got:\n{screen}"
    );
    assert!(
        screen.contains("Nova [128k] · NO"),
        "N never left the top place, got:\n{screen}"
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

    // The sections come first; the favourites — including the one with no
    // country — close the list behind the rule and the header (S5, #22).
    let section = view
        .labels()
        .iter()
        .position(|label| label == "Stations without a country")
        .expect("the no-country section is listed");
    assert_eq!(
        view.labels(),
        [
            "Stations without a country",
            "", // the dotted rule
            "favourites",
            "★ Jazz One [MP3] · NO",
            "★ Free Radio",
        ],
        "the favourites close the list, got: {:?}",
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

// ---- the refusal of the row under the cursor (S1, issue #19) ----------------

#[test]
fn the_selected_row_says_why_the_filter_rejected_it() {
    let mut view = view();
    press(&mut view, KeyCode::Enter); // country index
    press(&mut view, KeyCode::Enter); // Norway
    view.set_catalog(norway(), catalog());

    press(&mut view, KeyCode::Char('G')); // last row: Hls One
    assert_eq!(
        view.selected_refusal().as_deref(),
        Some("an HLS manifest: its .ts segments cannot be decoded"),
        "the cursor rests on the rejected station: its refusal is readable"
    );

    press(&mut view, KeyCode::Char('k')); // up onto Nova
    assert_eq!(
        view.selected_refusal(),
        None,
        "a playable station carries no refusal"
    );
}

#[test]
fn favourites_and_hand_added_stations_are_never_refusals() {
    // They skip the filter by design, so their playing is never a refusal
    // the way a rejected catalog station's is.
    let mut view = view();
    press(&mut view, KeyCode::Char('G')); // the favourite below the sections
    assert_eq!(
        view.selected_refusal(),
        None,
        "a favourite was put there deliberately"
    );

    // The hand-added station lives in the no-country place (S4).
    press(&mut view, KeyCode::Char('g'));
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
        view.labels()[view.cursor()],
        "Night FM",
        "the cursor rests on the hand-added station"
    );
    assert_eq!(
        view.selected_refusal(),
        None,
        "a hand-added station was put there deliberately"
    );
}

#[test]
fn the_status_line_says_why_the_station_under_the_cursor_cannot_play() {
    // A wire with no directory client: no request ever starts, so the
    // refusal note is the only writer on the status line and the assertion
    // is exact without waiting on the network.
    let mut app = App::new(Vec::new());
    app.set_radio_wire(Box::new(RadioData::<HttpTransport>::new(None, None, None)));
    shell(&mut app, KeyCode::Char('R')); // the radio opens over its wiring
    app.show_radio_catalog(norway(), catalog());

    // The cursor starts on Nova: playable, nothing said.
    assert_eq!(app.status(), "", "a playable row says nothing");

    shell(&mut app, KeyCode::Char('G')); // onto Hls One
    assert_eq!(
        app.status(),
        "velo: Hls One cannot play (HLS manifest) — i reports it",
        "the status names the station and the reason while the cursor rests there"
    );

    shell(&mut app, KeyCode::Char('k')); // back onto Nova
    assert_eq!(
        app.status(),
        "",
        "the note goes away when the cursor leaves the row"
    );
}

#[test]
fn moving_between_rejected_stations_says_the_new_reason_not_the_old() {
    let mut app = App::new(Vec::new());
    app.set_radio_wire(Box::new(RadioData::<HttpTransport>::new(None, None, None)));
    shell(&mut app, KeyCode::Char('R'));
    app.show_radio_catalog(
        norway(),
        vec![
            station(
                "Hls One",
                "https://stream.example/live/index.m3u8",
                "MP3",
                96,
                "NO",
            ),
            station(
                "Hls Two",
                "https://stream.example/live/other.m3u8",
                "MP3",
                96,
                "NO",
            ),
        ],
    );
    shell(&mut app, KeyCode::Char('j')); // onto Hls Two
    assert_eq!(
        app.status(),
        "velo: Hls Two cannot play (HLS manifest) — i reports it",
        "the note follows the cursor, got: {:?}",
        app.status()
    );
}

/// S1 and S3 together: the note has to carry the key, and it has to survive the
/// width of a normal terminal — an 80-column line is where the message is read,
/// and a hint that gets cut off by the border is a hint nobody reads.
#[test]
fn the_refusal_note_names_the_key_and_fits_a_standard_terminal() {
    let mut app = App::new(Vec::new());
    app.set_radio_wire(Box::new(RadioData::<HttpTransport>::new(None, None, None)));
    shell(&mut app, KeyCode::Char('R'));
    app.show_radio_catalog(norway(), catalog());

    shell(&mut app, KeyCode::Char('G')); // onto Hls One
    let status = app.status();
    assert!(
        status.contains('i'),
        "the note names the key that reports it, got: {status:?}"
    );
    assert!(
        status.contains("i reports it"),
        "and says what the key does, got: {status:?}"
    );
    assert!(
        status.chars().count() < 80,
        "the note fits a standard terminal, got {} columns: {status:?}",
        status.chars().count()
    );
}

// ---- the report of a rejected station (S3, issue #19) ----------------------

// The opener's per-thread record: thread-local because the tests run in
// parallel threads and a fn pointer cannot capture state, so each thread
// records into its own list.
thread_local! {
    // The URLs the opener of the running test's thread was handed, in order.
    static OPENER_URLS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    // Whether the opener of this thread succeeds; a failure stands in for
    // a system without a browser.
    static OPENER_SUCCEEDS: Cell<bool> = const { Cell::new(true) };
}

/// A plain function pointer the shell can hold: it records the URL and
/// answers whatever [`OPENER_SUCCEEDS`] says. No test ever spawns a browser.
fn recording_opener(url: &str) -> Result<(), String> {
    OPENER_URLS.with(|urls| urls.borrow_mut().push(url.to_owned()));
    if OPENER_SUCCEEDS.with(Cell::get) {
        Ok(())
    } else {
        Err(String::from("no browser here"))
    }
}

fn reset_opener(succeeds: bool) {
    OPENER_URLS.with(|urls| urls.borrow_mut().clear());
    OPENER_SUCCEEDS.with(|flag| flag.set(succeeds));
}

/// The URLs the opener received, in order.
fn opened_urls() -> Vec<String> {
    OPENER_URLS.with(|urls| urls.borrow().clone())
}

/// A store file in the system temp directory with one favourite and one
/// hand-added station, so the shell tests can stand on those rows. The test
/// removes it before it ends; nothing is ever written inside the worktree.
fn store_file() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "velo-ui-radio-report-{}-{}.toml",
        std::process::id(),
        OPENER_CALLS.fetch_add(1, Ordering::SeqCst),
    ));
    std::fs::write(
        &path,
        "[[favorite]]\nname = \"Jazz One\"\nurl = \"https://jazz.example/stream\"\n\
         countrycode = \"NO\"\ncodec = \"MP3\"\n\n[[station]]\nname = \"Night FM\"\n\
         url = \"http://night.example/live\"\n",
    )
    .expect("the store fixture writes");
    path
}

static OPENER_CALLS: AtomicUsize = AtomicUsize::new(0);

#[test]
fn i_on_a_rejected_station_opens_a_prefilled_issue_in_the_browser() {
    reset_opener(true);
    let mut app = App::new(Vec::new());
    app.set_url_opener(recording_opener);
    app.set_radio_wire(Box::new(RadioData::<HttpTransport>::new(None, None, None)));
    shell(&mut app, KeyCode::Char('R'));
    app.show_radio_catalog(norway(), catalog());
    shell(&mut app, KeyCode::Char('G')); // onto Hls One, marked ✗

    shell(&mut app, KeyCode::Char('i'));

    let version = env!("CARGO_PKG_VERSION");
    let expected = format!(
        "https://github.com/alvaldes/velo/issues/new?title=Radio%20station%20\
         %22Hls%20One%22%20does%20not%20play&body=Station%3A%20Hls%20One%0A\
         Codec%3A%20MP3%0AURL%3A%20https%3A%2F%2Fstream.example%2Flive%2F\
         index.m3u8%0AReason%3A%20an%20HLS%20manifest%3A%20its%20.ts%20\
         segments%20cannot%20be%20decoded%0A%0Avelo%20{version}%20on%20{}%20\
         %28{}%29&labels=bug",
        std::env::consts::OS,
        std::env::consts::ARCH,
    );
    assert_eq!(
        opened_urls(),
        [expected],
        "the opener received the prefilled issue: title and body percent-encoded, \
         the station's name, codec and url_resolved, the refusal from the directory, \
         velo's version and the platform in the body, the bug label attached"
    );
}

#[test]
fn one_keypress_hands_over_exactly_one_report() {
    reset_opener(true);
    let mut app = App::new(Vec::new());
    app.set_url_opener(recording_opener);
    app.set_radio_wire(Box::new(RadioData::<HttpTransport>::new(None, None, None)));
    shell(&mut app, KeyCode::Char('R'));
    app.show_radio_catalog(norway(), catalog());
    shell(&mut app, KeyCode::Char('G')); // onto Hls One, marked ✗

    shell(&mut app, KeyCode::Char('i'));
    assert_eq!(
        opened_urls().len(),
        1,
        "one keypress, one opener call, got: {:?}",
        opened_urls()
    );

    // Moving the cursor and pressing every other radio key opens nothing.
    shell(&mut app, KeyCode::Char('k'));
    shell(&mut app, KeyCode::Char('f'));
    shell(&mut app, KeyCode::Char('j'));
    assert_eq!(
        opened_urls().len(),
        1,
        "the opener was called exactly once for the one keypress, got: {:?}",
        opened_urls()
    );
}

#[test]
fn i_passes_on_every_row_but_a_rejected_catalog_station() {
    // The full top level: every section row, the rule, the header and the
    // favourite — `i` on each of them passes through untouched.
    let mut view = view();
    let top = view.labels().len();
    for _ in 0..top {
        assert_eq!(
            press(&mut view, KeyCode::Char('i')),
            RadioOutcome::Pass,
            "on row {:?} the key falls through: no URL is built and a global \
             binding stays free",
            view.cursor()
        );
        press(&mut view, KeyCode::Char('j'));
    }

    // A playable catalog station is not reportable either.
    press(&mut view, KeyCode::Char('g')); // back to the first row
    press(&mut view, KeyCode::Enter); // the country index
    press(&mut view, KeyCode::Enter); // Norway
    view.set_catalog(norway(), catalog());
    press(&mut view, KeyCode::Char('g')); // onto Nova, playable
    assert_eq!(
        press(&mut view, KeyCode::Char('i')),
        RadioOutcome::Pass,
        "a playable station has no report to file"
    );

    // The rejected station is the one row that answers with the report.
    press(&mut view, KeyCode::Char('j')); // onto Hls One, marked ✗
    let outcome = press(&mut view, KeyCode::Char('i'));
    let RadioOutcome::ReportIssue { title, body } = outcome else {
        panic!("the rejected station answers with the report, got: {outcome:?}")
    };
    assert_eq!(
        title, "Radio station \"Hls One\" does not play",
        "the title names the station"
    );
    for fact in [
        "Station: Hls One",
        "Codec: MP3",
        "URL: https://stream.example/live/index.m3u8",
        "Reason: an HLS manifest: its .ts segments cannot be decoded",
    ] {
        assert!(
            body.contains(fact),
            "the body carries {fact:?}, got: {body:?}"
        );
    }

    // A hand-added station, down in the no-country place, is deliberate
    // user data: it passes through too. Back out of Norway first: the top
    // is where the section lives.
    assert!(view.back(), "out of Norway");
    assert!(view.back(), "out of the country index");
    press(&mut view, KeyCode::Char('g'));
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
        press(&mut view, KeyCode::Char('i')),
        RadioOutcome::Pass,
        "a hand-added station is not reportable"
    );
}

#[test]
fn i_on_any_other_row_never_reaches_an_opener_or_the_status_line() {
    let path = store_file();

    // A playable catalog station.
    reset_opener(true);
    let mut app = App::new(Vec::new());
    app.set_url_opener(recording_opener);
    app.set_radio_wire(Box::new(RadioData::<HttpTransport>::new(None, None, None)));
    shell(&mut app, KeyCode::Char('R'));
    app.show_radio_catalog(
        norway(),
        vec![station(
            "Nova",
            "https://stream.example/nova.mp3",
            "MP3",
            128,
            "NO",
        )],
    );
    shell(&mut app, KeyCode::Char('G'));
    shell(&mut app, KeyCode::Char('i'));
    assert!(
        opened_urls().is_empty(),
        "no URL for a playable station, got: {:?}",
        opened_urls()
    );
    assert_eq!(app.status(), "", "the status line stays empty");

    // A non-station row: the first browse row of the top level.
    let mut app = App::new(Vec::new());
    app.set_url_opener(recording_opener);
    app.set_radio_wire(Box::new(RadioData::<HttpTransport>::new(None, None, None)));
    shell(&mut app, KeyCode::Char('R'));
    shell(&mut app, KeyCode::Char('i'));
    assert!(
        opened_urls().is_empty(),
        "no URL for a browse row, got: {:?}",
        opened_urls()
    );
    assert_eq!(app.status(), "", "the status line stays empty");

    // A favourite and a hand-added station, from the store file: the
    // favourite is the last row of the top, the added station the first
    // row of the no-country place.
    let mut app = App::new(Vec::new());
    app.set_url_opener(recording_opener);
    app.set_radio_wire(Box::new(RadioData::<HttpTransport>::new(
        None,
        Some(path.clone()),
        None,
    )));
    shell(&mut app, KeyCode::Char('R'));
    shell(&mut app, KeyCode::Char('G')); // the favourite below the sections
    shell(&mut app, KeyCode::Char('i'));
    assert!(
        opened_urls().is_empty(),
        "no URL for a favourite, got: {:?}",
        opened_urls()
    );
    assert_eq!(app.status(), "", "the status line stays empty");

    shell(&mut app, KeyCode::Char('g'));
    let screen = app_screen(&app);
    let section = screen
        .lines()
        .position(|line| line.contains("Stations without a country"))
        .expect("the no-country section is on screen");
    let _ = section;
    // From the top: one `j` onto `Stations without a country`, `enter` in.
    shell(&mut app, KeyCode::Char('j'));
    shell(&mut app, KeyCode::Enter);
    shell(&mut app, KeyCode::Char('i'));
    assert!(
        opened_urls().is_empty(),
        "no URL for a hand-added station, got: {:?}",
        opened_urls()
    );
    assert_eq!(app.status(), "", "the status line stays empty");

    std::fs::remove_file(&path).expect("the store fixture is removed");
}

#[test]
fn a_failed_opener_leaves_the_report_url_on_the_status_line() {
    reset_opener(false);
    let mut app = App::new(Vec::new());
    app.set_url_opener(recording_opener);
    app.set_radio_wire(Box::new(RadioData::<HttpTransport>::new(None, None, None)));
    shell(&mut app, KeyCode::Char('R'));
    app.show_radio_catalog(norway(), catalog());
    shell(&mut app, KeyCode::Char('G')); // onto Hls One, marked ✗

    shell(&mut app, KeyCode::Char('i'));

    let status = app.status().to_owned();
    assert!(
        status.contains("https://github.com/alvaldes/velo/issues/new?"),
        "nothing is lost: the status line carries the URL itself, got: {status:?}"
    );
    assert!(
        status.ends_with(&opened_urls()[0]),
        "the URL on the line is exactly the one the opener refused"
    );
}

#[test]
fn a_successful_report_says_it_waits_in_the_browser_untouched() {
    reset_opener(true);
    let mut app = App::new(Vec::new());
    app.set_url_opener(recording_opener);
    app.set_radio_wire(Box::new(RadioData::<HttpTransport>::new(None, None, None)));
    shell(&mut app, KeyCode::Char('R'));
    app.show_radio_catalog(norway(), catalog());
    shell(&mut app, KeyCode::Char('G')); // onto Hls One, marked ✗

    shell(&mut app, KeyCode::Char('i'));

    let status = app.status().to_owned();
    assert!(
        status.contains("browser"),
        "the status says where the report is, got: {status:?}"
    );
    assert!(
        status.to_lowercase().contains("send"),
        "sending the report is the human's to do, got: {status:?}"
    );
    assert!(
        !status.to_lowercase().contains("sent"),
        "the report is never claimed sent, got: {status:?}"
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
        "Browse all countries",
        "Norway (near you)",
        "★ Germany",
        "Stations without a country",
        "Browse genres & tags",
        "favourites",
        "★ Jazz One",
    ] {
        assert!(
            screen.contains(expected),
            "the radio list must show {expected:?}, got:\n{screen}"
        );
    }
    assert!(
        !screen.contains("Nova") && !screen.contains("Hls One"),
        "the top screen lists no station rows (S9), got:\n{screen}"
    );

    // The dotted rule is a line of dots across the width, and the rule and
    // the header take no number: the numbering counts only the rows a person
    // can act on (S5, #22), so the favourite keeps the last number.
    assert!(
        screen
            .lines()
            .any(|line| !line.is_empty() && line.chars().all(|character| character == '·')),
        "the rule is a dotted line, got:\n{screen}"
    );
    assert!(
        !screen.contains("7. ") && !screen.contains("5. ·"),
        "the rule and the header take no row number, got:\n{screen}"
    );
    assert!(
        screen.contains("6. ★ Jazz One"),
        "the favourite keeps the last selectable number, got:\n{screen}"
    );

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

// ---- a station that cannot play is marked wherever it appears (S1) --------

/// A store with the three cases the marker has to tell apart: a refused
/// favourite, a playable favourite, and both kinds among the hand-added
/// stations. Favourites stay playable on purpose; the marker only states the
/// fact.
fn store_with_refused_stations() -> RadioStore {
    let mut store = RadioStore::default();
    store.favorite.push(Favorite {
        name: "Hls Fav".to_owned(),
        url: "https://fav.example/live.m3u8".to_owned(),
        countrycode: "NO".to_owned(),
        codec: "MP3".to_owned(),
    });
    store.favorite.push(Favorite {
        name: "Good Fav".to_owned(),
        url: "https://fav.example/good".to_owned(),
        countrycode: "NO".to_owned(),
        codec: "MP3".to_owned(),
    });
    store.favorite.push(Favorite {
        name: "No Country Hls".to_owned(),
        url: "https://fav.example/nc.m3u8".to_owned(),
        countrycode: String::new(),
        codec: "AAC".to_owned(),
    });
    store.station.push(store::Station {
        name: "Mine Hls".to_owned(),
        url: "https://mine.example/live.m3u8".to_owned(),
    });
    store.station.push(store::Station {
        name: "Mine Good".to_owned(),
        url: "https://mine.example/good".to_owned(),
    });
    store
}

fn view_with_refused_stations() -> RadioView {
    RadioView::new(store_with_refused_stations(), Vec::new(), Vec::new(), None)
}

/// The favourites section marks the one that cannot play and leaves the other
/// alone — the fact does not depend on how the station got there.
#[test]
fn a_favourite_that_cannot_play_is_marked_and_a_playable_one_is_not() {
    let view = view_with_refused_stations();
    let labels = view.labels();
    assert!(
        labels.iter().any(|label| label == "★ ✗ Hls Fav [MP3] · NO"),
        "the refused favourite is marked, got: {labels:?}"
    );
    assert!(
        labels.iter().any(|label| label == "★ Good Fav [MP3] · NO"),
        "the playable favourite is not marked, got: {labels:?}"
    );
}

/// The hand-added stations and the no-country favourites are marked the same
/// way inside their own section.
#[test]
fn a_hand_added_station_that_cannot_play_is_marked() {
    let mut view = view_with_refused_stations();
    view.open_place(Place::NoCountry);
    let labels = view.labels();
    assert!(
        labels.iter().any(|label| label == "✗ Mine Hls"),
        "the refused hand-added station is marked, got: {labels:?}"
    );
    assert!(
        labels.iter().any(|label| label == "Mine Good"),
        "the playable hand-added station is not marked, got: {labels:?}"
    );
    assert!(
        labels
            .iter()
            .any(|label| label == "★ ✗ No Country Hls [AAC]"),
        "a no-country favourite is marked too, got: {labels:?}"
    );
}

/// The distinction the whole rule rests on: marked, and still playable — you
/// put it there deliberately, so `enter` tries it instead of refusing.
#[test]
fn a_refused_favourite_is_still_playable_on_purpose() {
    let mut view = view_with_refused_stations();
    view.open_place(Place::NoCountry);
    view.select_first();

    assert_eq!(
        view.labels().first().map(String::as_str),
        Some("✗ Mine Hls")
    );
    assert_eq!(
        view.key(KeyEvent::from(KeyCode::Enter)),
        RadioOutcome::Play {
            url: "https://mine.example/live.m3u8".to_owned(),
            name: "Mine Hls".to_owned(),
        },
        "a marked favourite or hand-added station is still played on purpose"
    );
}

/// And it can be reported: the refusal it shows is the one the report carries.
#[test]
fn a_refused_favourite_says_why_and_can_be_reported() {
    let mut view = view_with_refused_stations();
    view.open_place(Place::NoCountry);
    view.select_first();

    assert_eq!(
        view.selected_refusal().as_deref(),
        Some("an HLS manifest: its .ts segments cannot be decoded")
    );

    let RadioOutcome::ReportIssue { title, body } = view.key(KeyEvent::from(KeyCode::Char('i')))
    else {
        panic!("i on a refused station files a report");
    };
    assert!(title.contains("Mine Hls"), "got: {title}");
    assert!(
        body.contains("https://mine.example/live.m3u8"),
        "got: {body}"
    );
    assert!(
        body.contains("an HLS manifest: its .ts segments cannot be decoded"),
        "the report carries the sentence, got: {body}"
    );
    assert!(
        body.contains("unknown"),
        "a hand-added station claims no codec, and the report says so, got: {body}"
    );
}

/// The other side of the same rule: a playable favourite is not marked, states
/// no refusal, and `i` passes through.
#[test]
fn a_playable_favourite_is_never_marked_and_cannot_be_reported() {
    let mut view = view_with_refused_stations();
    view.open_place(Place::NoCountry);
    view.select_next(); // Mine Good

    assert_eq!(view.labels()[1], "Mine Good");
    assert_eq!(view.selected_refusal(), None);
    assert_eq!(
        view.key(KeyEvent::from(KeyCode::Char('i'))),
        RadioOutcome::Pass,
        "i belongs to no other row"
    );
}
