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
| T5a | Audio tap: the device callback pushes each frame's mono mix into a second lock-free SPSC ring; full ring drops, never blocks. Risk: item 4 (concurrency) | S2 | inline + independent verifier | `c87743c` (verifier: PASS) |
| T5b | Spectrum analysis: Hann window, radix-2 FFT, log-spaced bands, rise at once and fall smoothly, silence falls to zero | S2 | inline | `6f8483a` |
| T5c | Spectrum in the now-playing block (4 rows of eighth-block bars with a vertical gradient, baseline when silent) on windows tall enough; ~30 fps redraw while playing | S1, S2, S4 | inline | `a5d712a` |
| T6 | Cover art from the folder (`cover`/`folder`/`front` `.jpg`/`.png`) when the file embeds none | S1, S3 | inline | `2053d98` |
| T7 | The volume number takes three columns so the right-aligned slider stops jumping | S1 | inline | `d47ce9a` |

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

**L10** (2026-11-07, user, after trying the build): "la lo probe pero la imagen no la veo por ninguna parte. Y ademas el vol hace un salto al estar alineado a la derecha y pasar de 100 a 99 un digito menos. cuando termiines sigue con el espectro"

**L11** Analysis of the missing cover: only 43 of the 247 tracks in `~/Music/Library` embed a picture, while 12 of 18 album folders hold a `cover.jpg`; velo read only the embedded one and a track without art is silent by design (T8b). Second possible cause: this session runs under `TERM_PROGRAM=herdr` (inside Ghostty); `ratatui-image` guesses by env vars, then queries the terminal, and a multiplexer that does not pass kitty graphics through yields no protocol — then the status line says so. T6: folder fallback, decoded by content (a PNG named `.jpg` still decodes); RED observed (1 test), the embedded-wins test passed already and pins existing behavior. T7: RED observed (the slider moved at 95%); `●─ 95` became `●─  95` in the older slider test, deliberately.

**L12** T5 design. The tap sits in the device callback, not the producer: the producer runs up to 2 s ahead (the ring), so only the callback is in sync with what is heard. It taps pre-gain samples, so the bars show the music, not the volume knob; a paused callback returns early, pushes nothing, and the bars fall. A hand-written radix-2 FFT (tested against a known sine) instead of adding `rustfft`: one dependency fewer in a published single binary. The event loop polls every 100 ms (10 fps), too choppy for bars: 33 ms while playing, 100 ms otherwise. The 11-row block with the spectrum needs ≥22 content rows, so the 80×24 golden screen does not change.

**L13** T5a (`c87743c`): RED observed (compile: `tap_mono` missing). Independent verifier on the frozen commit: PASS, no blockers; `cargo test`, clippy, fmt green; probes showed `RingBuffer::new(0)` does not panic and a zero-channel tap pushes nothing. Advisories taken: drain the tap every tick even when the spectrum is hidden (else it holds stale audio — done in T5c via `App::listen`), and the stale `build_stream` doc comment (fixed in T5c). Native review: `managed_assets_outdated` cleared with `gentle-ai sync --agent claude-code`; START pending for the T5 slice.

**L14** T5b (`6f8483a`): RED observed (compile). Hand-written radix-2 FFT, Hann window, 40 Hz–16 kHz log bands, −60…−6 dB mapped to 0…1, rise at once, fall 0.06 per update; a 1 kHz tone lights its band and not 100 Hz or 8 kHz.

**L15** T5c: RED observed for the bar geometry (compile); the window and cadence tests were written with it. 11-row block at ≥22 content rows (80×24 unchanged), baseline `▁` in BORDER when silent, gradient BAR_LOW→BAR_HIGH, 33 ms redraw while playing or while bars still fall. Process slip: a `git checkout` used to drop a throwaway dump test also reverted the uncommitted T5c render tests; they were re-added verbatim and pass.
