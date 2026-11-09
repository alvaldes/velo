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

## Tasks

| ID | Spec | Route | Task | Commit |
|:--|:--|:--|:--|:--|
| T1 | S1, S2, S5 | worker | `src/config.rs`: `ThemeKeys` learns `name` and `resolved_over` (the layering rule), `themes_dir`, `select_theme` (folder load, `default` first, notices) | |
| T2 | S3 | worker | the notices: missing name, unparsable file, unparsable colour (file + key named), unreadable folder — said once on the status line at startup, the built-in used | |
| T3 | S4 | worker | `src/ui/mod.rs`: the `theme` row, `←`/`→` preview with wrap, `enter` applies and writes `[theme] name`, `esc` reverts an unapplied preview; the card's colour cycles record their overrides so a later apply keeps them | |
| T4 | S1–S4 | parent | wiring in `src/bin/velo.rs`: both launch paths call `select_theme` and `set_theme_library`; README; this logbook | |

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
