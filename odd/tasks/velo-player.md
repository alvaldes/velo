# velo — library music player for the terminal

Rust, single binary, no external daemon. New repository at `/Users/alvaldes/Developer/github.com/alvaldes/velo`; `gentle-stream` is read-only reference for design lessons, not code.

Goal: publish it so other people use it. Installation of one command, single binary, docs, releases.

## Specs

**S1 — Product.** A local *library* player for the terminal, not a generative engine.
> "Reproductor de mi biblioteca"

**S2 — Stack.** Built from scratch in Rust, in a new repository.
> "Rust desde cero, repo nuevo"

**S3 — Objective.** Publish it and have other people use it: one-command install, single binary, docs, releases.
> "Publicarlo y que otros lo usen"

**S4 — Location.** `/Users/alvaldes/Developer/github.com/alvaldes/velo`.
> "~/Developer/github.com/alvaldes/velo"

**S5 — Formats.** MP3, FLAC, M4A/AAC, OGG Vorbis, Opus, WAV.
> "el alcance del de arriba"

Status: MP3, FLAC, M4A/AAC, OGG Vorbis and WAV are decodable with symphonia 0.5. **Opus has no decoder in symphonia 0.5.4 or 0.6.1**, so it cannot ship in v1 without adding a bundled `libopus` build (see T14). The requirement stands; only its schedule moved.

**S6 — Library.** Scan one root folder into SQLite; browse by artist, album and track.

Root folder: `/Users/alvaldes/Music/Library` (L15). Metadata comes from tags first, then from the `Artist/YYYY - Album/NN - Title.ext` path layout when a file carries none.

**S7 — Gapless.** The next track is pre-decoded and joined with no silence inserted, no clicks, no padding.

**S8 — Transport.** Pause, seek and volume, all without clicks.

**S9 — Search.** Fuzzy search across the library.

**S10 — Cover art.** Rendered in the terminal through kitty, sixel, iTerm2 or half-block protocols.

**S11 — Queue.** A play queue the user can edit.

**S12 — Config.** TOML at `~/.config/velo/`.

**S13 — CLI.** `velo --help` documents every flag and key.

**S14 — Subsonic/Navidrome.** Stream from a Subsonic-compatible server. Later version.
> "lo que hace subsonic/navidrom y replaygain tambien hay que hacer no importa que sea en otra versión"

**S15 — ReplayGain.** Track/album gain read from tags and applied in the audio path. Writing tags back is a later version.

**S16 — Platforms.** macOS (CoreAudio) and Linux (ALSA) supported; Windows must compile but is untested.

**S17 — No external daemon.** The binary does everything; no MPD, no server, no sidecar process.

## Tasks

| ID | Task | Specs | Route | Commit |
|:--|:--|:--|:--|:--|
| T1 | Audio spike: decode with symphonia, output through cpal, lock-free ring buffer, gapless join between two tracks | S5, S7, S16 | inline | `cc2d967` |
| T2 | Replace the spike's resampling with rubato; audible verification of the join | S7 | inline (worker) | `a6adebc` |
| T3 | ratatui skeleton: layout, keybindings, themes | S1, S13 | inline (worker) | `f8aee9f` |
| T4 | Library scan: jwalk + lofty + rusqlite schema, artist/album/track browser | S6 | inline (worker) | `8b42092` |
| T5 | Play from the library and manage the queue | S11 | inline (worker) | `b931250` |
| T6 | Pause, seek, volume with fades | S8 | inline | — |
| T7 | Fuzzy search with nucleo | S9 | inline | — |
| T8 | Cover art with ratatui-image | S10 | inline | — |
| T9 | clap CLI, config TOML, `--help` | S12, S13 | inline | — |
| T10 | README, install path (brew / cargo-binstall), release workflow | S3 | inline | — |
| T11 | ReplayGain read + apply in the audio path | S15 | inline | — |
| T12 | Subsonic/Navidrome backend | S14 | inline | — |
| T13 | ReplayGain tag writer | S15 | inline | — |
| T14 | Opus decode via a bundled `libopus` (`audiopus_sys`), keeping the single-binary promise | S5 | inline | — |
| T15 | Evaluate the symphonia 0.6 API (`codecs::audio::AudioDecoder`, generic buffers) and migrate behind the existing tests | S5, S7 | inline | — |
| T16 | Trim AAC/MP4 encoder priming: symphonia 0.5 does not populate `delay`/`padding` for isomp4, so an M4A join carries up to ~1024 frames of priming (read `iTunSMPB` or the `elst` edit list) | S7 | inline | — |
| T18 | Edit the queue: add, remove, reorder | S11 | inline | — |
| T19 | Report a track that fails mid-queue instead of playing silence to the end | S7, S8 | inline | — |
| T17 | Bit-perfect mode: opt in to reopening the device per track when the rate changes instead of resampling (waits for the config system in T9) | S7, S12 | inline | — |

### Slice boundaries

- T1 (`cc2d967`, branch `feat/audio-engine`): **926 authored lines in one commit**, over the ~400-line work-unit budget. Splitting it after the fact was measured and rejected: the decode half alone is ~540 lines, so the split buys the reviewer nothing while breaking a compile-consistent commit. Recorded as a known deviation. From T2 on, each unit stays at or under ~400 authored lines, and a unit whose Risk line names a high-risk item gets its own commit.
- T2 (`a6adebc`): **~700 authored lines** (`resample.rs` 290, `tests/resample.rs` 200, `output.rs` 62, docs), again over budget. The cause was bundling three things in one unit: the converter, the device-format selection, and the wiring. Accepted as recorded; T3 splits its own work into the shell and the keybinding layer so each commit stays reviewable.
- **Delivery strategy (decided by the human):** no pull requests. `feat/audio-engine` was merged into `main` in one `--no-ff` merge commit (`4ce9329`), which keeps the three units visible as separate commits with their own tests and revertible one by one. Main was verified after the merge, not before it. The verbatim decision is L14.
- T3 (`f8aee9f`): **~660 authored lines** (`ui/mod.rs` 362, `tests/ui_state.rs` 137, `tests/ui_render.rs` 95, `bin/velo.rs` 66). Third unit in a row over budget, all three for the same reason: a new subsystem arrives with its tests and its wiring in one piece. Noted rather than hidden; the remaining units are scoped so the interface already exists and new work lands as increments to it.

## Log

**L1** (original request, verbatim):
> Tengo este repo que este es un reproductor de música. El problema es que quiero crear el mío propio, utilizando este, digamos, como una plantilla. Quizás antes de iniciar pensar en que voy a utilizarlo como una herramienta de terminal, entonces quizás no sea la mejor opción. Analizar primero qué otras opciones hay en el mercado, quizás Rust he visto que son bastante rápidas las aplicaciones creadas y se pueden usar en terminal

**L2** (decisions from the questionnaire, verbatim option labels): Product — "Reproductor de mi biblioteca"; Stack — "Rust desde cero, repo nuevo"; Objective — "Publicarlo y que otros lo usen".

**L3** (scope, verbatim):
> el alcance del de arriba pero l o que hace subsonic/navidrom y replaygain tambien hay que hacer no importa que sea en otra versión

Interpretation: S1–S13 and S16–S17 are v1; S14 and S15 are required by the project but may ship in later versions.

**L4** (environment finding): `cargo` and `rustc` are absent from this machine; no `~/.cargo`, no rustup toolchains, no Homebrew rust formula. Rust must be installed before any task can be built or verified. `mise` 2026.11.2 is present and already manages Node on this machine.

**L5** (toolchain): installed as `mise use -g rust@stable` → rustc 1.99.1, cargo 1.99.1, clippy, rustfmt, in `/Users/alvaldes/.local/share/mise/installs/rust/`. Pinned in the repo by `.mise.toml` (`rust = "1.99.1"`) and `mise trust`.

**L6** (dependency findings): `symphonia` 0.6.1 renamed its public API (`codecs::audio::AudioDecoder`, generic audio buffers) and, like 0.5.4, ships **no Opus decoder**. The spike pins `symphonia@0.5` so the risk stays in the gapless join rather than in API archaeology; T15 handles the migration. Resolved versions: `cpal` 0.18.2, `rtrb` 0.4.1, `symphonia` 0.5.x, `rubato` 5.0.1.

**L7** (functional verification, T1): decoding works for FLAC, MP3, OGG Vorbis and WAV. Two real bugs surfaced and were fixed or tracked: (a) **M4A/AAC failed at open** because the container declares no channel layout until the decoder parses the first packet — `TrackSource::open` now decodes the first block and uses it as `pending`, with the spec taken from the decoder instead of the container (`tests/formats.rs`); (b) **AAC priming is not trimmed** by symphonia 0.5 for isomp4 (an MP3 of the same 2.00s trims to exactly 88 200 frames; the M4A decodes ~1 024 frames long), so AAC joins are not yet gapless — tracked as T16. Opus is rejected at probe time, as expected until T14.

**L8** (T1 acceptance evidence): `cargo test` 5/5 green (3 gapless rules, 2 format rules); `cargo clippy --all-targets` 0 warnings; `cargo fmt --check` clean. Playing a quiet 440 Hz FLAC followed by a 660 Hz FLAC exits 0 with **zero underruns** — the first attempt reported 1 underrun, fixed by pre-filling the ring before the device starts and by counting an underrun only while the producer still has work. `velo /a.flac /mono.wav` rejects the join with `sample format mismatch: sequence is 44100 Hz/2 ch, track is 44100 Hz/1 ch` and exit code 1; a missing file exits 1; no arguments exits 2. MP3 of 2.00s reports exactly 88 200 frames, proving LAME's priming is trimmed.

**L9** (product decision, verbatim):
> voy con tu recomendacion

Interpretation: resample every track to the output format by default (T2), so a queue never stops; bit-perfect playback becomes an opt-in setting (T17) once the config system exists (T9), because an option nobody can configure is not an option. Channel-layout differences keep being rejected: converting a rate preserves pitch and level, remixing a layout does not preserve anything.

**L10** (T2 acceptance evidence): `cargo test` 10/10 (5 resample rules: exact passthrough, length within 1%, pitch within 20 Hz, level within 0.02, layout rejected), `cargo clippy --all-targets` 0 warnings, `cargo fmt --check` clean. A mixed-rate queue — 96 kHz FLAC, 44.1 kHz FLAC, 44.1 kHz MP3 — plays as one stream: exit 0, **zero underruns**. A mono track is still rejected with the same `SpecMismatch` message. The delegated writer found a genuine behavioural RED on its own: its first converter measured a 1 kHz tone at 1052.2 Hz because the resampler's startup ramp had not been dropped — a 5% pitch error that no length assertion would ever catch. Parent review then caught a defect no test covered: `flush()` computed `out_frames - target` without saturating, so a stalled resampler would panic in debug and discard the whole tail in release; fixed, and pinned with a test for the branch where the tail arrives only through `flush`.

**L11** (T2 performance, resolved): on a debug build the 96 kHz sinc path spent ~2.9 s of CPU per 3 s of audio, which looked like a real-time risk. On a **release** build the same queue (3 × 1 s of 96 kHz FLAC through the sinc resampler) plays in 3.31 s wall time using **0.04 s of user CPU** — about 1.3% of one core — with zero underruns. No real-time risk to carry into the interface work; only note that a debug build is not representative of playback cost.

**L12** (T3 acceptance evidence): `cargo test` 22/22 (8 keymap rules in `tests/ui_state.rs`, 4 render rules in `tests/ui_render.rs`), `cargo clippy --all-targets` 0 warnings, `cargo fmt --check` clean. `--info` and `--play` keep their previous behaviour and exit codes; a missing terminal exits 1 with a message.

The interface was not assumed to work, it was driven: inside a real pty with a 24×90 window — via a small Python harness, because `script` hands the child a zero-size pty and the app then correctly draws nothing, which is itself why the first attempt captured only 77 bytes. The pty frame proved it renders the wordmark, the count, the state, both tracks with their folder, and the key hints. It also exposed a defect the writer's "verified by construction" claim had missed: the footer printed a separator with nothing before it — `, stopped`. Removed, and the rule is now pinned by `the_footer_has_no_dangling_separator`. Volume is stored but not yet audible (T6); the palette was judged from ANSI-stripped frames, so a human eye still owes the interface one look.

**L13** (resolved, not a decision on its own): an entry appeared in this Log, written by something other than this session, claiming the library root is `~/Music/Library`. It quoted no human message and was recorded as unattributed for that reason. The claim turned out to match reality, which does not make an unattributed edit acceptable — L15 records the human's confirmation and the verification that now settles the path.

**L15** (library root, verbatim):
> si usa la ruta music/library

Verified after the confirmation: `/Users/alvaldes/Music/Library` exists, laid out as `Artist/YYYY - Album/NN - Title.ext` with a `cover.jpg` per album. The tree is being populated live, so any count is a snapshot: it held 32 MP3 across 3 artists when checked, and 104 audio files across 16 artists by T4 acceptance (56 mp3, 33 ogg, 4 m4a, 4 flac, 4 wav, 3 opus). That growth is why the incremental re-index matters. Every sampled file carries real ID3 tags (title, artist, album, track, date) at 44.1 kHz stereo, and the album tag disagrees with the folder name (`Lo-Fi Scientists EP` vs `2009 - Lo-Fi Scientists`), so **tags are the source of truth and the folder layout is only a fallback** for files that carry none. T4 indexes this root by default; T9 makes it configurable.

**L16** (T4 acceptance evidence): `cargo test` 27/27 across nine suites (5 library rules), `cargo clippy --all-targets` 0 warnings, `cargo fmt --check` clean. Parent verification ran the binary against the real library with a throwaway `VELO_DB`: **104 tracks, 23 albums, 16 artists**, matching the tree exactly (104 audio files among 116 files), **zero duplicate paths, zero rows pointing at a file that does not exist**. A second run over an unchanged tree is idempotent and takes 22 ms, which is the incremental shortcut working. `--library /no/such/dir --index` exits **1** with a message. Data quality spot-check: 0 empty titles, 0 null albums, 0 null track numbers, 0 null durations, 56 rows with embedded cover art, and the album of `Lo-Fi Scientists` resolves to the tag `Lo-Fi Scientists EP` rather than the folder name `2009 - Lo-Fi Scientists` — tags really do win.

Two corrections worth recording. First, an earlier count of "32 MP3 across 3 artists" was stale within the same session because the human is actively adding music; counts in this document are snapshots, never invariants. Second, a `--library /no/such/dir` run first appeared to exit 0 — that was a pipe swallowing the status code in the verification command, not a defect; measured without the pipe it exits 1.

**Known limit (T4):** the path fallback understands exactly `Artist/Album/file`. An untagged file in a flatter or deeper tree would get an empty title. It does not occur in the current library (0 empty titles across 104 tracks), so it is recorded as a limit rather than guessed at. The worker also found and fixed a real case from live data: an album folder with no `YYYY - ` prefix was falling through to "Unknown album" and now keeps its full folder name with no year.

**L17** (T5 acceptance evidence): `cargo test` 33/33 across nine suites, `cargo clippy --all-targets` 0 warnings, `cargo fmt --check` clean. The audio callback was verified unchanged: only the producer gained a counter and the player a read-only accessor.

The browse flow was driven in a real pty, which is the only reason two defects were caught: the artist list rendered `1 albums · 4 tracks` for almost every artist (counts of one were not pluralised), and while the app was indexing, the body said *"no artists in the index — run velo --index to scan the library"* directly under a status line that said *"indexing …"*. Both are fixed here and the count rule is pinned by `a_count_of_one_is_not_pluralised`. Nobody would report either as a bug; everybody would notice them.

Counts are again a snapshot: the tree reached 140 tracks / 27 albums / 20 artists while this ran, and the incremental re-index over the grown tree stayed at 27 ms.

Known limits recorded, not fixed: the queue-editing half of S11 is not implemented (S11 says the user can edit the queue; today an album plays from the selected track to its end); and a track that fails *mid*-queue still plays silence to the end, because the interface never polls `wait_until_finished`, so producer errors surface only when the first file fails at selection time.

**L18 — UNATTRIBUTED, NOT A DECISION.** An uncommitted `[tasks.dev]` block appeared in `.mise.toml`: it builds velo in release and opens one album from `~/Music/Library`, defaulting to `Poldoore/2012 - Lush Life`, using `fd`. Neither this session nor the delegated writer wrote it. It is useful and was left untouched and uncommitted rather than adopted or deleted; the human should say whether it is theirs. This is the second unexplained edit to appear in this repository, after the L13 entry.

**L19** (the `.mise.toml` block, resolved): the human confirmed authorship — verbatim *"si lo hice yo para probar la app"* — and asked that it not be committed: *"pero no lo comitees"*. Verified by running it in a real pty: `mise run dev` prints mise's own echo, then opens the interface in argv mode on the seven tracks of `Poldoore/2012 - Lush Life` (`velo  7 tracks … stopped … 01 - Lush Life.mp3`), and exits 0 on `q`. The task is correct as written: it validates the directory, collects the six formats with `fd`, keeps paths with spaces quoted, and `exec`s so no shell wrapper survives. L18 is resolved: the block is the human's, it works, and it stays out of the repository on their instruction. Every future commit must stage files explicitly so it is never swept in.

**L20** (the two gaps, decided by the parent): the human's answer was *"no se que quieres que responda aqui"*, so the decision is mine and it is not theirs to make in the abstract. Both gaps are real and neither belongs inside T6: queue editing becomes **T18** and mid-queue failure reporting becomes **T19**, both after the transport. S11 stays open until T18 lands, and the silent-to-the-end failure is recorded as a known defect of the current player rather than as a limitation of the spec. No further question is needed for them.
