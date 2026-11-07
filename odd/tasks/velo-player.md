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
| T6 | Pause, seek, volume with fades | S8 | inline (worker) + fixes `9a8dff1`, `8139a22` | `8e2c4bc` |
| T7 | Fuzzy search with nucleo | S9 | inline | `2282892` |
| T8 | Cover art with ratatui-image | S10 | inline | `feb8a43` |
| T9 | clap CLI, config TOML, `--help` | S12, S13 | inline | `6b7f180` |
| T10 | README, install path (brew / cargo-binstall), release workflow | S3 | inline | `5c28143` |
| T11 | ReplayGain read + apply in the audio path | S15 | inline | `c8a418c` |
| T12a | Subsonic client and authentication: JSON, token or API key, error sentences, `--subsonic-test` | S14 | worker (clone) | `f6f7ec7` |
| T12b | Sync the remote library into the index as rows with a `source` column; search over them | S14 | worker (clone) | `b2eef0b` |
| T12c | Cache-on-play: fetch the original bytes with `format=raw`, then play locally (keeps seek, gapless, bit-perfect) | S14 | pending | — |
| T12d | Remote cover art via `getCoverArt` and scrobbling on track change | S14 | pending | — |
| T13 | ReplayGain tag writer | S15 | inline | — |
| T14 | Opus decode via a bundled `libopus` (`audiopus_sys`), keeping the single-binary promise | S5 | inline | — |
| T15 | Evaluate the symphonia 0.6 API (`codecs::audio::AudioDecoder`, generic buffers) and migrate behind the existing tests | S5, S7 | inline | — |
| T16 | Trim AAC/MP4 encoder priming: symphonia 0.5 does not populate `delay`/`padding` for isomp4, so an M4A join carries up to ~1024 frames of priming (read `iTunSMPB` or the `elst` edit list) | S7 | inline | — |
| T18a | Share the queue with the interface so edits reach the audio (proved by appending mid-track) | S11 | inline | `9ca9b4f` |
| T18b | Queue view and keys: `Tab` to switch, `a` append, `d` remove, `J`/`K` move, refusals in the status line | S11 | inline | `e43c02b` |
| T19 | Report a track that fails mid-queue instead of playing silence to the end | S7, S8 | worker `gentle-ai-worker` | `207dc0f` |
| T20 | Remap the channel layout instead of refusing it: a mono track could not be played at all, and in a queue it took the album down with it (found while verifying T19) | S5, S7 | inline | `0bd1e9d` |
| T21 | Expose the current track's duration so the clock clamps and the footer shows `/ total` when the queue came from the command line (found while proving T20) | S8 | inline | `de97f97` |
| T17 | Bit-perfect mode: opt in to reopening the device per track when the rate changes instead of resampling (waits for the config system in T9) | S7, S12 | inline | `6771fcc` (in clone, not merged) |

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

**L21** (T6 acceptance evidence): 46 tests green (42 integration plus 4 unit tests of the clock arithmetic), `cargo clippy --all-targets` 0 warnings, `cargo fmt --check` clean. The audio callback was inspected line by line: it locks nothing, allocates nothing, logs nothing — the `Mutex` the writer introduced is touched only by the producer thread and the UI.

Parent review caught one real defect the worker's tests could not: after a seek the clock restarted at `00:00` mid-song, because `position()` subtracts where the track began but never adds the seek offset back. Fixed by extracting the arithmetic into a pure `elapsed_frames(played, track_start, track, seek_track, seek_offset)` with four unit tests, rather than leaving it untestable inside `Player::position`, which needs a device.

**Verification technique worth keeping.** Twice I read a raw pty byte stream and concluded a defect that did not exist: the footer looked stuck at `vol 10%` and the clock looked frozen at `00:00`. Both were my mistake — ratatui writes only the bytes that changed, so a diff stream is not a screen. The fix is to force a full redraw by resizing the pty (`TIOCSWINSZ`), which makes ratatui repaint everything; that immediately showed `00:03 / 01:45` while playing and `00:05 / 01:45` after a 5 s seek. Never diagnose a TUI from a raw diff stream.

Still only a human ear can confirm: that the fades are click-free at the device's real buffer size (the ramp math is pinned, the device is not), and that a seek sounds clean on MP3 and M4A (the tests pin WAV, sample-exact).

**L22** (T6 defects found by using it, verbatim report):
> el seek -> no funciona no se mueve a delante sino a donde mismo esta por lo que pude ver y el otro si parece funcionar. los volumenes funcionan bien pero al hacer seek se suben de repente y no respetan el volumen que esta puesto.

Three separate things came out of that report, and only two were code:

1. **The volume jump was real and is fixed** (`9a8dff1`). A seek rebuilds the player, and the rebuilt player began at its own default level of 100. The level is now re-applied on every rebuild, and the footer reads it from the player instead of a mirror that can drift. Verified in a pty: three steps down to 85%, seek, forced full redraw → `vol 85%`, unchanged.
2. **A units bug in the previous fix**: the seek offset was stored in the track's frames and then divided by the *output* rate, so the clock was wrong whenever the two differed (the user's library is 48 kHz MP3 on a 48 kHz device, so it did not show for them). The decoder keeps the track-rate offset; the clock now keeps its own.
3. **The seek itself could not be reproduced as broken**, and four independent measurements say it moves: a UI trace showed `position=2.438s → target=7.438s`; a producer trace showed `index=0 == start_track=0 offset_frames=362592` (7.554 s × 48 kHz) reaching the decoder; a WAV test with every frame carrying its own index showed the first decoded sample after the seek is exactly frame 1000's; and a new MP3 fixture (one second of silence, then a tone) decoded exactly the tone's level after a seek to 1.5 s. The honest conclusion is that the seek now works and the earlier perception was dominated by the volume jump, but that is an inference, not a proof — it needs the human's ear on a rebuilt binary.

**Lesson, again about test calibration:** the new MP3 test first failed and looked like proof that MP3 seeking is broken. It was my threshold that was wrong: I assumed the fixture's tone would measure near full scale, and it measures **0.084 RMS** (a 32 kbps mono encode of a pure sine). The fix was to assert against the fixture's own measured level, not against an invented one. A test can lie in the accusing direction, and a failing test is a claim to verify, not a verdict.

**L23** (the seek, actually diagnosed): the human's second report was precise and it broke the case open —
> lo que vi es que si se esta respetando el volumen pero el -> salta 5 atras y el <- salta 10 atras tambien

**Root cause: one number in two units.** The seek is computed from the reported position, and the position mixed units. The played counter counts **interleaved samples** (rate × channels); the seek offset was stored as **frames**. On stereo the rebuild therefore reported exactly half its offset — a seek to 9.49 s reported 4.75 s — and the next seek computed from that halved value, so every press landed behind the music and compounded. The offset is now converted with `offset_samples`, pinned by a unit test that asserts one second of stereo 44.1 kHz is 88 200 samples and not 44 100.

Two further defects could freeze the position at zero, both now gone in the same change: the producer published the current **track index before writing that track's start**, so a reader could get an index with no start and report zero (and the UI could then seek from zero); and the start table **filled gaps with zero**, which made a missing entry report the entire played count instead of a position. A single `Progress { track, start }` record, written in one step and read under one lock, removes the whole class.

**Verification that finally worked.** Every earlier instrument measured the *decoder* — the offset reaching `open_at`, `open_at` landing on the exact frame, a device-level read of the first sample — and all of it was correct, which is why the seek looked innocent four times over. The defect lived between that offset and the number the UI used to *compute* it. The instrument that caught it logged two things: what `seek_by` read and computed (`position=4.493s → target=9.493s`), and what the rebuilt player then reported (`position=4.746s`). Half of 9.493 is not a coincidence; that is a unit bug, and it took one line to see once the pair was printed side by side.

Verified end to end with a 40 s fixture whose each second carries its own DC level, read back from the device callback: a seek from 4.5 s lands at 9.5 s, and the clock reads `00:03` before the press and `00:10` after, where it used to read `00:05`.

**Lesson:** when a symptom says "it lands in the wrong place", instrument *both* sides of the decision that produced it — what was read, and what was written — not just the side that looks guilty. Four correct measurements of the wrong layer feel exactly like proof.

**L24** (T19 done, and a worse bug found while proving it): verifying the skip in a terminal exposed a bug T19 could not fix. The fixtures were mono, and the player answered
> velo: sample format mismatch: sequence is 44100 Hz/2 ch, track is 44100 Hz/1 ch

so **a mono track cannot be reproduced at all** — not in a queue, not alone. `RateConverter::new` rejects a channel-layout difference because remixing channels changes the music itself, which is right for a *surround* downmix and absurd for mono: duplicating one channel into two is not a remix, and mono is everywhere (voice memos, old recordings, half of what a random folder holds). In a queue it is worse than useless: the layout mismatch aborts the sequence, so one mono file silences the whole album. T20 is that, and it moves ahead of T18 and T7 because a library player that refuses mono is not publishable.

`SpecMismatch` aborting the sequence stays correct for the case that motivated it (a 5.1 track against a stereo device needs a real downmix decision, not a duplicate), which is exactly the distinction T20 has to draw.

T19's own acceptance, measured not assumed: a queue of `[good, broken, good]` in a terminal showed `velo: skipped broken.mp3: unsupported format: unsupported feature: core (probe): no suitable format reader found` in the status line and moved the playing marker to the second good file; `--play` on the same three announced the skip and exited 0, because a skip is not a failure. The headless test pins the stronger claim: the drained samples are exactly the two good files concatenated, with no silence where the broken one was.

**L25** (T20 done, T21 found): the mono bug was one line of arrogance repeated in two places. `RateConverter::new` refused any channel difference — reasonable for a *surround* downmix, absurd for mono, where duplicating a channel is not a remix — and `output_spec` additionally demanded that the device accept the first track's layout, so the refusal happened before a single block was decoded. Together: **a mono file was unplayable**, alone or in a queue, and inside a queue it aborted the sequence and silenced the album.

`ChannelMap` now owns the decision: matching layouts pass through, mono duplicates into every output channel, several channels average into mono, and a layout needing real downmix coefficients is still refused — those coefficients are a product decision, and inventing them by averaging a 5.1 film mix into stereo is exactly the kind of "helpful" default that ruins audio. A refusal is now a **skip with an announcement**, not a fatal queue error, because a device that cannot hold one track must not silence the album.

The proof that matters is a fixture, not an opinion: a mono WAV drained into a stereo expected spec yields **twice the samples with both channels identical**, and a six-channel WAV is announced once and skipped. The same mono file that printed `sample format mismatch` now plays and reaches `00:03` in the interface, which is where the whole detour started.

**T21**, found while proving it and closed in `de97f97`: when the queue came from the command line, `current_track_duration()` returned nothing, so the footer showed a bare `mm:ss` and `seek_target` had no duration to clamp against — a seek past the end of a 3 s file left the clock frozen at `00:08`. The decoder had always known the length and nothing exposed it: `TrackSource::duration` reports it, the producer publishes it in the same record the clock reads so the two flip together, and the footer falls back to it. Verified both ways: a 0.25 s fixture reports exactly 250 ms, and the interface now reads `00:03 / 00:03` after a seek past the end.

**L26** (T18 split in two, and why): S11 asks for a queue that can be edited, and the first thing that turned up was that the *player did not read the interface's queue at all* — it consumed a `Vec<PathBuf>` at construction, so any edit would have been theatre. That half is done and committed in `9ca9b4f`: the queue is an `Arc<Mutex<Vec<PathBuf>>>`; the producer reads **one entry per track it starts**, under the lock, and reaching the end is how a sequence finishes.

The proof is the part worth remembering. Both new tests drive the editing *from inside the audio production*, deterministically, with no threads to race: one pushes a second track from the first track's own block callback and asserts 1 200 frames are heard (500 + 700), the other empties the queue mid-track and asserts the playing track finishes and the sequence ends without an error. A test that appends *before* draining would have passed against a snapshot too — it would have proved nothing.

**T18b is the remaining half** and is written down here so it does not have to be re-derived: a queue view (`Tab` switches between the library browser and the queue, play order, the playing entry marked the way the library list already marks it); in the library, `a` appends the selection — the track, the whole album, or every album of the artist — without interrupting playback; in the queue view, `d` removes, `J`/`K` move, `Enter` plays from there. The rule that keeps the producer's index valid without renumbering: **only entries after the playing one can be removed or reordered**, the playing entry cannot be removed, nothing moves across it, and appending is always allowed at the end. A refusal writes `velo: ` plus the reason into the status line.

One wrinkle for whoever picks it up: `App` keeps `queue: Vec<PathBuf>` as the drawing mirror and creates the shared handle only when playback starts, so an edit must land on both. Either give `App` the handle as its single truth, or add one helper that applies the mutation to the mirror and to the handle in lockstep — not two call sites that must remember to agree.

**L27** (T18b closed): S11 is now real. `Tab` switches to the queue in play order, `a` appends the selection (a track, or every track of the album) without interrupting playback, `d` removes, `J`/`K` move, `Enter` plays from there, and the status line names the view when it changes so the keys never move somewhere invisible.

The rule that makes editing safe without renumerating the producer's index: **only entries after the one playing are editable**. The playing entry cannot be removed, nothing moves across it, appending is always allowed at the end. Each refusal says why — and in a real terminal the message that came back for `d` was `velo: that track already played`, not the "cannot remove the track that is playing" the code reads first, because by the time the key landed the two-second fixture had advanced. The rule was right and the message was honest about which half applied, which is what "explain the refusal" has to mean if it is going to be worth anything.

One design decision worth keeping: `App::edit_queue` applies a mutation to the drawing mirror **and** to the `SharedQueue` the player reads, in one place. The alternative — two call sites that must remember to agree — is the kind of thing that works until the day someone adds a third.

`queue_move_target` and `editable_queue_index_among` are pure, so the six refusal cases are pinned as rules rather than through a live player; the `on_key` wiring is pinned separately on the empty-queue path. The joint path (a refusal with a real device) is what the terminal run above confirms, because no test here can hold a sound card.

**L28** (T7 closed in `2282892`): search is in, over the whole library, on `/`. Matching is case-insensitive, tolerates a subsequence (`mnlgt` finds `Moonlight`), and ranks a prefix above a scattered match. `Esc` restores the view and selection you left — including from the queue view — because the browse state is never written while a search is open.

Two things worth keeping from how it was built. First, **the matcher is a pure struct**: query text plus a function from candidate strings to ranked indices, in its own module with no terminal, no library and no audio. That is the difference between seven tests that run in milliseconds and a feature nobody can pin. Second, a wrong test that nearly became a wrong implementation: the expectation was that `NIGHT` ranks `Night Drive` above `Moonlight Sonata`, and it does — but `Moonlight` also *contains* `night` as a subsequence, so the honest answer was `[1, 0]`, prefix first, and the test was fixed rather than the matcher. Fuzzy matchers are right far more often than the intuition that writes their assertions.

Verified on the real library, not on fixtures: typing `lofi` lists `Lo-Fi Scientists` — a query with no hyphen matching a title that has one, which is the whole point.

The docs had drifted while the code moved: the README still called the library database and search "not yet", and `--help` still carried the pre-T7 key line without `tab`, `a`, `d`, `J`/`K` or `/`. Both now describe what the binary actually does. A help screen that lies about the keymap is worse than no help screen.

**L29** (T8 closed in `feb8a43`): cover art is in and, more importantly, **nothing else moved**. The 80x24 screen is pinned by a golden test whose expected text was captured from the code *before* the change — that is what turns "the interface still looks the same" from a hope into a check. It earned its keep within the hour: when the footer hints changed, the golden failed and named the one line that differed, which is exactly the behaviour a preservation test should have.

The art is honest or absent. Half-blocks are rejected rather than used to paint a mosaic that is not the cover, and no protocol, no picture, undecodable bytes, or a terminal under 100x28 all leave the frame untouched. Decoding happens once per track change, never per frame, and never in the audio callback.

**A false alarm worth recording**, because it cost time and taught something: the terminal harness reported the app dying mid-playback with cover art in place. It was the harness — it sent `q` as a test key and then sent `q` again as its own way out, so the second write hit a closed pty. The app survived every resize I threw at it afterwards, including 20x60. The lesson is the same one this project keeps relearning from the other side: **when a test harness reports a crash, suspect the harness first, and prove the binary is alive with the smallest scenario that could be guilty** `(enter, wait, resize, read)` rather than the largest one.

**Not verified, and it must be said plainly**: the art as rendered pixels in a real terminal. No Kitty, Sixel or iTerm2 terminal was available here, so the drawing path is covered only up to "a protocol was detected and the size rule allows a pane". That needs a human eye on a real terminal — the one kind of check this project cannot automate.

**L30** (T9 closed in `6b7f180`): `clap`, a config file, and help that stops lying. The exit-code contract was measured on the binary before and after rather than argued about — help 0, usage error 2, missing file 1, missing library root 1 — because that is the one part of a CLI that scripts depend on.

The config is deliberately small and strict: `library`, `volume`, `bit_perfect`; a missing file is silent, an unknown key is an error that **names the key and lists the ones that exist**, and a bad value quotes the file and the line. A config file is the user's own input, and an error that does not quote it back is a riddle. `bit_perfect` is stored now and read by nobody, with a doc comment saying so: T17 needs a place to put it, and adding the setting later would mean migrating a config format that people have already written.

One defect the red-first tests earned, worth keeping: the first version reported **line 3 for an error on line 2**, because it counted lines in the byte prefix of the span rather than counting newlines. A message that points at the wrong line is worse than no line at all — it teaches the user to distrust the error.

The default config path is resolved by a function that **takes the two environment values as arguments**, so the resolution is testable without setting a variable. Edition 2024 made `set_var` unsafe and this repo forbids it in tests; the design that falls out of that constraint is simply better, because the test names the case it exercises instead of mutating global state and hoping.

**L31** (T10 closed in `5c28143`): the project is installable by a stranger — README for somebody who has never seen it, crate metadata that makes `cargo install` and crates.io both coherent, an MIT license, publishing excludes for `.mise.toml`, `odd/` and `design/`, CI on Linux and macOS, and a release workflow for four targets on a `v*` tag.

Three things worth keeping from this one:

1. **A clean lockfile says nothing about a fresh resolution.** `cargo install --path .` failed with a trait mismatch because the resolver gave ratatui-image its own newest ratatui (0.30) while velo kept 0.29 — the suite was green the whole time, because the lockfile pinned the working pair. Publishing is the one act that ignores your lockfile, so **the install is the test**, and it has to be run, not reasoned about. Pinning the graph so both edges unify (ratatui 0.28, ratatui-image 2) is what makes a published install reproduce; `cargo tree -i ratatui` now shows one version.
2. **A CLI about to be published needs `--version`.** It was missing, and only the install check surfaced it. One attribute, found by using the artifact instead of reading it.
3. **CI is not green: it has never run.** The workflows parse, and saying more than that would be a lie. They become true the first time a human pushes and tags — which is a decision that belongs to the repository's owner, not to this agent.

Scratch left outside the worktree by the delegation: `/tmp/velo-install` and `/tmp/velo-install-2` (install roots, disposable) plus a handful of `mktemp` directories; nothing in the repository.

**L32** (what T10 did *not* do, so the row does not overclaim): the task line says "install path (brew / cargo-binstall)" and only half of that exists. `cargo install --path .`, `cargo install velo` after publishing, and the four prebuilt tarballs are real and verified. Homebrew needs a tap repository and `cargo-binstall` needs release assets named `velo-<target>.tar.gz` with the binary at a known depth — ours carry the tag in the name, which `cargo-binstall` will not find. Both are follow-ups, not promises.

**L33** (T11 closed in `c8a418c`): `ReplayGain` is read from the file's tags with `lofty`, chosen per track (`track`, `album` with a track fallback, or `off`), converted from dB and multiplied into each block **on its way into the ring** — so the callback keeps its single job (copy, ramp, count) and the user's volume ramp behaves exactly as before. Clipping is prevented rather than hoped against: with a peak in the file, the applied gain is capped so `peak × gain ≤ 1.0`, and a value landing exactly on 1.0 is left alone. Without a peak the gain is applied as read, and the module says so.

The proof is a fixture, not arithmetic: a constant-amplitude WAV tagged `-6.02 dB` drains at **half** its amplitude, and the same file with `replay_gain = "off"` drains unchanged, sample for sample.

**The classification worth keeping**: the delegated half arrived as *partial*, honestly, because two call sites that build a player sat outside its edit surfaces. Wiring them exposed the same defect twice — `replay_gain` **and** the config's `volume` were both parsed, validated, stored, and then ignored, because the entry points those two call sites used predated them. The terminal said `vol 100%` with `volume = 70` written in the config file, which is how it was caught. A setting that parses and never arrives is worse than a setting that does not exist: the file claims otherwise, and the user has no way to tell the difference between "velo ignored me" and "velo is broken".

So the check that closes a feature is not "does it parse" but **"does the value reach the code that acts on it"**, and the way to answer that is to look at the artifact: the volume readout after a seek (`vol 70%`, twice — before and after the player is rebuilt), not the test that proves `Settings` holds a 70.

---

**L34 — handoff (session pause, not a closure).** Everything is committed: 42 commits on `main`, working tree clean except `.mise.toml`, which is the human's own and must stay unstaged. 16 suites green, clippy 0, `fmt` clean, `cargo install --path .` measured working.

**Where the project stands.** Formats and gapless playback (T1, T2); a terminal interface (T3) browsing an index built from tags (T4, T5); transport with fade, seek and a clock (T6, fixed twice — the offset counted interleaved samples while the clock counted frames, L23); a library that survives broken files (T19) and mono tracks (T20); a command-line queue that knows its lengths (T21); search over everything (T7); cover art that never fakes itself and now explains when it hides (T8, T8b); a queue you can edit (T18a, T18b); `clap` with a strict config (T9); and ReplayGain applied before the ring (T11). It installs (T10).

**Two things owed to a human, in this order:**
1. **Cover art in a real terminal.** The drawing path is only verified up to "a protocol was detected and the size allows a pane"; no Kitty/Sixel/iTerm2 terminal was ever available here. Step 3 of L33's T8b entry prints the reason in the status line — `no image protocol` or `the window is NxM and cover art needs 100x28` — and that sentence is the whole diagnosis. Report which one appears.
2. **A design review of the interface**, which the human has asked for and postponed until after the functional work. Cover art's placement depends on that conversation, so do not tune the art panel before it.

**Next task, decided by the human's own ordering question.** T17 (bit-perfect: reopen the device per track) is unblocked — `bit_perfect` already exists in the config, resolved and passed through, with nothing reading it yet — and is the smaller of the two remaining substantial tasks. T12 (Subsonic/Navidrome) is the ambitious one and deserves a fresh context and its own design conversation: streaming versus caching, auth, XML versus JSON, and how a remote source coexists with the SQLite index. Do not start T12 by guessing those four answers.

**Environment facts a fresh session must not rediscover:** Rust only through `mise exec -- cargo` (1.99.1 pinned); no remote is configured, so nothing is pushed and CI has never run; `.mise.toml` is the human's uncommitted work; commit style is the repo's Conventional Commits, one work unit each, docs entries recorded per task in this file.

**L35** (T17 done in the clone, not yet merged): bit-perfect means what the key says — the decoder's samples reach the device unmodified, and when a track's rate differs the stream is closed and reopened rather than resampled. The producer owns the stream, so the swap lives there; counters, progress and queue handles carry over, so the clock and the markers never notice the gap. Layout differences still map (mono into stereo is a device constraint, not a DSP choice — L25), and a device with no output at the track's rate converts instead and **says so once** rather than failing the track. Volume pins to 100% and `ReplayGain` is skipped, each announced once, because software volume scales samples and that is exactly what this mode promises not to do.

The proof that matters is equality: a 48 kHz fixture drained with the stream open at 44.1 kHz is `assert_eq!`-identical to the decoder's own output, and the same fixture carrying `REPLAYGAIN_TRACK_GAIN` stays identical — which demonstrates the skipped gain instead of asserting it. The audio callback is byte-identical to the previous commit, checked by extracting the regions and diffing them.

**Slice boundary, recorded rather than hidden.** This landed as one commit of 1128 authored lines, past the ~400-line review slice. It is not laziness about slicing: the mechanism (sink, stream swap, settings threading) and the four behaviours it must not break (no resample, no gain, volume pinned, layout mapped) are one unit, and any split leaves a commit whose own tests cannot run. A reviewer should read it in this order — `needs_stream_reopen` (the whole decision is one line), then `StreamOut::swap_stream`, then the two equality tests, then the settings threading.

**Two things owed before this is real**: the merge into `main`, blocked because the main checkout currently has uncommitted work in `src/ui/mod.rs` — the same file T17 touches — so merging over it would either lose that work or entangle two changes; and the audible check of the rate-boundary gap and the first buffer's fade-in, which no test can hear.

**Tooling note for a future session**: this session could not delegate into a Git worktree — `subagent_run` and `session_worktree_register` both refuse one ("select an existing worktree in the same Git clone") for a nested path and for a sibling path alike. An **independent clone** is the supported route (`repository_root`, with consent), and it is also the cleanest: `git fetch ../velo-clone work/next` merges it back without pretending to be a worktree.

**L36 — the four decisions that bind the Subsonic feature** (the human approved these defaults; recorded here so no future session reopens them):

1. **Playback is cache-on-play.** The original bytes are fetched once with `stream` and `format=raw` — Navidrome returns the original whenever no transcoding profile applies — into `~/.cache/velo/subsonic/<id>`, then played from disk. This keeps seek, gapless, bit-perfect and ReplayGain-from-tags working with **no change to the audio path**. Streaming with HTTP ranges is a later optimisation, not a correctness question: a 1–3 s first play on a LAN is a fair price for not touching the engine.
2. **Authentication prefers an API key**, and falls back to token+salt (`t = md5(password + salt)`, fresh salt per request). The token cannot be stored — it depends on the salt — so the password must be. Secret resolution order: `[subsonic] api_key` in the config, then `$VELO_SUBSONIC_SECRET`, then a one-line `subsonic.key` file with mode 0600; a group- or world-readable file earns a warning naming the path and the mode. The keyring is a later, explicit task, not a promise.
3. **JSON only** (`f=json`). A server that answers XML is reported as too old to speak JSON; parsing XML for servers nobody runs is cost without a user. OpenSubsonic extensions are detected, never assumed: they decide whether API keys and ReplayGain are available.
4. **One index, one search.** Remote tracks become rows in the existing SQLite index with a `source` column (`local` or `subsonic:<id>`) and a stable `remote_id`. That is what lets the fuzzy search, the genre views, the queue and `a` work on remote rows without a second code path. Syncing is explicit (`--sync`, or on open after N hours), never on every start.

**Debt taken deliberately in T12a**: JSON is parsed with `miniserde` instead of `serde_json`, because serde_json's `PartialEq<Value>` makes an existing assertion in `tests/search.rs` ambiguous (`assert_eq!(hits, [])`) and that file was outside the task's surfaces. Migrating to `serde_json` means fixing that assertion first; until then, know that `#[serde(rename = ...)]` is spelled like serde here but is not serde.

**A lesson this entry cost me, so it is written where it will be read**: writing this file with a *quoted* heredoc is mandatory. An unquoted `<<PY` let the shell execute every backticked word in the prose — it ran `stream` (which printed two hundred lines of ImageMagick help into the document) and substituted the rest with emptiness, destroying the section it was supposed to record. The repair was an amend, but the general rule is the one that matters: **when a script writes Markdown containing code spans, the heredoc must not be expandable.**

**L37 — delivery plan for branch `work/next`** (2912 authored lines, two features, four commits). The slice rule exists to protect a reviewer, and the branch now carries two unrelated units: bit-perfect (T17) and the Subsonic client (T12a). They land as **two separate merges**, in that order, each with `--no-ff`, so either can be reverted on its own and a reviewer can read one idea per merge:

1. `git fetch ../velo-clone work/next`, then merge T17: `6771fcc` with its docs `1ee0156`.
2. Then merge T12a: `f6f7ec7` with its docs `db92eaa`.
3. Then merge T12b: `b2eef0b` with its docs commit.

Three units, three merges. The branch is now ~4 500 authored lines across five commits, which is exactly why it must not land as one.

Both wait for the main checkout to have no uncommitted work in `src/ui/mod.rs`. Merging features onto someone else's uncommitted edits is how a branch loses a day.

**L38** (T12b closed in `b2eef0b`): the remote library is rows in the existing index. `source` and `remote_id` with a partial unique index, a migration in one transaction guarded by `user_version`, and queries that did not change their signatures — that is what lets fuzzy search, the artist and album views and `velo --index` reach a server's catalogue with no second code path.

**The migration was the risk, so it was checked against real data, not a fixture.** I copied the human's actual 251-track index, ran the new binary on the copy, and it came back `user_version 2`, 251 rows, every one `source = local`. Fixtures prove the code path; a copy of the real file proves it survives contact with a real file.

**Verified against Navidrome's public demo** (`https://demo.navidrome.org`, `demo`/`demo`) rather than only against canned JSON: `--subsonic-test` answered `navidrome 0.64.2, OpenSubsonic, token + salt — verified` (the demo does **not** advertise `apiKeyAuthentication`, so the fallback is what actually ran); `--sync` brought in 501 tracks and 50 albums in **52 requests** and 9 s, and a second run changed nothing; and searching `sad` in the interface listed `sad robot — pornophonique · 8-bit lagerfeuer`, a remote row found by the fuzzy matcher. The request formula is therefore arithmetic, not a hope: `1 ping + ⌈albums/500⌉ pages + one getAlbum per album`.

**Two findings worth keeping.** First, a partial sync deletes nothing: the transaction is written at the end, so a failure anywhere before it leaves the index exactly as it was — destructive in intent, conservative in behaviour. Second, and this one is a UX gap T12b exposed: with only `[subsonic]` configured, the interface still demands a local `library` path, so a remote-only user is asked for a folder they do not have. That belongs with T12c or the config work; it is written here so it is not discovered twice.

**What the tests pin that my terminal did not show me**: pressing Enter on a remote row (and on a remote search result) writes `velo: remote playback arrives with the cache (T12c)` and starts nothing — two tests assert the exact sentence and pass. My pty capture of the same interaction showed no status line at all, which is the known diff-stream trap of this project: ratatui only writes what changed. The tests are the evidence here; the terminal was the unreliable witness.

**L39 — the three merges landed, and what the conflicts actually taught.** `c26f4bf` (T17), `e3f38f3` (T12a), `d123740` (T12b): one unit per merge, as planned in L37, each tested before the next.

**None of the conflicts were textual.** They happened because both branches had grown *the same abstraction in different shapes*, and a three-way merge produces a hybrid that compiles nowhere:

- T17 was written before the settings layer existed, so the binary took volume, theme and ui apart; main took a `Settings`. The merge left a function signature nobody called correctly until the call sites were joined by hand.
- T12a had written its own `FileConfig` plus a resolved `SubsonicConfig`, while main had made `Config` *be* the file shape. The merge produced a `load_from` that parsed and then referenced a `file` binding that did not exist.
- T12b read `source` and `remote_id` unconditionally, while main had learned that a read-only index cannot be migrated and made `genre` conditional. Merged, `tracks_of` failed on an old file with **no such column: source** — a bug main had already fixed once, reintroduced by a branch that could not see the fix.

**The lesson worth keeping**: when two branches grow the same abstraction, the compiler is the only honest guide through the merge, and its first error is usually several steps downstream of the real seam. Each of the three merges needed a hand-written decision about *which* shape wins for *which* role — `Config` as the file, `Settings` as the resolved state, the library learning both `has_genre` and `has_source` — followed by a full suite run before the next merge started.

**And a test that had quietly stopped covering the truth**: main's schema test asserted `user_version == 1`. After the second migration it had to be 2, and the helper now reports all three facts (version, genre, source) — a test that asserts only the first migration would keep passing while the second one silently failed to run.

Verified **from main** after the merges, not from the branch: a fresh copy of the real 251-track index migrated `1 → 2` with every row `local` and all three columns present; `--subsonic-test` against Navidrome 0.64.2 reported token authentication verified; `--sync` brought 501 tracks and 50 albums in 52 requests.

The clone at `../velo-clone` and the worktree at `../velo-worktrees/next` are now redundant: everything they carried lives on `main`. They can be removed whenever the human says so — nothing here deletes them on its own.

