# Library refresh: reopen the prompt, read-only indexes, background indexing

Objective: the library can be set again at any time, an old read-only index still opens, and the index refreshes in the background — on launch and on a key — with an indicator.
Branch: `main` (each delivery ends with `scripts/release.sh`). Runner: `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --all -- --check`.

## Specs

**S1 — Reopen the library prompt.**
> "Si cerrás la ventana con Esc, no hay tecla para volver a abrirla."

Decision: `L` opens the prompt at any time (also to change the library); an accepted path is saved and indexed as on first run.

**S2 — An old read-only index opens.**
> "Un índice viejo en un archivo de solo lectura ya no abre. La actualización del índice necesita escribir en él."

**S3 — Refresh on a key, on every launch, with an indicator.**
> "es importante que se pueda refrescar el indice con una key ademas de que siempre que se abara la app haga reload solo y un indicador en la interfaz que esta reaload"

Decision: `u` refreshes; launch refreshes by itself; both run in the background while the interface stays usable; the title bar shows `⟳ indexing…` while it runs; the lists reload when it ends and the status line reports the counts.

## Tasks

| ID | Task | Specs | Route | Commit |
|----|------|-------|-------|--------|
| T1 | `L` opens the library prompt at any time | S1 | inline | `51415bd` |
| T2 | A version-0 index that cannot be written opens without the migration (genres unknown) instead of failing | S2 | delegated writer (parallel with T3, disjoint surfaces) | `1d57b16` |
| T3 | Background indexing on launch and on `u`, title-bar indicator, reload on completion. Risk: item 4 | S3 | delegated writer (parallel with T2) + independent verifier | `1832e9e` (verifier pending) |

## Log

**L1** (2026-11-07, user): "trabajemos en esto: - Si cerrás la ventana con Esc, no hay tecla para volver a abrirla. - Un índice viejo en un archivo de solo lectura ya no abre. La actualización del índice necesita escribir en él. y es importante que se pueda refrescar el indice con una key ademas de que siempre que se abara la app haga reload solo y un indicador en la interfaz que esta reaload"

**L2** Today the launch index runs synchronously inside the terminal session (`index_root` in `src/bin/velo.rs`): the interface shows "indexing …" and is frozen until it ends. S3 needs it on a thread with its own `Library` connection, reporting back to the interface.

**L3** T1 (`51415bd`): RED observed (compile). `App` keeps the config file (`set_config_file`, set by `browse_interface`), and `L` opens the prompt with it whenever the search and the prompt are closed and the list is not command-line files. The help card is full at 80×24, so the `L` row replaced the passive "no library set" row instead of adding one. 17 suites, clippy, fmt green.

**L4** T2 (writer): RED observed — `Library::open_at` on a chmod-0444 velo 0.5.0 index (WAL, version 0, no `genre`) failed with `SqliteFailure(ReadOnly) "attempt to write a readonly database"` from `migrate`. Making only the file read-only was enough; the folder can stay writable. A trap found on the way: any read of a read-only WAL file in a writable folder (also velo 0.5.0 and today's version-1 path) creates `index.db-wal`/`-shm` with the file's 0444 mode and leaves them, so writes keep failing after the file is made writable again. Fix: when the opened connection reports `is_readonly`, `open_at` reopens the file as `file:…?mode=ro&immutable=1` (path percent-encoded), creates nothing, skips the pragmas and the migration, and records whether `tracks` has `genre`; without it the genre reads select `NULL`, so every track is under `(no genre)`. `index` on such a library fails with the read-only error, as in 0.5.0. Writable files take the path they took before. GREEN: 4 new tests in `tests/library.rs` (read-only v0 with read-only folder and `#?%` in its name browses artists/albums/tracks/genres and writes nothing; read-only-then-writable migrates on the next open and leaves no `-wal`/`-shm`; read-only v1 keeps its genres; a non-database file still fails); library suite 13/13, clippy and fmt green. Gaps: a non-UTF-8 path keeps the old path; leftover 0444 `-wal`/`-shm` already created by 0.5.0/0.6.0 still block writes until removed; `immutable` assumes nothing writes the read-only file while velo reads it.

**L5** T3 (writer): RED observed — the 10 new tests did not compile (`Action::Reindex`, `App::set_library_root`/`index_started`/`index_finished`/`indexing`, `velo::ui::indexing` missing). Design: `src/ui/indexing.rs` holds `Indexer::spawn(root, open)`, a thread that opens its OWN `Library` (the binary passes `Library::open`, tests `Library::open_at`) and sends one `Result<IndexReport, String>` through an mpsc channel; `poll` is non-blocking (a dead thread reads as a failure, never as running forever), `wait` blocks (tests). `App` stays pure: `u` answers `Action::Reindex` (browse only; refuses with `already indexing <root>` while one runs, and `no library set — press L to choose one` without a root); `apply(Reindex)` hands the root over through the existing `take_library_root`, so launch, `u` and a root accepted in the prompt share one path. The event loop polls the indexer every turn (the 100 ms idle cadence is short enough; the indicator is static, so `cadence` is unchanged) and starts a pending root only when no index runs — a root chosen in the prompt during an index waits for it. The title bar shows `⟳ indexing…` in `ACCENT` after the tabs while it runs. `index_finished` writes the counts exactly as `index_root` did, then `reload`, which now re-reads the artists, the current level's list and the list `esc` returns to, keeps view/level, keeps the selected row by identity (artist name, artist+album, track path, genre), and returns to the top of the view when an artist/album/genre is gone; a read error keeps the last snapshot and goes to the status line. Locking: the index is WAL and `index` commits one transaction, so the UI connection keeps reading the last committed snapshot and never sees a half-written list; lists are re-read only after the outcome arrives; rusqlite's default 5 s busy timeout covers the brief locks of `open_at`. Quit drops the indexer (detached thread); an uncommitted transaction is rolled back by SQLite. Help: the `L` row became `L / u — set the library / refresh the index` (no new row); README keys table and quick start updated. GREEN: `tests/ui_state.rs` +9 (`u` → `Reindex` and the root; `u` while indexing; `u` without a library; prompt root is the one `u` refreshes; launch index on its own connection reloads the lists with the counts; finish keeps AllTracks view and the selected track with a new file listed; finish refreshes an open album and the albums above it; a vanished album returns to the top; failed index — also an unopenable index — reports `velo: indexing failed: …`), `tests/ui_render.rs` +2 (indicator in accent while indexing, gone after; help names the refresh). Full `cargo test` green, fmt green; clippy green on every target except `tests/library.rs:663` (`assert_is_empty`, T2's surface). Gaps: the TUI wiring in `src/bin/velo.rs` (launch starts the index, the loop polls it, quit mid-index) needs a terminal and is untested; an index that finishes while the search is open leaves its pool as it was until the search reopens.

**L6** Parent: T2 and T3 ran in parallel with disjoint surfaces; each diff checked against its surface; seam check on the combined tree (fmt, clippy, all 17 suites) green; T2 committed as `1d57b16` (code and tests only), T3 as `1832e9e`. T3 changes shared code (`App::reload` now refreshes the parent list and keeps the selection by identity) and adds a thread: independent verifier next (item 4).
