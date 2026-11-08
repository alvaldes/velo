//! The station's country, read from the locale and only from the locale.
//!
//! "Near you" needs an ISO 3166-1 code, and the locale carries one directly:
//! `es_AR.UTF-8` is Argentina, `de_DE.utf8@euro` is Germany. A timezone would
//! first give a city and then need a timezone→country database — a dependency
//! or a hand-maintained table of hundreds of rows — so the plan's "timezone
//! first" step is dropped on purpose (spec S5): a wrong "near you" row is
//! worse than no row.
//!
//! Everything here is pure: no files are read, no environment is mutated,
//! nothing is written. The caller decides whether to ask and what to store,
//! and validates the answer against the directory's `/countries` list from
//! the client — this module does not know that list.

/// The country code from a locale string in the POSIX shape
/// `language[_territory][.codeset][@modifier]`, or `None` when there is no
/// usable territory.
///
/// The territory must be exactly two ASCII letters; it is upper-cased on the
/// way out (`pt_br` → `BR`). `C` and `POSIX` are not locales with a country,
/// and neither are `en`, `en_`, `en_USA`, `en_1A` or the empty string.
#[must_use]
pub fn from_locale(locale: &str) -> Option<String> {
    // `@modifier` and `.codeset` both come after the territory, so cutting
    // them off first leaves `language_territory`; trimming first keeps a
    // trailing newline from an environment value harmless.
    let head = locale.trim().split(['.', '@']).next()?;
    let territory = head.split_once('_')?.1;
    if territory.len() == 2 && territory.bytes().all(|b| b.is_ascii_alphabetic()) {
        Some(territory.to_ascii_uppercase())
    } else {
        None
    }
}

/// The country from the environment, walking `LC_ALL`, `LC_MESSAGES` and
/// `LANG` in that order. The first variable that yields a country wins; one
/// that is unset or has no usable territory falls through to the next. When
/// nothing yields a country there is no "near you" row.
///
/// The environment arrives as a closure so this stays pure and testable; the
/// production caller passes `|key| std::env::var(key).ok()`.
#[must_use]
pub fn detect(env: impl Fn(&str) -> Option<String>) -> Option<String> {
    ["LC_ALL", "LC_MESSAGES", "LANG"].into_iter().find_map(|key| {
        let locale = env(key)?;
        from_locale(&locale)
    })
}
