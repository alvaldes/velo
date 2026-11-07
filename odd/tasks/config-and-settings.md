# Every setting in one file, and a settings card in the app

Objective: `~/.config/velo/config.toml` becomes the single home of every velo setting — session state, theme colours and UI visibility — and a card shaped like the help card opens with `o` to change all of it live.
Branch: `feat/config-and-settings` (delivery = merge + `scripts/release.sh`). Runner: `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --all -- --check`.

## Specs

**S1 — One file for everything.**
> "`.config/velo/config.toml` debe de incluir toda la config de velo."

Every setting lives in that file and nowhere else: `library`, `replay_gain`, `bit_perfect` at the top level, then `[state]`, `[theme]` and `[ui]`. No second state file, no `~/.local/state`, no settings table in the library database. A key the file lacks falls back to the documented default; a key it has beats that default and loses only to an explicit CLI flag (`--library`), as today.

**S2 — What "estado" means for persistence.**
> "Esto incluye estado como del volumen pero tambien el shuffle, el repeat y demas para la persistencia."

`[state] volume` (0–100), `shuffle` (bool) and `repeat` (`off`/`all`/`one`).

1. At launch the persisted volume reaches the player the way `volume` does today, and shuffle/repeat reach the queue and the player before the first frame.
2. A change made in the app is written to the file immediately, and only when the value actually changed: pressing `+` at 100 writes nothing.
3. The write is per key, on the user's own file: comments, blank lines, ordering and unrelated keys survive untouched (the shape `set_library` already has).
4. A failed write never kills the app: it becomes the status line message (`velo: <error>`) and the app keeps running.
5. `volume` at the top level of an existing file keeps being read as `[state] volume`, and the first write moves it under `[state]`. `deny_unknown_fields` stays, so a typo is still an error that names the line.

**S3 — Y dónde estabas.**
> Decision: "+ vista y dónde estabas".

`[state] view` = `library` | `queue`, `browse_by` = `artists` | `albums` | `tracks` | `genres`, plus optional `artist`, `album` and `genre` naming the open node. On launch the app comes back to that tab, that library view and that open node (an artist's albums, an artist's album's tracks, or a genre's tracks; a flat view needs no node). A node that the index no longer has falls back to the level above it, silently. The cursor row is not restored — the open node comes back with its first row selected. The queue and the playing track are out of scope.

**S4 — Colours as hex.**
> "Ademas cuestiones de UI como los colores." Decision: hex RGB.

`[theme] background`, `text`, `dim`, `accent`, `secondary`, `border`, `ink`, each `"#rrggbb"`.

1. `"reset"` is accepted for `background` and `text` only, and is their default: the terminal's own colours, as today.
2. `accent_bright` is derived from `accent` (a lighter step) and is not a file key.
3. An invalid value is a config error in today's shape — `path: line N: detail` — never a panic and never a silent fallback.
4. Defaults keep the look velo has today: `background`/`text` `reset`, `dim`/`border` `#767676`, `accent` `#af00af` (ANSI magenta), `secondary` `#5f87d7` (ANSI blue), `ink` `#000000`.
5. Every cell that uses a colour today uses the configured one: card borders and titles, the selection highlight, the volume slider, the spectrum bars, the dim hints.

**S5 — Show and hide.**
> "y el show/hide de los elementos como es el caso del cover." Decision: all four elements.

`[ui] cover_art`, `spectrum`, `hints`, `top_bar`, each a bool, all `true` by default.

1. `cover_art = false` draws no cover and gives its columns to the list; with `true` the existing rules still decide (graphics protocol, `ART_MIN_*`, `cover_slot`).
2. `spectrum = false` gives its rows back to the now-playing block; `hints = false` keeps the status row and drops the key hints; `top_bar = false` drops the tabs/`V E L O` row.
3. Hiding something never leaves a hole, a blank row or a border where it was.

**S6 — The settings card.**
> "Esto de la UI tambien debe de poder configurarse desde la app con una ventana como la de ayuda apra configurar todo esto."

`o` opens a card shaped like the help card: rounded border in the accent colour, title ` settings `, centred, `Clear` underneath, and a fixed size (it does not resize as the cursor moves).

1. `j`/`k` (and `↓`/`↑`) move between rows, `g`/`G` jump to the ends, `enter` and `←`/`→` change the row under the cursor, `esc` closes it.
2. Each row shows its value on the right (`accent  #af00af`); the focused row is marked in the accent colour and the section headers (`sound`, `screen`, `library`) are dim.
3. While the card is open every other key is inert, `q` and `space` included, exactly as the help card. `?` is not a settings key and does nothing there.

**S7 — Everything, live.**
> Decision: "Todo lo que hay en config.toml".

Rows, in order: `sound` — volume, replay gain, repeat, shuffle; `screen` — accent, text, dim, secondary, border, ink, background, cover art, spectrum, hints, top bar; `library` — folder.

1. A change takes effect on the next frame (colour, hidden element, volume, shuffle, repeat) and is written to the file immediately, following S2.
2. The `folder` row shows the configured library and `enter` on it opens the existing library prompt (the one `L` opens), so saving a new folder keeps its single-path behaviour.

## Tasks

| ID | Task | Specs | Route | Commit |
|----|------|-------|-------|--------|
| T1 | The reader: `[state]`, `[theme]`, `[ui]` — types, defaults, validation, legacy top-level `volume`, `Settings` carrying all of it, `default_config_text` | S1, S2, S3, S4, S5 | inline | `c155171` |
| T2 | Theme at runtime: seven hex roles to `Color`, derived `accent_bright`, every draw site, README palette | S4 | delegated writer | `53d6823` |
| T3 | Visibility: the four flags wired into the layout | S5 | delegated writer | `5ec68f9` |
| T4 | The writer: `config::set_value` and `config::remove_value`, per key and per table, preserving the user's file | S2 | delegated writer | `9136a18` |
| T5 | Restore and save: `App::set_session` at launch, and every persisted value written when it changes | S2, S3 | delegated writer | `ac83383` |
| T6 | The settings card: `o`, rows, navigation, live apply, `HELP_ROWS`, README keys | S6, S7 | delegated writer | `9b832d8` |

## Log

**L1** (2026-11-07, user, verbatim):

> `.config/velo/config.toml` debe de incluir toda la config de velo. Esto incluye estado como del volumen pero tambien el shuffle, el repeat y demas para la persistencia. Ademas cuestiones de UI como los colores y el show/hide de los elementos como es el caso del cover. Esto de la UI tambien debe de poder configurarse desde la app con una ventana como la de ayuda apra configurar todo esto.

**L2** (2026-11-07, user, the four product decisions, verbatim labels):

> Colores: Hex RGB: "#ff00aa"
> Ocultar: Cover art, Espectro / visualizador, Barra de hints inferior, Barra superior (tabs V E L O)
> Persistencia: + vista y dónde estabas
> Ventana: Todo lo que hay en config.toml

**L3** (2026-11-07, parent, engineering decisions taken without a question): per-key in-place writing that preserves the user's comments (the `set_library` shape, not a whole-file `toml` re-serialisation); write only on a real change; a failed write is a status message, never fatal; the queue, the playing track and the cursor row stay out of scope (S3). `replay_gain` and `bit_perfect` keep their top-level place because they are listening settings, not session state. Baseline before T1: `cargo test` 17 suites green (240 tests), `clippy` and `fmt` clean.

**L4** (2026-11-07, parent, T1/S1–S5). `Config` is now the serde shape of the file itself: `library`, `bit_perfect`, `replay_gain`, the legacy top-level `volume`, and `state: StateKeys`, `theme: ThemeKeys`, `ui: UiKeys`. `merged` resolves them into `Settings { …, session: Session { shuffle, repeat, view, browse_by, node }, theme: ThemeColors, ui: UiParts }`. `ThemeColor` is `Terminal | Rgb` (17 insertions in `config.rs` per colour error carry a line), `"reset"` is refused for the five roles where it would erase a colour, the three names are checked through serde's `rename_all = "lowercase"` on `Repeat`, `BrowseBy` and `View` (each gained `serde::Deserialize` and a `#[default]`), and a blank or missing node name is no node. `default_config_text` writes the three tables with comments and the documented defaults; the existing test that demanded a comment above every key now accepts the table's own comment for the keys under it. RED: 8 new tests (the full four-table file, the defaults, `[state]` beating the flat `volume`, the out-of-range `[state]` volume naming line 2, five bad colours naming their line, the three unknown variant names plus a blank node, the default file round-tripping) failed to build on the missing types. GREEN: 37 `config` tests, 17 suites, clippy and fmt clean. Slice: 824 inserted lines in one commit — larger than the ~400 guide, accepted whole because the reader, its tests and the file text it writes are one unit; the later units stayed near the guide except T6.

**L5** (2026-11-07, writer, T2/S4). `src/ui/theme.rs` holds `Theme` (the seven roles plus derived `accent_bright`), `From<ThemeColors>` (`Terminal` → `Color::Reset`) and `Default`. `App.theme` with `set_theme(ThemeColors)`; the eight constants are gone and all ~79 draw sites read the palette, the free helpers (`draw_help`, `draw_library_prompt`, `hint_line`, `list_row`, `spectrum_lines`, `bar_color`, `volume_slider`, `progress_bar`, `draw_empty`) take `&Theme` or a `Color`. `lit()` became a closure inside `draw_transport`, its only caller. RED: `the_interface_takes_its_colors_from_the_terminal_palette` failed twice (`Black` vs `Reset`, then `DarkGray` vs `Rgb(118,118,118)`) and `a_configured_palette_reaches_the_cells` did not compile. GREEN: 17 suites. Defaults unchanged, so the 80×24 golden screen is byte-identical.

**L6** (2026-11-07, writer, T3/S5, T4/S2, T5/S2–S3). Visibility: `App.ui` + `set_ui`, one gate each — the top bar's row goes to the content, the footer is the status row alone without hints, a private `App::now_playing_rows` caps the block at the seven-row tier without the spectrum, and `self.ui.cover_art` filters the slot chain before the picker and the artwork, with `explain_hidden_art` silent for a deliberate hide. The decoded-art leg is pinned by a unit test (`Artwork::for_test`) because no integration fixture can hand the App a cover without a live stream. RED: six render tests, plus the unit differential failing with `Some(24)` against the gate removed. GREEN: 17 suites, 282 tests, golden screen unchanged. Writer (T4): `set_value(path, section, key, value)` and `remove_value(path, section, key)` with the bare table name as `section`, `None` for the top level, whole-file texts pinned, `set_library` kept byte-identical (it inserts at the top, which `set_value` does not) and now sharing the key matcher; 11 new tests; the one `unreachable!` became an `io::Error` on parent review. Restore and save (T5): `App.saved: Session` + `saved_volume` are what the file already says, `remember`/`forget` write only real changes through `set_value`/`remove_value` and turn a failure into `velo: …`; hooks in `apply(SetVolume)`, `apply(SetRepeat)`, `toggle_shuffle`, `toggle_view`/`apply(Browse)` and `descend`/`ascend` (removing the keys that stop applying), with the first volume save also dropping the legacy top-level line. `set_session` restores shuffle, repeat, tab, view and node (level-above fallback, silent) and writes nothing; the repeat reaches the first stream because both player-creation sites already pass `self.repeat`. RED: `no method named set_session`, then 7 whole-file tests. GREEN: 17 suites, 300 tests. Parent review: the remaining `unreachable!` replaced by an error, all checks re-run.

**L7** (2026-11-07, writer, T6/S6–S7). `o` opens `App.settings: Option<SettingsCard>` routed after the prompt and before the help, so `q`, `space` and `?` are inert there. `SETTINGS_ROWS` (19 rows: the three headers plus the 16 settings), `SettingsKey`, `ColorRole`, `COLOR_PRESETS` (`#af00af`, `#5f87d7`, `#767676`, `#000000`, `#d70000`, `#00af5f`, `#d78700`, `#00d7d7`, with `reset` ahead of them for background and text). Every change goes through the existing hooks — `apply(SetVolume)`, `apply(SetRepeat)`, `ToggleShuffle`, `set_replay_gain` plus a top-level save, `change_color` rebuilding `ThemeColors` from the new `App.colors` and saving `[theme] <role>`, `toggle_ui` saving `[ui] <key>` — and `folder` closes the card and opens the library prompt. The card keeps one size (clamped, scrolled only to keep the cursor visible), draws last, and `HELP_ROWS` gained `("o", "settings")` (24 → 25, so eight help pins changed their total only). RED: `settings_open`/`settings_focused` did not exist (19 compile errors), then the unquoted colour write. GREEN: 17 suites, 310 tests, clippy and fmt clean; the card was also rendered and eyeballed at 90×30.

**L8** (2026-11-07, parent, close). Every unit re-verified by the parent: `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` (17 suites, 310 tests, 0 failed) green after each commit, and the 80×24 golden screen byte-identical throughout. Slice note: T1 (824) and T6 (918) are above the ~400-line guide because each is one indivisible unit — a schema with its tests, a card with its row table and its render tests; T2–T5 stayed at or below it in spirit. Delivery is the branch: merge and `scripts/release.sh` are the user's decisions. Gaps: no real-terminal pass of the settings card or of a hidden cover; the repeat restored at launch reaches the first stream but a playing track keeps the gain of the mode it started with, and a view switch that leaves a node open keeps `artist`/`album` in the file until the next descend or ascend.
