# Feature: sources view (issue #3)
Branch `feat/sources-view`; deliverable: one PR against `main`.

## Specs

Quoted verbatim from issue #3 (github.com/alvaldes/velo/issues/3).

- **S1** "A **sources** view as the first thing on screen, with one level of choice under it."
- **S2** "The first screen lists what velo can play: `Local library` and `Radio`, with the cursor already on it. The library's current views become what sits *inside* `Local library`."
- **S3** "The library's current views become what sits *inside* `Local library`."
- **S4** "Entering `Radio` shows the stations view, which today has countries, the home country's regions, genres and tags, and the directory's catalogue. It should also have a section for stations that belong to no country — an independent station of our own is the case in mind — so that \"radio\" is not only \"radio in a country\"."
- **S5** "**Favourites get their own section at the top of the radio view**, so the stations someone actually listens to are one keypress away instead of buried under a country."
  **Revised the same day (#22), by the human:** the favourites go **below** the sections,
  under their own header and a dotted rule that closes the list above them. Verbatim:
  "En source hay que separar fav del resto. arriba lo normal y abajo favs con su header y
  una ....... que delimite".
- **S8** (issue #23) `tab` is one round trip: from `Sources`, the library at any level, or
  the radio view at any depth it goes to the queue; from the queue it returns to exactly
  the screen and state you left, the radio keeping its place and its loaded list. Verbatim:
  "de la cola regreso al estado donde yo estaba, que es el source, ya sea que esté dentro
  de una estación, dentro de un país o dentro de un género o cualquier otra fuente de
  radio".
- **S9** (issue #20) a screen lists what it is: categories list categories, a place lists
  its own stations or says it is loading, and no child or sibling place's rows survive into
  a parent. The library must keep answering the same way. Verbatim: "al entrar a uno de
  estos browse y que carguen la lista de estaciones al regresar se muestran las estaciones
  junto a lo de esa pagina y eso no debe pasar. las categorias muestran categorisas no
  estaciones. si me explico? igual revisa en librari local porque esto tampoco puede pasar
  ahi".
- **S10** (issue #21) a wait says so: a directory request and a live station opening both
  show progress, and the message goes away when the answer lands. Verbatim: "cuando entre a
  por ejemplo a browse ... debe mostrar cargando para que el usuario vea que se esta
  haciendo algo. Igual cuando se reproduce una radio que tarda un tin en cargar".
- **S6** "`esc` walks back one level at a time and lands on `Sources`, so the way out of any screen is the way in."
- **S7** "Once a source is chosen, nothing about its own keys changes: the library keys stay the library's, the radio keys stay the radio's."
- **S11** (issue #26) the title bar names what `tab` is a round trip to: `sources` and
  `queue`, with `sources` lit anywhere inside it — `Sources`, the library, the radio view.
  Verbatim: "arriba sigue diciendo Library queue en vez de source queue".
- **S12** (issue #27) entering `Local library` always lands on the library, and with no path
  configured it opens the library prompt instead of leaving the reader somewhere else.
  Verbatim: "si library no esta definido el path y entro por source a library me manda a
  queue pero lo que debe hacer es mostrarme la ventana para poner el path".
- **S13** (issue #25) the keymap does not collapse a modified key into its plain arm:
  `ctrl+r` never cycles repeat.

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
| T5 | S8 (#23) | worker | `src/ui/mod.rs`, `tests/ui_sources.rs`, `tests/ui_state.rs`, `tests/ui_radio.rs` | `feat(ui): tab is a round trip to the queue and back` |
| T6 | S9 (#20) | worker | `src/ui/radio.rs`, `src/ui/radio_data.rs`, `tests/ui_radio.rs`, `tests/ui_radio_data.rs` | `fix(radio): a place shows its own stations, never another's` |
| T7 | S10 (#21) | worker | `src/ui/radio.rs`, `src/ui/mod.rs`, `tests/ui_radio.rs`, `tests/ui_radio_data.rs` | `feat(ui): a request and a live stream say they are working` |
| T8 | S5 (#22) | worker | `src/ui/radio.rs`, `src/ui/radio_data.rs`, `tests/ui_radio.rs` | `feat(radio): favourites below the sections, behind a header` |
| T9 | S11 S12 S13 (#26 #27 #25) | worker | `src/ui/mod.rs`, `tests/ui_render.rs`, `tests/ui_state.rs`, `tests/ui_sources.rs` | `fix(ui): name sources in the title bar, land the library on the library` |

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

**L4 — the human used it, and found four things. Verbatim.**

> con tab ya no se pasa de library a queue sino de source a queue.
> En source hay que separar fav del resto. arriba lo normal y abajo favs con su header y
> una ....... que delimite
> en source todo tiene su ventana o vista no se mescla. en radio cuando entre a por ejemplo
> a browse ... debe mostrar cargando para que el usuario vea que se esta haciendo algo.
> Igual cuando se reproduce una radio que tarda un tin en cargar.
> Otro detalle es que al entrar a uno de estos browse y que carguen la lista de estaciones
> al regresar se muestran las estaciones junto a lo de esa pagina y eso no debe pasar. las
> categorias muestran categorisas no estaciones. si me explico?
> igual revisa en librari local porque esto tampoco puede pasar ahi.

And on the key, the correction that turned a bug report into a requirement:

> El hecho es mejor donde yo estaba. El hecho es que ahora el tab sigue llevando a Q. Q
> solamente va a tener vistas locales, pero la otra parte que lleva el tab ya no va a ser
> al library, va a ser al solus, pero ese solus igual mantiene el estado de donde estaba
> antes, en el que renuevo a la cola, y de la cola regreso al estado donde yo estaba, que
> es el source, ya sea que esté dentro de una estación, dentro de un país o dentro de un
> género o cualquier otra fuente de radio.

**What reproduction said, before any of it was touched** (four claims, read from the
source at `4b801cf` plus one new commit):

1. **`tab` on `Sources` did not reproduce as "jumps to the queue"**: `sources_key` absorbs
   `Tab` and `tab_is_inert_on_the_sources_screen` passes. The report was a requirement, not
   a defect — hence #23 and S8, which *replaces* the inert behaviour.
2. **The mixing reproduces** (#20, S9): one `catalog` slot (`radio.rs:225`) is rendered by
   `Place::Country | Tag` (`radio.rs:792`), never cleared on `enter`/`back`, `set_catalog`
   never called empty, and `radio_data.rs:402-404` re-injects a cached list on every build.
3. **Loading partly reproduces** (#21, S10): `⟳ loading…` exists in the footer
   (`mod.rs:1905-1914`) but is never cleared, the list area shows stale rows or
   `no stations here`, and a live stream publishes nothing until Ok/Err because
   `PlayState` has no connecting state (`mod.rs:277-281`).
4. **The library cannot reproduce it** — each level owns its vector (`mod.rs:3332-3341`,
   `2856-2871`), `descend` writes the child into that level's slot and ascent reads no new
   data. That mechanism is what the radio lacks, and it is the model for T6.
5. **Sections have no structure to separate with**: `Row` is `{ label, fact, kind }`, there
   is no header or separator variant, and the renderer draws every row identically
   (`radio.rs:1044-1065`). T8 needs that concept, not just a reorder.

**L5 — the four findings are implemented (T5–T8).**

- **T5 (#23, S8)**: `tab` is one round trip. `App::tab_origin` remembers the screen you
  left; the queue returns you to it — `Sources`, the library at its level, or the radio view
  **still alive** at the place you were inside. `RadioOutcome::ToQueue` no longer throws the
  radio away, so the round trip keeps its loaded list; the radio's keys and its drawing are
  both gated on the queue being off screen, and a round trip through `Sources` persists no
  tab (the sources screen is not a persisted tab).
- **T6 (#20, S9)**: the catalog belongs to the place that asked for it
  (`catalog_place`), rendered only in that exact place; every move (`enter`,
  `open_countries`, the new `open_place`) drops the previous list, and the wiring re-injects
  a cached answer only into its own place. `Place::Top` no longer renders station rows at
  all: **the top-level "loaded catalog" section is gone**, because Top is a category screen.
- **T7 (#21, S10)**: `RadioView` carries the waiting fact (`pending`), an empty place
  renders `⟳ loading…` instead of `no stations here` while its own request is in flight, the
  footer's `⟳ loading…` is cleared when the answer lands (it used to stick), and `enter` on
  a station writes `velo: opening <name>…` before the player answers. No audio change and no
  `PlayState` variant: the status line was enough.
- **T8 (#22, S5 as revised)**: two display-only row kinds (`Rule`, `Header`), favourites at
  the **end** of the top screen behind a dotted rule and a `favourites` header; navigation
  skips the delimiters, `selection()` is `None` on them, `enter`/`f`/`a` do nothing, and the
  row numbers count only selectable rows.

**Evidence**: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`
— **36 suites, 668 passed, 0 failed, 3 pre-existing ignored** (651 after T5, 657 after T6,
665 after T7, 668 after T8). Each unit was written test-first with its RED observed, and the
full suite was re-run by the parent after the four landed.

**Slice decision**: T5–T8 land as **one code commit**, not four. All four were implemented
sequentially in the same files (`src/ui/mod.rs`, `src/ui/radio.rs`, `tests/ui_radio.rs`), so
a per-unit split would be a hunk-level reconstruction whose intermediate commits do not
compile. The units stay readable through the four test clusters and the issue references in
the commit body; the price is disclosed rather than hidden.

**L6 — two more findings from the human's own review of the pull requests, plus the
`ctrl+r` observation filed as asked.**

> de momento vi que arriba sigue diciendo Library queue en vez de source queue
> otra cosa es que si library no esta definido el path y entro por source a library me
> manda a queue pero lo que debe hacer es mostrarme la ventana para poner el path

Both reproduced from the source, and the second has a cause worth naming: `open_source`
handed the library entry to `saved.view`, and because the `tab` round trip now writes
`queue`, the next launch's `Local library` opened the queue. The fix is not to trust the
recorded tab for a library entry at all. That leaves `[state] view` written but no longer
able to steer a launch — a dead setting by this repository's own rule ("settings that parse
but never arrive are worse than missing ones"), flagged for the human rather than retired
silently mid-review.
