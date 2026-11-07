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
| T2 | A version-0 index that cannot be written opens without the migration (genres unknown) instead of failing | S2 | delegated writer (parallel with T3, disjoint surfaces) | — |
| T3 | Background indexing on launch and on `u`, title-bar indicator, reload on completion. Risk: item 4 | S3 | delegated writer (parallel with T2) + independent verifier | — |

## Log

**L1** (2026-11-07, user): "trabajemos en esto: - Si cerrás la ventana con Esc, no hay tecla para volver a abrirla. - Un índice viejo en un archivo de solo lectura ya no abre. La actualización del índice necesita escribir en él. y es importante que se pueda refrescar el indice con una key ademas de que siempre que se abara la app haga reload solo y un indicador en la interfaz que esta reaload"

**L2** Today the launch index runs synchronously inside the terminal session (`index_root` in `src/bin/velo.rs`): the interface shows "indexing …" and is frozen until it ends. S3 needs it on a thread with its own `Library` connection, reporting back to the interface.

**L3** T1 (`51415bd`): RED observed (compile). `App` keeps the config file (`set_config_file`, set by `browse_interface`), and `L` opens the prompt with it whenever the search and the prompt are closed and the list is not command-line files. The help card is full at 80×24, so the `L` row replaced the passive "no library set" row instead of adding one. 17 suites, clippy, fmt green.
