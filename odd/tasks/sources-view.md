# Feature: sources view (issue #3)
Branch `feat/sources-view`; deliverable: one PR against `main`.

## Specs

Quoted verbatim from issue #3 (github.com/alvaldes/velo/issues/3).

- **S1** "A **sources** view as the first thing on screen, with one level of choice under it."
- **S2** "The first screen lists what velo can play: `Local library` and `Radio`, with the cursor already on it. The library's current views become what sits *inside* `Local library`."
- **S3** "The library's current views become what sits *inside* `Local library`."
- **S4** "Entering `Radio` shows the stations view, which today has countries, the home country's regions, genres and tags, and the directory's catalogue. It should also have a section for stations that belong to no country — an independent station of our own is the case in mind — so that \"radio\" is not only \"radio in a country\"."
- **S5** "**Favourites get their own section at the top of the radio view**, so the stations someone actually listens to are one keypress away instead of buried under a country."
- **S6** "`esc` walks back one level at a time and lands on `Sources`, so the way out of any screen is the way in."
- **S7** "Once a source is chosen, nothing about its own keys changes: the library keys stay the library's, the radio keys stay the radio's."

## Decisions taken from the issue (not extra requirements)

- `Sources` is a new top-level state, **not** a fifth tab: `Tab` keeps toggling Library↔Queue and stays meaningless on `Sources`.
- `R` keeps working as an accelerator from inside the library (S7: the radio's keys do not change); `Sources` is the discoverable path, `R` is the shortcut.
- `Sources` exists only in browsing mode. A file launch (`App::new`) still starts at the queue with `Position::Files`, unchanged.
- The "no country" section holds the user's **own** stations (`[[station]]`) and favourites with no `countrycode`. No new directory query: `StationSearch.countrycode` is already optional, and the issue's case is an independent station of our own.
- The radio view keeps reporting "close" to the shell; the shell decides that closing the radio lands on `Sources` (S6). The radio module never learns about sources.

## Tasks

| ID | Specs | Route | Surface | Commit |
| --- | --- | --- | --- | --- |
| T1 | S1 S2 S3 S6 S7 | worker A | `src/ui/mod.rs`, `src/ui/sources.rs`, `src/bin/velo.rs`, `tests/ui_sources.rs`, `tests/ui_state.rs` | `e6a3bb7` — slice 1, 462 lines |
| T2 | S4 S5 | worker B | `src/ui/radio.rs`, `src/ui/radio_data.rs`, `tests/ui_radio.rs`, `tests/ui_radio_data.rs` | `17bc33e` — slice 2, 337 lines |
| T3 | S1–S7 | parent | `README.md`, `odd/tasks/sources-view.md` | `docs: document the sources view and the new radio order` — slice 3 |
| T4 | all | parent | — | verification: fmt, clippy, 648 tests, independent pass |

## Log

**L1 — the human's request, verbatim.**

> le damos al issue 3

with the issue as the specification: https://github.com/alvaldes/velo/issues/3

**L2 — implemented, and the two things the independent pass changed.**

T1 (S1, S2, S3, S6, S7): `View::Sources` as a new top-level state, `src/ui/sources.rs`
holding the fixed two-row list and its cursor, `open_sources()` called by
`browse_interface` after `set_session`, `sources_key` intercepting the list keys and
making the library's keys inert there, `esc` from the top of the library and the radio's
`Close` both landing on `Sources`, and `remember_view` never persisting it. `App::browse`
and `App::new` deliberately keep starting on the library: 59 existing test call sites
depend on it, and "the first screen" is a property of the launch, not of the constructor.

T2 (S4, S5): favourites moved to the top of `top_rows`; `Place::NoCountry` plus a
`Stations without a country` row placed after the pins, listing the hand-added stations
then the favourites with an empty `countrycode`, with `place_key` and `refresh` both
returning early so that section never makes a directory request.

Two findings from the independent read, both fixed here:
1. Closing a radio opened with `R` from the library landed on `Sources`, a screen the
   user never crossed. The shell now remembers where the radio was opened (`radio_from`)
   and closing returns there — S6's "the way out is the way in" taken literally.
2. `favourites_come_first_at_the_top` only proved that row 0 was a favourite, so it could
   not catch a favourite interleaved further down. It now counts the favourites and
   requires them to be exactly the leading rows.

Evidence: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and
`cargo test` — **36 suites, 648 passed, 0 failed, 3 pre-existing ignored**. The
independent verifier's baseline comparison found one environment-dependent failure
(`the_library_prompt_is_a_floating_card`, which embeds `$HOME` into a 90-column card)
failing identically on `main` with a scratch `HOME`; it does not reproduce here.

**L3 — the slice boundary, recorded as the delivery strategy requires.** Two code
slices rather than one: the sources screen (`e6a3bb7`, 462 authored lines) and the radio
order (`17bc33e`, 337), because they are independent of each other — the radio section
never learns about sources — so a reviewer can take them apart. 799 authored lines in
total, past the ~400-line slice on purpose and disclosed: the split is two commits, and
the pull-request split is the human's to choose (one PR with both, or one per commit).
