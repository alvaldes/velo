//! The fuzzy matcher behind `/` (T7, S9), tested as the pure thing it is:
//! query text in, ranked candidate indices out. No terminal, no library,
//! no audio lives here — that is what makes these rules cheap to pin.

use velo::ui::search::Search;

/// A query typed one character at a time, as the interface types it.
fn typed(query: &str) -> Search {
    let mut search = Search::new();
    for character in query.chars() {
        search.push(character);
    }
    search
}

fn ranked(query: &str, candidates: &[&str]) -> Vec<usize> {
    typed(query).rank(candidates)
}

#[test]
fn the_query_matches_case_insensitively() {
    // `NIGHT` in caps still finds `Night Drive` — and finds it first, because
    // it is also written right there in `Moonlight`, but scattered.
    assert_eq!(
        ranked("NIGHT", &["Moonlight Sonata", "Night Drive",]),
        [1, 0]
    );
    // And lower-case letters find upper-case text.
    assert_eq!(ranked("MOONLI", &["Moonlight Sonata", "Starlight",]), [0]);
}

#[test]
fn the_query_matches_as_a_subsequence() {
    // Three letters of a track find it through the letters scattered
    // across the rest of the word.
    assert_eq!(ranked("mnlgt", &["Moonlight"]), [0]);
    assert_eq!(ranked("MLGT", &["moonlight"]), [0]);

    // A candidate that does not contain the letters stays out.
    assert_eq!(ranked("mnlgt", &["Moonlight", "Starlight"]), [0]);
}

#[test]
fn the_rank_prefers_a_prefix_over_a_scattered_match() {
    // `hash` starts one candidate and is scattered through the other.
    assert_eq!(ranked("hash", &["Rehash", "Hashbrown"]), [1, 0]);

    // An exact match beats everything.
    assert_eq!(ranked("hash", &["Rehash", "Hashbrown", "hash"]), [2, 1, 0]);
}

#[test]
fn an_empty_query_returns_everything_in_order() {
    let candidates = ["Moonlight", "Night Drive", "Starlight"];
    assert_eq!(ranked("", &candidates), [0, 1, 2]);
    assert_eq!(Search::new().rank(&candidates), [0, 1, 2]);
}

#[test]
fn a_query_that_matches_nothing_returns_nothing() {
    assert_eq!(ranked("zzzz", &["Moonlight", "Night Drive"]), []);
}

#[test]
fn a_query_with_spaces_matches_across_artist_album_and_title() {
    // The candidates the interface searches are whole rows.
    let candidate = "Poldoore Lush Life Summer Breeze";

    assert_eq!(typed("poldoore summer").rank(&[candidate]), [0]);
    assert_eq!(typed("lush breeze").rank(&[candidate]), [0]);

    // Letters that never appear stay out, spaces or not.
    assert_eq!(typed("poldoore zzzz").rank(&[candidate]), []);
}

#[test]
fn the_query_text_is_editable_one_character_at_a_time() {
    let mut search = Search::new();
    assert_eq!(search.query(), "");

    search.push('m');
    search.push('o');
    assert_eq!(search.query(), "mo");

    search.push(' ');
    search.push('o');
    assert_eq!(search.query(), "mo o", "a space is part of the query");

    assert_eq!(search.pop(), Some('o'));
    assert_eq!(search.pop(), Some(' '));
    assert_eq!(search.pop(), Some('o'));
    assert_eq!(search.pop(), Some('m'));
    assert_eq!(search.pop(), None, "backspacing past the first character");
    assert_eq!(search.query(), "");
    assert!(search.is_empty());
}
