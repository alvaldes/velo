# velo — saved tracks and the store they live in

Issue: https://github.com/alvaldes/velo/issues/7 — `f` marks a track, marked
tracks are playable as a list like any other, and the marker survives a
restart. One slice: the durable store (`S1`), then the marking, the marker
and the list on top of it (`S2`–`S4`).

## Specs

**S1 — One file: `library.toml` beside `config.toml`.**
> Uno solo: `library.toml` con `[playlist]` and `[favorite]`.

The human decided the storage: one file for both answers, so saved playlists
(#4) never need a format change. The file lives beside `config.toml` — the
same place `radio.toml` and `history.toml` live, reusing the config-file path
the shell already resolves — and holds both tables from the start:

```toml
[[favorite]]
path = "/music/Tide/01 - Nightfall.flac"
artist = "Tide"
album = "Harbor"
title = "Nightfall"

[[playlist]]
name = "Noche"
track = ["/music/a.flac", "/music/b.flac"]
```

Writing is atomic in the shape `radio/store.rs::save` uses: a temporary file
in the same directory, flushed to disk, renamed over the target, the
temporary removed on failure. Loading is lenient: a missing file is an empty
store, unknown keys are tolerated so a newer velo never has its file wiped,
and a file that does not parse is reported where a reader asks for it and
ignored by the interface.

**S2 — `f` marks a track, and again unmarks it.**
The identity is the row's path exactly — a `subsonic://` path stays itself —
and marking twice never duplicates it. The favourite keeps `artist`, `album`
and `title` copied from the row when it has them: there is no lookup by path
in the index, so the song must still be nameable from the file alone.
Marking writes the file immediately (atomic, best effort: a store that cannot
be written is lost, not fatal — the same courtesy rule `history.rs` follows).
The state is loaded with the config, next to the history (`App::set_config_file`),
so it survives a restart.

**S3 — A marked track shows `★` before its title.**
In every library list — tracks, all tracks, an album, a genre — the way the
radio view already marks its favourites. The marker is state, not copy: it
comes from the store, so it is there on the next launch.

**S4 — `Favourites` is a source, LAST.**
`Sources` gains a `Favourites` row after `Recently played`, so `j` from the
top still lands on `Radio`. Opening it lists the marked tracks playable and
queueable exactly like any other track list (`enter` plays from the row, `a`
queues, `j`/`k` follow the conventions, `esc` returns to `Sources`), each row
showing its `★`. An empty store opens an empty list that says so, not an
error. The `Position::Recent` shape from `odd/tasks/history.md` is the model:
the list is its own `Position` variant holding the store's entries, not a
synthesized `tracks` snapshot.

**S5 — Nothing else changes.**
Library browsing, the radio view, the queue, the history and the settings
behave exactly as they do today. `f` keeps meaning "favourite a station /
pin a place" inside the radio view.

## Tasks

| ID | Spec | Route | Task | Commit |
|:--|:--|:--|:--|:--|
| T1 | S1 | parent | `src/saved.rs`: both tables, `path_beside`, lenient load, atomic save, toggle by path | |
| T2 | S2, S3 | parent | `App::set_config_file` loads the store, `f` toggles a library track row and saves at once, `★` before the title in every library list | |
| T3 | S4 | parent | `Favourites` as the last source, its `Position` and `Level`, playing, queueing, `esc`, the empty list, and the docs | |

## Log

**L1** — the issue's own words: "`f` marks a track, marked tracks are
playable as a list like any other, and the marker survives a restart. Where
that list lives is the same decision as saved playlists — one answer for
both, not two."

**L2** — the human's decision, asked to choose between a favourites file and
a shared one: "Uno solo: `library.toml` con `[playlist]` and `[favorite]`" —
one file, both tables from the start.

**L3** — scope addition, two decisions on when the file exists:
`library.toml` must EXIST after a launch — created at startup, not on first
use, so `config.toml`, `radio.toml` and `library.toml` all exist after the
first launch while `history.toml` does not (it appears when the first track
begins) — and the launch's write is self-healing create-only: the template is
written only when the file is MISSING, and an existing file is never touched,
never rewritten, never reordered. The template is all `#` comment lines (a
short sentence about the file and the whole-file rewrite, then one commented
`[[favorite]]` and one commented `[[playlist]]` example), so an untouched
template parses as an empty store.
