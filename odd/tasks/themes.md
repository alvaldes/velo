# velo — themes as named files

Issue: https://github.com/alvaldes/velo/issues/10 — themes as files: named
ones the user drops in a folder, the built-in one always there, and a picker
that previews the focused theme before applying it. A theme should be a thing
you can hand to another person, not a block of a config file.

## Specs

**S1 — A theme is a TOML file in the config directory's `themes/` folder.**
> Themes en `themes/*.toml`; `[theme]` queda como override

`~/.config/velo/themes/<name>.toml`, or `$XDG_CONFIG_HOME/velo/themes/`, with
the same seven colour keys the `[theme]` table already has. The built-in
theme always exists and is named `default`; the folder may not exist, which
is not an error.

**S2 — The selected name lives in `[theme] name = "<name>"` in `config.toml`.**
The other `[theme]` keys keep working exactly as they do today and WIN over
the selected theme, so a one-off change still does not require copying a
whole theme file: built-in → selected theme file → the `[theme]` keys in
`config.toml`. *(Revised in L3: a key whose value equals the built-in
default — as every key of the generated config's does — is documentation,
not a choice, and does not mask a selected theme; a key that differs keeps
winning. Selecting `default`, or no name at all, keeps the old layering.)*

**S3 — A name that is not on disk falls back to the built-in and says so
once** (status line at startup, never a crash and never a silent dark
screen). A theme file that does not parse or carries a colour that does not
parse is reported naming the file and the key, and the built-in is used; the
app still starts.

**S4 — The settings card (`o`) gains a `theme` row** that lists the loaded
names, PREVIEWS the focused theme before applying it, and applies on `enter`
by writing `[theme] name` to the config file. `esc` leaves the previous
theme applied, exactly as it does for the other rows.

**S5 — With no `themes/` folder and no `[theme]` keys, the app looks exactly
as it does today** (the built-in is today's default theme).

## Slices

Sliced for issue #37 (S6–S10 below): the card row, the catalogue and the
picker with its live preview and restore land in one pass; the picker's
key contract and the freeze of everything else ride along with them. If a
pass cannot carry all of it, S6–S9 (the row, the catalogue, the live
preview, the restore) land first and S10 is reported as remaining — never
a half-changed tree.

**S6 — The settings card stops listing the seven colours.**
> "los elementos internos de los temas (colores) no deben salir en settings
> en la TUI sino que lo que debe salir es el tema actual y al hacer enter
> debe abrir una ventana que muestre la lista de temas"

The card keeps ONE `theme` row whose value is the current theme's name;
the colour rows and their preset-cycling machinery are gone. Editing a
colour by hand in `config.toml` keeps working exactly as it does — the
`[theme]` table is untouched.

**S7 — `enter` on the `theme` row opens a picker overlay**, shaped like
the radio's order card: one row per theme, `default` first, then the
built-in catalogue sorted by name, then every `themes/*.toml` that does
not carry a built-in's name. A file whose stem matches a built-in wins
and there is one row, not two.

**S8 — The built-in catalogue is eleven themes** embedded in the binary,
each documented with its source scheme and mapped onto velo's seven
slots: `dracula`, `nord`, `solarized-dark`, `solarized-light`,
`gruvbox-dark`, `catppuccin-mocha`, `catppuccin-latte`, `tokyo-night`,
`one-dark`, `one-light`, `monokai`. `default` stays exactly the palette
velo has always drawn with, and no theme pairs an unreadable foreground
with its background.

**S9 — Moving the cursor in the picker APPLIES that theme immediately**:
the whole interface repaints, because the preview IS the interface. `esc`
restores exactly the palette in force when the picker opened, `[theme]`
overrides included; `enter` keeps the theme and writes `[theme] name` to
the config — the `s`/shuffle write contract: only when it changed.

**S10 — The picker is its own overlay, and nothing else changes**: `j`/`k`
and the arrows move and wrap at the ends like every navigation list,
`enter` applies and closes, `esc` closes and restores, every other key
waits while it is open — and playback, the queue, the radio, the lists
and the other settings rows are untouched.

## Tasks

| ID | Spec | Route | Task | Commit |
|:--|:--|:--|:--|:--|
| T1 | S1, S2, S5 | worker | `src/config.rs`: `ThemeKeys` learns `name` and `resolved_over` (the layering rule), `themes_dir`, `select_theme` (folder load, `default` first, notices) | |
| T2 | S3 | worker | the notices: missing name, unparsable file, unparsable colour (file + key named), unreadable folder — said once on the status line at startup, the built-in used | |
| T3 | S4 | worker | `src/ui/mod.rs`: the `theme` row, `←`/`→` preview with wrap, `enter` applies and writes `[theme] name`, `esc` reverts an unapplied preview; the card's colour cycles record their overrides so a later apply keeps them | |
| T4 | S1–S4 | parent | wiring in `src/bin/velo.rs`: both launch paths call `select_theme` and `set_theme_library`; README; this logbook | |
| T5 | S6–S10 | worker | issue #37: `src/ui/theme/builtins.rs` (the eleven embedded themes), `src/config.rs` (`select_theme` seeds the catalogue; a file matching a built-in takes its row), `src/ui/mod.rs` (the card keeps one `theme` row, the colour rows and their cycling machinery leave, `enter` on the row opens the picker overlay with live preview on move, restore on `esc`, write-on-change on `enter`), tests, README | |

## Log

**L1** — user, the issue: "Themes as files: named ones the user drops in a
folder, the built-in one always there, and a picker that previews the focused
theme before applying it. A theme should be a thing you can hand to another
person, not a block of a config file."

**L2** — user, the shape decided: "Themes en `themes/*.toml`; `[theme]` queda
como override" — the selected file layers over the built-in, the `[theme]`
colour keys of `config.toml` win over both.

**L3** — the lane's review flagged the generated config: `default_config_text`
writes all seven colour keys at the built-in defaults, and those keys won,
so for anyone with a generated config a theme file was completely masked —
the feature looked broken in exactly the common case. Decision: under a
**named theme other than `default`**, a `[theme]` colour key whose value
equals the built-in default is not a choice and does not mask the theme; a
key that differs keeps winning (S2 as revised). The generated file keeps
writing the seven keys — they document what exists; changing that is out of
scope. Selecting `default`, or no name at all, keeps today's layering,
defaults and all. `ThemeKeys::non_default_choices` is the rule; the three
RED tests are `the_generated_file_does_not_mask_a_named_theme_with_its_default_keys`,
`a_deliberately_different_key_still_wins_over_a_named_theme` and
`selecting_default_or_no_name_keeps_todays_layering`.
One consequence: a user's `[theme]` accent deliberately written as `#af00af`
— the built-in default value — is indistinguishable from the generated
default and no longer masks a theme under a named selection. That is the
accepted trade: the value is the default, whatever wrote it.

**L5** (2026-11-09, writer, T5/S6–S10). The author of issue #37: "los
elementos internos de los temas (colores) no deben salir en settings en la
TUI sino que lo que debe salir es el tema actual y al hacer enter debe
abrir una ventana que muestre la lista de temas". So the seven colour rows
left the card with their preset-cycling machinery (`ColorRole`,
`COLOR_PRESETS`, the card's own preview keys), the `theme` row shows the
applied name, and its `enter` opens a picker shaped like the radio's order
row card. Moving the cursor applies the focused theme at once; `esc`
restores the opening palette (snapshot, overrides included); `enter`
writes `[theme] name` only when the theme changed. The eleven built-ins
live in `src/ui/theme/builtins.rs`, each with its source scheme in a doc
comment; `select_theme` seeds them after `default`, and a
`themes/<built-in>.toml` file replaces that built-in's row instead of
shadowing it — which also retires the special case `default.toml` used to
have. One layering consequence worth keeping: a `[theme]` colour override
rides on top of every preview, so a preview's changed colours are read
through the keys that are not overridden.

**L4** — **the folder ships `themes/default.toml`.** The human: "si themes/ se tiene
que crear con el tema de velo que es el que esta ahora mismo y es el que se carga por
default". The file is velo's own palette, so it documents the format without changing
how anything looks, and a test pins it to `ThemeColors::default()` so the shipped copy
cannot drift from the palette it claims to be. Created like every other config-side
template — only when missing, never over an edited one — and an edited `default.toml`
still replaces the built-in in the picker, which is the rule above.

