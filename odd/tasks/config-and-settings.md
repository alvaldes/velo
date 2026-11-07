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
| T1 | The reader: `[state]`, `[theme]`, `[ui]` — types, defaults, validation, legacy top-level `volume`, `Settings` carrying all of it, `default_config_text` | S1, S2, S3, S4, S5 | inline | pending |
| T2 | Theme at runtime: seven hex roles to `Color`, derived `accent_bright`, every draw site, README palette | S4 | inline | pending |
| T3 | Visibility: the four flags wired into the layout | S5 | inline | pending |
| T4 | Writer and restore: `config::set_value` per key, save-on-change, and the tab/view/node/volume/shuffle/repeat coming back at launch | S2, S3 | inline | pending |
| T5 | The settings card: `o`, rows, navigation, live apply, `HELP_ROWS`, README keys | S6, S7 | inline | pending |

## Log

**L1** (2026-11-07, user, verbatim):

> `.config/velo/config.toml` debe de incluir toda la config de velo. Esto incluye estado como del volumen pero tambien el shuffle, el repeat y demas para la persistencia. Ademas cuestiones de UI como los colores y el show/hide de los elementos como es el caso del cover. Esto de la UI tambien debe de poder configurarse desde la app con una ventana como la de ayuda apra configurar todo esto.

**L2** (2026-11-07, user, the four product decisions, verbatim labels):

> Colores: Hex RGB: "#ff00aa"
> Ocultar: Cover art, Espectro / visualizador, Barra de hints inferior, Barra superior (tabs V E L O)
> Persistencia: + vista y dónde estabas
> Ventana: Todo lo que hay en config.toml

**L3** (2026-11-07, parent, engineering decisions taken without a question): per-key in-place writing that preserves the user's comments (the `set_library` shape, not a whole-file `toml` re-serialisation); write only on a real change; a failed write is a status message, never fatal; the queue, the playing track and the cursor row stay out of scope (S3). `replay_gain` and `bit_perfect` keep their top-level place because they are listening settings, not session state. Baseline before T1: `cargo test` 17 suites green (240 tests), `clippy` and `fmt` clean.
