# velo — recently played

Issue: https://github.com/alvaldes/velo/issues/5 — a history file written when a
track begins, readable from the shell, and a `Recently played` screen that shows
it. Two reviewable slices: the file and its three commands (`T1`), then the
screen (`T2`).

## Specs

**S1 — A history file is written when a track begins.**
> A history file, written when a track begins, newest first, one entry per path
> (playing it again moves it up instead of duplicating it), capped at a few
> hundred.

The file lives beside `config.toml` (`history.toml`, the way `radio.toml` does)
and holds the path the queue carries — `subsonic://…` stays itself, never the
cache file it resolves to. Writing is best effort: a history that cannot be
written never interrupts playback, and a corrupt file never blocks a launch.
A seek is not a new beginning: rebuilding the player on the same entry must not
re-stamp it. `Entry::Station` has no path and is not history. The one-shot
`--play` mode writes nothing: the file is written by the interface.

**S2 — The shell reads it without the TUI.**
> `velo history`, `velo history --json` for a script, and `velo history clear`.

`velo history` prints one line per entry, newest first, and exits 0 — also when
there is no file. `--json` prints the same order as a JSON array. `clear` empties
the file and exits 0 when there is nothing to clear. None of the three needs an
interactive terminal, and `history` is never treated as a file to play.

**S3 — Each entry records when and, when known, what.**
`played_at` is unix seconds. `artist`, `album` and `title` are filled from the
library row the queue was built from, and are absent for a path the index cannot
describe. The dedupe key is the path alone (S1): the same file played under two
spellings stays two entries.

**S4 — `Recently played` is a source beside `Local library` and `Radio`.**
It opens a list of the history entries, newest first, and its rows play, queue
with `a` and follow the list conventions the rest of the app already has. The
rows follow the library's row convention: the title, with `artist · album` as
the note, and the file name with its folder when the index never knew it. The
entry that is playing is marked like every other playing row.
An empty history says so and opens an empty list, not an error.

**S5 — The file and the screen agree.**
The screen reads the same file the shell prints, and a track played on the
screen is the entry at the top of the file. A history longer than the cap is
trimmed on write, oldest first.

**S6 — A station that begins playing is recorded too (issue #33).**
> en el recent played no salen los radios pero eso porque, si se puede no? y
> estaria muy bien que saliera ahi

A station beginning records one entry like a track does: its stream URL is
the identity — two writings of one station (`https://Stream.example:443/x`
and `https://stream.example/x`) are one entry, the radio store's
`station_key` decides — and its own name is the title, with no artist and
no album. The file says `kind = "station"` where a track says
`kind = "file"`; an entry written before the key existed loads as a track.
The `Recently played` screen lists the station beside the tracks, in the
same newest-first order, with the note `radio`; `enter` on it plays the
live stream through the same route the radio view uses, and `a` queues it
without starting anything.

## Slices

One commit per independently reviewable step, each green on its own:

1. the history file and its format (`src/history.rs`), nothing wired into it;
2. the interface recording every beginning into it, nothing to read it with;
3. the shell reading it: `velo history`, `--json`, `clear`;
4. the `Recently played` screen, which reads the same file;
5. stations in the history and on the screen (issue #33).

## Tasks

| ID | Spec | Route | Task | Commit |
|:--|:--|:--|:--|:--|
| T1 | S1, S2, S3, S5 | parent | `src/history.rs` (TOML, cap, dedupe, atomic save), the `App` write hook, and the `history` subcommand: plain, `--json`, `clear` | `29caaa5`, `dea7f33`, `03ca99a` |
| T2 | S4, S5 | parent | `Recently played` in `Sources`: the level, its rows, playing marker, and the docs | `1624e20` |
| T3 | S6 | parent | the file learns the station `kind` and dedupes on the entry's identity, the tick records a station beginning, the screen lists stations beside the tracks, and the docs | |

## Log

**L1** — user, the issue: "A history file, written when a track begins, newest
first, one entry per path (playing it again moves it up instead of duplicating
it), capped at a few hundred. And a way to read it without opening the TUI:
`velo history`, `velo history --json` for a script, and `velo history clear`."

**L2** — user, asked whether the TUI gets a screen the issue body never names:
"Historial + una vista en la TUI" — so the deliverable is the file, the three
commands, and a `Recently played` source (S4), in that order.

**L3** — T2, three decisions worth keeping. `ascend()` out of `Position::Recent`
restores the top of the current `BrowseBy` view and re-reads it, then returns
before `remember_node()`: the recent list replaced the `tracks` snapshot, so
there is no parent list to keep, and the saved node was never this list's to
walk out of. The synthesized rows carry no invented title — the row's name comes
from the file, and `history_tick` turns an empty field into an absent one — so
replaying an entry from the screen cannot write back metadata the file did not
have. The playing marker stays untested by construction: `playing_path()` needs
a real player and no TUI test has one.

**L4** — T3, the human's words, verbatim: "en el recent played no salen los
radios pero eso porque, si se puede no? y estaria muy bien que saliera ahi"
— so a station beginning is recorded like a track's, and `Recently played`
lists stations beside the tracks.
