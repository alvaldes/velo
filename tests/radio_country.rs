//! Country detection tests (spec S5): the station's country comes from the
//! locale — `LC_ALL`, then `LC_MESSAGES`, then `LANG` — never from a
//! timezone, never from geo-IP, and nothing is read, written or mutated to
//! get it. The environment always arrives as a closure.
//!
//! All fixtures are string literals in this file: no network, no files
//! outside the repo, no new dependencies, no real environment reads.

use velo::radio::country::{detect, from_locale};

/// An environment map for `detect`: the closure the production caller would
/// build from `std::env::var`, pinned to these literals.
fn env_of<'a>(entries: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
    move |key| {
        entries
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| value.to_string())
    }
}

/// An environment where the named variables are simply absent.
fn env_without() -> impl Fn(&str) -> Option<String> {
    |_| None
}

#[test]
fn territory_with_codeset() {
    assert_eq!(from_locale("es_AR.UTF-8"), Some("AR".to_string()));
}

#[test]
fn territory_alone() {
    assert_eq!(from_locale("en_US"), Some("US".to_string()));
}

#[test]
fn territory_with_codeset_and_modifier() {
    assert_eq!(from_locale("de_DE.utf8@euro"), Some("DE".to_string()));
}

#[test]
fn lower_case_territory_is_normalised() {
    assert_eq!(from_locale("pt_br"), Some("BR".to_string()));
}

#[test]
fn locale_with_modifier_but_no_codeset() {
    assert_eq!(from_locale("es_AR@latin"), Some("AR".to_string()));
}

#[test]
fn surrounding_whitespace_is_trimmed_first() {
    assert_eq!(from_locale(" es_AR.UTF-8 "), Some("AR".to_string()));
}

#[test]
fn c_locale_has_no_country() {
    assert_eq!(from_locale("C"), None);
}

#[test]
fn posix_locale_has_no_country() {
    assert_eq!(from_locale("POSIX"), None);
}

#[test]
fn empty_locale_has_no_country() {
    assert_eq!(from_locale(""), None);
}

#[test]
fn language_without_territory_has_no_country() {
    assert_eq!(from_locale("en"), None);
}

#[test]
fn trailing_separator_with_no_territory_has_no_country() {
    assert_eq!(from_locale("en_"), None);
}

#[test]
fn three_letter_territory_has_no_country() {
    assert_eq!(from_locale("en_USA"), None);
}

#[test]
fn territory_with_a_digit_has_no_country() {
    assert_eq!(from_locale("en_1A"), None);
    assert_eq!(from_locale("en_150"), None);
}

#[test]
fn detect_walks_lc_all_then_lc_messages_then_lang() {
    // LC_ALL wins over both others.
    let env = env_of(&[("LC_ALL", "es_AR.UTF-8"), ("LC_MESSAGES", "de_DE"), ("LANG", "en_US")]);
    assert_eq!(detect(env), Some("AR".to_string()));
}

#[test]
fn detect_lc_messages_wins_over_lang() {
    let env = env_of(&[("LC_MESSAGES", "de_DE"), ("LANG", "en_US")]);
    assert_eq!(detect(env), Some("DE".to_string()));
}

#[test]
fn detect_lang_alone_still_answers() {
    let env = env_of(&[("LANG", "fr_FR")]);
    assert_eq!(detect(env), Some("FR".to_string()));
}

#[test]
fn detect_falls_through_when_a_higher_variable_has_no_country() {
    // An LC_ALL without a usable territory must not stop the walk.
    let env = env_of(&[("LC_ALL", "C"), ("LC_MESSAGES", "de_DE"), ("LANG", "en_US")]);
    assert_eq!(detect(env), Some("DE".to_string()));
}

#[test]
fn detect_ignores_unset_variables() {
    let env = env_of(&[("LC_MESSAGES", "de_DE"), ("LANG", "en_US")]);
    assert_eq!(detect(env), Some("DE".to_string()));
}

#[test]
fn detect_with_no_country_anywhere_is_none() {
    assert_eq!(detect(env_without()), None);
    let env = env_of(&[("LC_ALL", "POSIX"), ("LANG", "en")]);
    assert_eq!(detect(env), None);
}

#[test]
fn detect_trims_values_like_the_locale_parser() {
    // The same value with and without trailing whitespace decides alike,
    // and a value that only whitespace yields nothing for the next
    // variable to win.
    let trimmed = env_of(&[("LC_ALL", "es_AR.UTF-8")]);
    let padded = env_of(&[("LC_ALL", "es_AR.UTF-8 ")]);
    assert_eq!(detect(trimmed), detect(padded));
    let blank = env_of(&[("LC_ALL", "  "), ("LANG", "en_US")]);
    assert_eq!(detect(blank), Some("US".to_string()));
}

#[test]
fn detect_never_touches_a_real_environment() {
    // The contract: the closure is the only window onto the environment.
    // Passing one that answers nothing must produce `None` even though the
    // ambient environment may carry LC_ALL/LC_MESSAGES/LANG.
    assert_eq!(detect(|_| None), None);
}
