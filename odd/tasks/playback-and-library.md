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

**S4 — A config file with every default.**
> "hoy no hay .config/velo/config.toml crealo y llenalo con la configuracion por defecto para cualquier usauario pueda custumizar velo. Por tanto todo puede cambiarse desde este archivo. Incluso la ubicacion de library asi cuando entras por primera vez y esta regisstrado aqui ya se carga todo. Si no esta en config.toml entonces debe mostrar una ventana flotante dentro de velo para escribir el path (ya sea absoluto o relativo '~')"

## Tasks

| ID | Task | Specs | Route | Commit |
|----|------|-------|-------|--------|
| T1 | Repeat in the producer: `Repeat`/`RepeatCell`, `next_track`, first-turn flag for the seek offset, stop when every track fails in a row. Risk: item 4 | S1 | inline + independent verifier | `caa5d8a`, fix `91d8fbf` |
| T2 | Shuffle and repeat keys, queue reorder through `edit_queue`, indicators, help, README | S1 | inline | `85ab08a` |
| T3 | Cover inside the now-playing block | S2 | inline | `c8937bb` |
| T4 | Library views: artists, albums, all tracks, genres (genre needs the index to store it) | S3 | inline | — |
| T5 | `~/.config/velo/config.toml` written with every default on first run; no library configured → a floating prompt for the path (absolute or `~`) | S4 | inline | — |

## Log

**L1** (2026-11-07, user): "agrega shuffle y repeat"

**L2** (2026-11-07, user, with a screenshot of the cover in the right quarter): "hay que buscarle una mejor posicion a la imagen porque aqui no queda bien, rompe el disenno"

**L3** (2026-11-07, user): "el library todo esta organizado en una vista donde para reproducir algo hay que hacer minimo 3 click (o movimientos) esto no esta mal pero hay que tambien agrear diferentes vistas como vista por artista o por album. Incluso ver todos los audios ahi mismo o por categoria (lofi, jazz, classic, ...)"

**L4** T1: RED observed (compile). The producer's `for index in start_track..` became a loop driven by `next_track(index, len, repeat)`; "first turn" is a flag (Progress.first), not `index == start_track`, so a repeat neither re-applies a seek offset nor treats a re-opened first track as the sequence error; the queue length is read at the boundary so an append made while a track plays still follows it (caught by the existing T18 test); the per-track body moved to `play_turn`. Dead `Progress.track` and `Player.start_track` removed. Found by reading, not reproduced: in file mode `play_from` starts the stream at index 0 while marking row `from + index`.

**L5** (2026-11-07, user, mid-work): "hoy no hay .config/velo/config.toml crealo y llenalo con la configuracion por defecto para cualquier usauario pueda custumizar velo. Por tanto todo puede cambiarse desde este archivo. Incluso la ubicacion de library asi cuando entras por primera vez y esta regisstrado aqui ya se carga todo. Si no esta en config.toml entonces debe mostrar una ventana flotante dentro de velo para escribir el path (ya sea absoluto o relativo '~')"

**L6** T1 independent verifier on `caa5d8a`: one blocker — under One/All a track that opens but yields no samples (a header-only WAV) was `Played`, so the producer reopened it ~21 000 times a second (no ring backpressure, `push_all` returns at once on an empty slice). Off matched the baseline. Fix `91d8fbf`: a turn with no samples is `Turn::Silent`, counted like a failure and never repeated alone; RED reproduced it (9 416 replays in 0.5 s), GREEN after; recheck limited to that blocker. Advisories left: One follows the index when entries before it are removed; One falls back to Off when its track fails.

**L7** T2 (`85ab08a`): RED observed (compile). `s` mixes the entries after the playing one through `edit_queue` (Fisher–Yates over xorshift, no new dependency) and `s` again restores their order, with tracks queued meanwhile after them; a new stream under shuffle keeps the chosen track first. `r` cycles Off → All → One and reaches a running player via `set_repeat`. The `▶` marker and the track length are now found by the playing file's path, not `from + index`, which a shuffled queue breaks; `Playback` lost its now-unused fields. Golden and README row re-recorded. 17 suites, clippy, fmt green.

**L8** T3 (`c8937bb`): RED observed (compile). The cover moved from the right quarter of the whole content (floating above the list, dead space beside and below it — the user's screenshot) to the left of the now-playing block: as tall as the block less its gap row, twice as wide (≈ square in cells), two columns of air; the block's text moves right and the list keeps the full width. No cover in the compact tiers or when the text would be under 48 columns; the window threshold is unchanged, so `hidden_reason` still tells the truth. The placement is the pure `cover_slot`, unit-tested; drawing it needs a graphics terminal and was not seen here.
