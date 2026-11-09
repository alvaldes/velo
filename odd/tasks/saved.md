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

**S6 — The store's `[[playlist]]` is filled: a name and its tracks, in
order.**
A name is unique, compared verbatim — no trimming, no case folding, the same
identity rule a favourite's path follows: saving or renaming onto a name that
is already saved is refused, never a silent merge, never a duplicate. The
store's writes stay atomic and best effort, like every other write there. The
verbs: save a new playlist, append one track to its end, rename (keeping the
tracks and the file's row order), delete, and move one track by swapping it
with its neighbour. Appending to an unknown name appends nothing — the store
never invents a playlist.

**S7 — `Playlists` is a source, LAST.**
`Sources` gains a `Playlists` row after `Favourites`, so `j` from the top
still lands on `Radio` and `Favourites` keeps its place. Opening it lists the
saved names, one row each with its track count; `enter` on a name opens that
playlist as a track list — playable from the row, `a` queues, the same list
conventions as `Favourites` — and `esc` walks back to the names list, then to
`Sources`. An empty store opens an empty list that says so. A playlist's rows
are the store's paths, read as the store stands (not a snapshot), so an edit
is on screen the frame after it is saved; a track the store also holds as a
favourite keeps its `★` there.

**S8 — Creating: `S` saves the queue, `S` appends a track.**
`S` in the queue view saves the queue as a new playlist, asking for a name
with a prompt in the style of the seek/library prompts (a card, `enter save`,
`esc cancel`). An empty queue is refused on the status line, and so is a
queue that holds a station — a playlist's tracks are paths, and a station has
none. A refused name (taken, or empty) keeps the prompt open with the reason
under what was typed, the way a refused seek does. `S` on a library track row
opens a small overlay listing the saved names; `enter` appends the row's path
— exactly the path the row carries — to the chosen one and `esc` cancels. No
saved names, nothing to choose from: one status line says so.

**S9 — `J`/`K` reorder inside a playlist; `d` deletes; `e` renames.**
Inside a playlist's track list, `J`/`K` move the selected track down/up and
the move is written to the file at once — the queue view already reorders
with those keys, and there is no playing entry to protect here, only the
list's ends. In the names list, `d` deletes the playlist under the cursor and
says so in the status line, and `e` renames it with the same prompt, opened
with the name as it stands; `esc` cancels. Nothing else changes: `f`, `a`,
`S` and the rest keep their meaning elsewhere — `f` does nothing on a
playlist row, `a` still queues it.

## Tasks

| ID | Spec | Route | Task | Commit |
|:--|:--|:--|:--|:--|
| T1 | S1 | parent | `src/saved.rs`: both tables, `path_beside`, lenient load, atomic save, toggle by path | |
| T2 | S2, S3 | parent | `App::set_config_file` loads the store, `f` toggles a library track row and saves at once, `★` before the title in every library list | |
| T3 | S4 | parent | `Favourites` as the last source, its `Position` and `Level`, playing, queueing, `esc`, the empty list, and the docs | |
| T4 | S6–S9, issue #4 | `feat/playlists` | the store's playlist verbs with their tests, `Playlists` as the last source with its two lists, the write verbs (`S` save, `S` append, `J`/`K`, `d`, `e`), and the docs |

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

**L4** — issue #4's own words: "Named playlists that outlive the process —
created, renamed, added to, reordered, and played like a queue — stored in
their own file under `~/.config/velo` rather than mixed into `config.toml`."
The file was already decided: `library.toml`, one file for marked tracks and
playlists (L2), and its `[[playlist]]` table has existed since that slice —
filling it needed no format change.
## Slices

One commit for this issue, on purpose: the store verbs, the `Playlists` screen and
the write keys share the same match arms in `src/ui/mod.rs`, so any split leaves a
tree that does not compile or a screen without the verbs that justify it. The slice
is large; the review reads it as store → screen → keys → tests, in that order.

