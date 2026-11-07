# Playback modes, cover placement and library views

Objective: shuffle and repeat, a cover that sits inside the design, and faster ways into the library.
Branch: `main` (each delivery ends with `scripts/release.sh`). Runner: `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --all -- --check`.

## Specs

**S1 — Shuffle and repeat.**
> "agrega shuffle y repeat"

Assumptions: `s` toggles shuffle of the entries after the playing one (off restores their order); `r` cycles repeat off → all → one; both shown beside the transport buttons.

**S2 — Cover placement.**
> "hay que buscarle una mejor posicion a la imagen porque aqui no queda bien, rompe el disenno"

**S3 — Library views.**
> "el library todo esta organizado en una vista donde para reproducir algo hay que hacer minimo 3 click (o movimientos) esto no esta mal pero hay que tambien agrear diferentes vistas como vista por artista o por album. Incluso ver todos los audios ahi mismo o por categoria (lofi, jazz, classic, ...)"

## Tasks

| ID | Task | Specs | Route | Commit |
|----|------|-------|-------|--------|
| T1 | Repeat in the producer: `Repeat`/`RepeatCell`, `next_track`, first-turn flag for the seek offset, stop when every track fails in a row. Risk: item 4 | S1 | inline + independent verifier | — |
| T2 | Shuffle and repeat keys, queue reorder through `edit_queue`, indicators, help, README | S1 | inline | — |
| T3 | Cover inside the now-playing block | S2 | inline | — |
| T4 | Library views: artists, albums, all tracks, genres (genre needs the index to store it) | S3 | inline | — |

## Log

**L1** (2026-11-07, user): "agrega shuffle y repeat"

**L2** (2026-11-07, user, with a screenshot of the cover in the right quarter): "hay que buscarle una mejor posicion a la imagen porque aqui no queda bien, rompe el disenno"

**L3** (2026-11-07, user): "el library todo esta organizado en una vista donde para reproducir algo hay que hacer minimo 3 click (o movimientos) esto no esta mal pero hay que tambien agrear diferentes vistas como vista por artista o por album. Incluso ver todos los audios ahi mismo o por categoria (lofi, jazz, classic, ...)"

**L4** T1: RED observed (compile). The producer's `for index in start_track..` became a loop driven by `next_track(index, len, repeat)`; "first turn" is a flag (Progress.first), not `index == start_track`, so a repeat neither re-applies a seek offset nor treats a re-opened first track as the sequence error; the queue length is read at the boundary so an append made while a track plays still follows it (caught by the existing T18 test); the per-track body moved to `play_turn`. Dead `Progress.track` and `Player.start_track` removed. Found by reading, not reproduced: in file mode `play_from` starts the stream at index 0 while marking row `from + index`.
