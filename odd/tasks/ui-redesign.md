# UI redesign — a modern terminal interface in the style of cliamp

Objective: restyle velo's interface after the reference video (cliamp.stream), visual first, spectrum later.
Branch: `feat/ui-redesign` (worktree `../velo-worktrees/ui-redesign`, from `main` `ed92ea4`). Delivery: no pull requests — commits accumulate on this worktree, then `main` is brought in and the UI updated over it (L7). Runner: `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`.

## Specs

**S1 — Style.** The interface follows the style of the reference video.
> "Que sea una TUI no significa que tenga una interfaz fea. Podemos usar una como esta de cliamp.stream que puedes ver en el video (/Users/alvaldes/Downloads/cliamp.mp4). Es moderna y muestra el estilo que quiero."

Read from the video (interpretation, not the user's words): a framed window with a title bar and a letter-spaced wordmark; a now-playing block (title with ♫, a dim subtitle, the clock, a state pill, a right-aligned fact); a meter band; a volume slider with a knob and its number; transport buttons with a filled accent primary button; a list header line; numbered rows with a `▸` marker, the current row on a subtle highlight, facts right-aligned.

**S2 — Stages.** Visual first; the spectrum analyzer is a second stage.
> "primero lo visual, el espectro en una segunda etapa"

Stage 1 changes `src/ui/` (and its tests) only. Stage 2 (the spectrum) taps the audio path and is tracked separately (T5).

**S3 — Nothing lost.** *Assumption*: every existing key, view (browse, queue, search, help), the cover art and the status messages keep working, and the screen never shows a control or a number velo does not have (no shuffle, repeat, EQ or listener counts), in keeping with the project's honest help.

**S4 — Small terminals.** *Assumption*: the new layout degrades instead of breaking at the sizes the existing tests draw (80×24, 90×12): the now-playing block shrinks before the list disappears.

## Tasks

| ID | Task | Specs | Route | Commit |
|----|------|-------|-------|--------|
| T1 | Theme tokens (surface, border), the window frame and title bar with the letter-spaced wordmark | S1, S3, S4 | inline | `68f790e` |
| T2 | Now-playing block: title and subtitle of the playing track, clock and state pill, progress bar, volume slider, transport buttons | S1, S3, S4 | inline | `6b5fdd6` |
| T3 | Lists restyled (browse, queue, search): header line, numbered rows, right-aligned facts, subtle selection, `▸` on the playing row | S1, S3 | inline | `4b236ff` |
| T4 | Footer key hints and help overlay restyled; golden screen re-recorded | S1, S3, S4 | inline | `ad0e3ee` (hints landed in T1) |
| T5 | Stage 2: spectrum analyzer from an audio tap (FFT), replacing the progress band's spare room | S2 | — (deferred) | — |

## Log

**L1** (2026-11-07, user): "se que estoy trabajando en T10 y en el resto de tareas faltantes en otro agente. Pero quiero abrir un feature en otro worktree para trabajar en la interfaz de velo. Que sea una TUI no significa que tenga una interfaz fea. Podemos usar una como esta de cliamp.stream que puedes ver en el video (/Users/alvaldes/Downloads/cliamp.mp4). Es moderna y muestra el estilo que quiero."

**L2** (2026-11-07, user, on whether the spectrum is in scope): "primero lo visual, el espectro en una segunda etapa"

**L3** Analysis: the spectrum needs a lock-free sample tap in the real-time output callback (`src/audio/output.rs`) plus an FFT; that is concurrency (high risk) and the same file T11 (ReplayGain, other agent) will edit. Stage 1 stays in `src/ui/`. The playing track's title comes from the library row whose path is playing, falling back to the file name, so no new state mirrors the queue.

**L4** T1 (`68f790e`): RED observed (2 render tests failed on `main`'s code). The frame cost the footer 4 columns and cut `q quit` at 80×24, so the key hints became `key label` pairs that drop from the middle and never lose `? help` / `q quit` (unit-tested). Golden 80×24 screen re-recorded on purpose. Assess: medium, `under_budget`.

**L5** T2 (`6b5fdd6`): RED observed (4 tests). The block is 7 rows at ≥18 content rows, 3 at ≥7, 1 below; volume moved from the footer into the slider (`the_footer_shows_the_volume…` renamed and re-asserted on the slider). The playing title comes from the library row with the playing path, else the file name (unit-tested). Not covered headlessly: the playing/paused look, which needs a sound device. Assess: medium, `under_budget`; branch at ~690 changed lines, over the 400 budget — the chain strategy is asked before the next commit.

**L6** T3: RED observed (3 tests). One `list_row` lays out every list (library levels, files, queue, search): `▸ n. name  note …… fact`, `▶` and accent on the row that plays, selection changes only the background (SURFACE). The list header names the level and count (`▸─ Tide › Harbor  [1 track]`, `▸─ Search /d  [1 match]`). The shell test now looks for the wordmark `V E L O` instead of `velo`.

**L7** (2026-11-07, user, asked how to split the branch into pull requests): "nada de pr tu sigue sumando commit en este worktree, y cuando termines con estas tareas que te faltan traete los cambios que esten en main y acutualiza la UI"

**L8** T4 (`ad0e3ee`): RED observed (1 test). The help is a rounded card titled `keys` on SURFACE, keys in SECONDARY in their own column; every row's text is unchanged, so the existing help tests still pass. The footer hints were already restyled in T1. Stage 1 tasks T1–T4 done; next per L7: bring `main` in and update the UI over it.

**L9** Per L7: merged `main` (T10: CI, release workflow, README, `--version`) with no conflicts. The README's text screenshot showed the old interface; it now holds a real 80×23 render of the new one (browse level, stopped), with a note on the playing state and the shrinking block. CI's checks pass on the merge: `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`. Open: the playing/paused look has only been checked by construction (no sound device in the tests); T5 (spectrum, stage 2) is next.
