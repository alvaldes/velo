# UI redesign — a modern terminal interface in the style of cliamp

Objective: restyle velo's interface after the reference video (cliamp.stream), visual first, spectrum later.
Branch: `feat/ui-redesign` (worktree `../velo-worktrees/ui-redesign`, from `main` `ed92ea4`). Delivery: `ask-on-risk`. Runner: `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`.

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
| T1 | Theme tokens (surface, border), the window frame and title bar with the letter-spaced wordmark | S1, S3, S4 | inline | — |
| T2 | Now-playing block: title and subtitle of the playing track, clock and state pill, progress bar, volume slider, transport buttons | S1, S3, S4 | inline | — |
| T3 | Lists restyled (browse, queue, search): header line, numbered rows, right-aligned facts, subtle selection, `▸` on the playing row | S1, S3 | inline | — |
| T4 | Footer key hints and help overlay restyled; golden screen re-recorded | S1, S3, S4 | inline | — |
| T5 | Stage 2: spectrum analyzer from an audio tap (FFT), replacing the progress band's spare room | S2 | — (deferred) | — |

## Log

**L1** (2026-11-07, user): "se que estoy trabajando en T10 y en el resto de tareas faltantes en otro agente. Pero quiero abrir un feature en otro worktree para trabajar en la interfaz de velo. Que sea una TUI no significa que tenga una interfaz fea. Podemos usar una como esta de cliamp.stream que puedes ver en el video (/Users/alvaldes/Downloads/cliamp.mp4). Es moderna y muestra el estilo que quiero."

**L2** (2026-11-07, user, on whether the spectrum is in scope): "primero lo visual, el espectro en una segunda etapa"

**L3** Analysis: the spectrum needs a lock-free sample tap in the real-time output callback (`src/audio/output.rs`) plus an FFT; that is concurrency (high risk) and the same file T11 (ReplayGain, other agent) will edit. Stage 1 stays in `src/ui/`. The playing track's title comes from the library row whose path is playing, falling back to the file name, so no new state mirrors the queue.
