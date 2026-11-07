# A scrollable, searchable help card and the open advisories

Objective: the help card scrolls and filters so every key fits again, and the minor advisories left by the last verifications are fixed.
Branch: `main` (each delivery ends with `scripts/release.sh`). Runner: `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --all -- --check`.

## Specs

**S1 — Help with scroll and search.**
> "si agregar un scroll que baje y suba con j/k y un buscador con /"

Decisions: while the help is open, `j`/`k` and `↓`/`↑` scroll it, `g`/`G` jump to the first/last row; `/` starts a filter — printable keys type into it and the card keeps only the rows whose key or description contains the text (case-insensitive), `Backspace` deletes, `Enter` stops typing and keeps the filter; `Esc` clears an active filter first and closes the help when there is none; `?` closes it. The card is as tall as the window allows, shows where it is (e.g. `12–20 of 26`) when it does not fit, and says when the filter matches nothing. The rows dropped while the card was full come back as their own rows: `L` "set the library folder", `u` "refresh the library index", "no library set: a prompt asks for its path, tab completes".

**S2 — The minor advisories.**
> "arregla los avisos menores"

1. A search open when a background index finishes keeps showing the old track pool: refresh the pool (and re-rank the current query, keeping the selection by track path when it survives) on `index_finished`.
2. When the index could not be opened at launch, `u` says "no library set — press L" although a root is configured: `u` must say the index could not be opened (with its error) instead; and `u` after choosing a library with `L` while no index is open must not silently do nothing — it says why.
3. Separate queries can straddle a background commit (e.g. artists from before, tracks from after): `App::reload` reads its lists inside one read transaction so they are one snapshot.

## Tasks

| ID | Task | Specs | Route | Commit |
|----|------|-------|-------|--------|
| T1 | Help card: scroll, filter, position, restored rows | S1 | delegated writer (context backstop) | — |
| T2 | Advisories 1–3 | S2 | delegated writer, after T1 (same files) | — |

## Log

**L1** (2026-11-07, user, after the parent listed CI/publishing, a real-terminal pass and the full help card): "3. si agregar un scroll que baje y suba con j/k y un buscador con / / arregla los avisos menores / el 1 y el 2 no los voy a hacer todavia"
