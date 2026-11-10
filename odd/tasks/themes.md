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

**S11 — The factory theme is named `velo`** (issue #54): it is the
built-in, it is what a fresh install selects — the generated `config.toml`
writes `name = "velo"` — and it ships beside the config as
`themes/velo.toml`, created only when missing like every other template.

**S12 — `default` keeps resolving to `velo`'s palette as an invisible
alias**: `[theme] name = "default"` — what every installation before the
factory theme had its name — selects the same palette, and there is no
`default` row in the picker. An existing `themes/default.toml` still wins
over the alias, exactly as any file whose stem matches a factory theme.

**S13 — A second factory theme, `default terminal`, whose seven colours
are all `reset`** (the terminal's own). It ships as `themes/default
terminal.toml` — the file stem is the name, spaces included — and it
appears in the picker like any other factory theme. A theme file must
therefore accept `reset` for every slot, not only for `background` and
`text`; the `[theme]` table of `config.toml` keeps its stricter rule.

**S14 — Nothing else changes, and an existing installation is never
rewritten**: the eleven well-known themes, the precedence (built-in →
file → `[theme]` keys, defaults-are-documentation), the picker's live
preview and its `esc`/`enter` contract, the settings card and every other
setting stay as they are; a `themes/default.toml` or a `config.toml`
already on disk is left exactly as it is, and the alias is what keeps
`name = "default"` working.

**S15 — The factory palette carries a real background and text** (issue
#57): `background = #141c28`, `text = #F4F0E2`, `dim = #767676`,
`accent = #F4BAA3`, `secondary = #A6C8E2`, `border = #252939`,
`ink = #000000`. It is the built-in `velo` AND what `themes/velo.toml`
ships, and the two must agree — the test that pins the shipped file to
the built-in palette is the guard, so they move together. The `border`
slot was `#767676` until L9, which took it off `dim` and gave the frame a
shade of its own; `dim` keeps `#767676`.

**S16 — `[theme] transparent_background` joins the table as an optional
boolean**; the table denies unknown keys, so the value the generated file
writes must parse as part of `[theme]`. `true` forces
the FINAL palette's `background` to the terminal's own — transparent where
the terminal is — for whichever theme is selected, and leaves every other
slot exactly as the theme says; `false` or absent changes nothing. It is
applied AFTER the layering (built-in → theme file → `[theme]` keys), so
it wins over the theme's own background.

**S17 — The picker preview honours the switch too**: with
`transparent_background = true`, moving across themes must not repaint the
background (it stays the terminal's), while the other colours preview as
they do. The launch rule and the preview rule stay the same function,
never two.

**S18 — The generated `config.toml` writes the key at its default, `false`,
uncommented inside `[theme]`, right under `name`** (written as a commented
example until L11): the switch is reachable from the interface, so the file
says what is in force the way every `[ui]` key does instead of leaving a
reader to uncomment a line to find out. The sentence above it says what the
switch does: the terminal's own background shows through when it is `true`,
whatever the theme says.

**S19 — Nothing else changes**: the eleven well-known themes,
`default terminal` (still all `reset`, and now the way to get the old
look), the invisible `default` alias, the precedence, the picker's
contract and every other setting.

**S20 — The settings card gains a row for the switch, immediately below
the `theme` row** (issue #61):
> "la opcion de transparent bg debe estar en la tarjeta de settings justo
> debajo del selector de tema. al cambiarlo debe verse el cambio al momento
> igual que al cambiar de tema"

Its value is shown the way the card shows every other switch (`on`/`off`,
not a colour), it is stepped by `enter` and the arrows like `repeat` or
`spectrum`, and it never opens a picker.

**S21 — Switching applies on the spot** (issue #61): with the switch on,
the background the interface draws with becomes the terminal's own —
transparent where the terminal is — with every other colour exactly as
the theme says; off, the theme's background comes back. No restart, and
the same immediate repaint holds across the theme picker: the card sits
over the interface, so the change is visible as it happens.

**S22 — The value is written to `[theme] transparent_background` only
when it changed**, through the surgical `set_value` the other keys use;
repeated presses that end where they started write nothing. The decision
pinned in the tests: a return to off writes `false` explicitly, the way
the `[ui]` switches write both of their values.

**S23 — The rule stays single** (issue #61): the session's switch is the
same `ThemeKeys` field the launch and the picker's preview resolve
through (`resolved_over`, applying it last), so with it on a move across
the theme picker keeps the background the terminal's own while the
accents preview. No second code path forces the background from the card.

**S24 — Nothing else changes** (issue #61): the other card rows and their
order, the theme picker, the palette of any theme, the eleven built-ins,
`default terminal`, and the generated config's commented line.

## Tasks

| ID | Spec | Route | Task | Commit |
|:--|:--|:--|:--|:--|
| T1 | S1, S2, S5 | worker | `src/config.rs`: `ThemeKeys` learns `name` and `resolved_over` (the layering rule), `themes_dir`, `select_theme` (folder load, `default` first, notices) | |
| T2 | S3 | worker | the notices: missing name, unparsable file, unparsable colour (file + key named), unreadable folder — said once on the status line at startup, the built-in used | |
| T3 | S4 | worker | `src/ui/mod.rs`: the `theme` row, `←`/`→` preview with wrap, `enter` applies and writes `[theme] name`, `esc` reverts an unapplied preview; the card's colour cycles record their overrides so a later apply keeps them | |
| T4 | S1–S4 | parent | wiring in `src/bin/velo.rs`: both launch paths call `select_theme` and `set_theme_library`; README; this logbook | |
| T5 | S6–S10 | worker | issue #37: `src/ui/theme/builtins.rs` (the eleven embedded themes), `src/config.rs` (`select_theme` seeds the catalogue; a file matching a built-in takes its row), `src/ui/mod.rs` (the card keeps one `theme` row, the colour rows and their cycling machinery leave, `enter` on the row opens the picker overlay with live preview on move, restore on `esc`, write-on-change on `enter`), tests, README | |
| T6 | S11–S14 | worker | issue #54: `src/config.rs` (`DEFAULT_THEME_NAME` is `velo`, the `default` alias in `select_theme`, `DEFAULT_TERMINAL_TEMPLATE`, `write_default_theme` writes both factory themes, a theme file accepts `reset` in every slot, the generated config names `velo`), `src/ui/theme/builtins.rs` (`default terminal` joins the catalogue), `src/ui/mod.rs` (the alias keeps the full layering in previews), tests, README | |
| T7 | S15–S19 | worker | issue #57: `src/config.rs` (`ThemeColors::default` is velo's own palette, `ThemeKeys.transparent_background` applied last inside `resolved_over`, `DEFAULT_THEME_TEMPLATE` carries the same palette, the generated file documents the key commented), `src/ui/mod.rs` (the preview resolves through the same `resolved_over`), `src/ui/theme.rs` (doc), tests in `tests/config.rs` and `tests/ui_state.rs`, README, this logbook | |
| T8 | S20–S24 | worker | issue #61: `src/ui/mod.rs` (the `transparent background` row directly below `theme`, flipping the same `ThemeKeys.transparent_background` and re-resolving through `theme_colors_of` — the single rule — with the surgical write), tests in `tests/ui_state.rs`, `tests/ui_render.rs` and `tests/visualizers.rs` (walk positions shifted by the new row), README, this logbook | |

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

**L6** (writer, T6/S11–S14, issue #54). The author: "debe haber un tema
llamado default terminal que es el tema de la terminal". So the factory
theme takes its own name — `velo`, the palette velo has always drawn with,
which the generated config now selects and which ships as
`themes/velo.toml` — and a second factory theme, `default terminal`, joins
the catalogue with `reset` in all seven slots: the terminal's own colours,
shipped as `themes/default terminal.toml` (the stem is the name, spaces
included). A theme file therefore accepts `reset` in every slot; the
`[theme]` table of `config.toml` keeps refusing it outside `background`
and `text`, where it would erase a colour instead of choosing it.
`default` becomes an invisible alias for `velo`: it resolves to the same
palette, gets no picker row, and keeps the old layering — defaults and all
— so an existing `config.toml` with `name = "default"` is never rewritten
and keeps drawing exactly what it drew; a `themes/default.toml` a user
already has wins over the alias, exactly as any file whose stem matches a
factory theme.

**L7** (writer, T7/S15–S19, issue #57). The author: "por defecto va a
tener ese color oscuro de background" and "si está activado … va a
utilizar el color de la terminal … transparente". So the factory palette
`velo` carries its own colours now — `#141c28` background, `#F4F0E2` text,
`#F4BAA3` accent, `#A6C8E2` secondary — and `themes/velo.toml` ships the
same values. The old look still exists twice over: `default terminal`
(all `reset`) is the theme for it, and the new
`[theme] transparent_background = true` forces the resolved `background`
to the terminal's own through the same function the layering ends in —
`ThemeKeys::resolved_over`, which both the launch (`select_theme`) and
the picker's preview (`theme_colors_of`) resolve through — so the switch
wins over the theme's own background and the preview never lies about it.
The generated config documents the key commented out under `name`, with
its one sentence. One consequence of the new defaults worth keeping: a
generated config from before this change carries the old colour values,
which now differ from the built-in defaults, so under a named theme those
old keys are genuine choices that mask the theme — every L3 guarantee
still holds for files written by THIS release; an older generated file
wants its seven colour lines deleted, and nothing is ever rewritten
automatically. One wording consequence: the generated config's colour
paragraph no longer claims `reset` is the default for background and text
— it is only what those two slots are allowed to carry.

**L8** (writer, T8/S20–S24, issue #61). The author: "la opcion de
transparent bg debe estar en la tarjeta de settings justo debajo del
selector de tema. al cambiarlo debe verse el cambio al momento igual que
al cambiar de tema". So the card grew a `transparent background` row
immediately below `theme` — a switch like `shuffle`, `on`/`off`, stepped
by `enter` and the arrows, never a picker. A press flips the same
`ThemeKeys.transparent_background` field the launch resolves through and
re-resolves the palette through `theme_colors_of` — the one function —
so the change is on screen at once and a preview across the theme picker
keeps the background the terminal's own while the accents change. The
surgical `set_value` writes `[theme] transparent_background` per change;
the decision pinned in the tests: a return to off writes `false`
explicitly rather than removing the key, the way the `[ui]` switches
write both of their values. Every other row, the picker and the palettes
are untouched; the tests that walked the card by a counted number of
`j` presses moved down one step.

**L9** (2026-11-10, inline, S15, issue #62). The author: "otro cambio en el
tema velo es el color del borde que a partir de ahora va a ser este: border
= \"#252939\"". The frame stops sharing `dim`'s gray: `ThemeColors::default()`
and the shipped `themes/velo.toml` template carry `#252939`, `dim` stays
`#767676`, and README's palette table says the new value. The same session
closed issue #62 — `ThemeColors::previous_factory()` makes the values velo
itself wrote before issue #57 (`reset` background and text, `#af00af`
accent, `#5f87d7` secondary) documentation too, so a generated config from
that era no longer masks a named theme; the `default` alias keeps its full
layering. An installation whose `themes/velo.toml` already exists keeps its
own border until that file is deleted.

**L10** (2026-11-10, inline, S12, issue #62). The author, asked whether
`name = "velo"` should keep the full layering like the alias `default`:
"no conservar alias default". The alias keeps resolving to velo's palette
and still gets no picker row, but it is no longer the one name that layers
every key: `non_default_choices` now runs on every named path, the picker
preview included, so the values velo itself wrote before issue #57 are
documentation under `default` too. The one path that layers everything is a
config with no `[theme] name`, unchanged. Tests: `the_default_alias_drops_
the_old_generated_values_too` and `without_a_name_every_key_layers_exactly_
as_before` replace the alias-keeps-the-old-look test.

**L11** (2026-11-10, inline, S16/S18). The author: "esta linea asta comentada
/Users/alvaldes/.config/velo/config.toml#L49 … si es cierto que el default de
transparent_background es false pero no importa que sea el defualt debe estar
sin comentar porque desde la tui se puede cambiar a true". So the generated
`config.toml` writes `transparent_background = false` uncommented under
`name`, the way every `[ui]` key is written, with the same sentence above it
plus where the card flips it (`ctrl+p`). Verified: the written line loads as
`Some(false)` and leaves the theme's own background alone — the switch is
off, and only `true` forces the terminal's. Tests renamed to
`the_generated_config_writes_transparent_background_at_its_default`.
