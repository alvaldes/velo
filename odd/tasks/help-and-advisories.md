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
| T1 | Help card: scroll, filter, position, restored rows | S1 | delegated writer (context backstop) | `9de4040` |
| T2 | Advisories 1–3 | S2 | delegated writer, after T1 (same files) | — |

## Log

**L1** (2026-11-07, user, after the parent listed CI/publishing, a real-terminal pass and the full help card): "3. si agregar un scroll que baje y suba con j/k y un buscador con / / arregla los avisos menores / el 1 y el 2 no los voy a hacer todavia"

**L2** (2026-11-07, writer, T1/S1): the help is now `App.help: Option<Help>` (`top`, `filter`, `typing`; no filesystem) and `App::on_key` routes to `on_help_key` after the search and the library prompt, so no key reaches the list underneath. The rows live in `HELP_ROWS` (24): `L / u` is split into `L` "set the library folder" and `u` "refresh the library index", and `""` "no library set: a prompt asks for its path, tab completes" is back; they sit after `1 2 3 4` so the seek rows stay inside a 90×12 window. The card is as tall as the window allows, keeps its width while filtering, shows `a–b of n` right-aligned on its bottom edge only when the rows do not fit, a dim ` / filter ` hint on that edge while no filter is set, the filter line (`/ text▏` while typing) under the title, and `no key matches “text”` when nothing matches. Scroll is clamped with the last drawn area (`seen_area`); before any draw every row counts as shown. RED: 5 `ui_render` tests and 1 `ui_state` test failed before the change (`the_help_scrolls_with_j_k_and_jumps_with_g_and_shift_g`, `slash_filters_the_help_and_says_when_nothing_matches`, `enter_keeps_the_filter_and_j_k_scroll_again`, `the_help_brings_back_the_library_rows`, `the_help_names_the_key_that_refreshes_the_index` (its text now "refresh the library index"), `an_open_help_owns_the_keys_and_esc_clears_its_filter_before_closing`); `a_help_that_fits_shows_no_position` is a guard that passed before. GREEN: `cargo fmt --all -- --check` clean, `cargo clippy --all-targets -- -D warnings` clean, `cargo test` 260 passed, 0 failed (the 80×24 golden with the help closed unchanged). Writer decisions: while typing every printable key types, `?`, `q`, `j`, `k`, `g` included (as in the library search), so `?` closes only when not typing; `Esc` while typing with an empty filter stops typing without closing; `/` with a kept filter resumes editing it; `Backspace` and `Enter` do nothing when not typing; every other key is swallowed while the help is open (playback keys too, `q` included). README `?` row updated. Gap: no real-terminal pass.
