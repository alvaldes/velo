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
| T1 | Audio spike: decode with symphonia, output through cpal, lock-free ring buffer, gapless join between two tracks | S5, S7, S16 | inline | — |
| T2 | Replace the spike's resampling with rubato; audible verification of the join | S7 | inline | — |
| T3 | ratatui skeleton: layout, keybindings, themes | S1 | inline | — |
| T4 | Library scan: jwalk + lofty + rusqlite schema, artist/album/track browser | S6 | inline | — |
| T5 | Play from the library and manage the queue | S11 | inline | — |
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

## Log

**L1** (original request, verbatim):
> Tengo este repo que este es un reproductor de música. El problema es que quiero crear el mío propio, utilizando este, digamos, como una plantilla. Quizás antes de iniciar pensar en que voy a utilizarlo como una herramienta de terminal, entonces quizás no sea la mejor opción. Analizar primero qué otras opciones hay en el mercado, quizás Rust he visto que son bastante rápidas las aplicaciones creadas y se pueden usar en terminal

**L2** (decisions from the questionnaire, verbatim option labels): Product — "Reproductor de mi biblioteca"; Stack — "Rust desde cero, repo nuevo"; Objective — "Publicarlo y que otros lo usen".

**L3** (scope, verbatim):
> el alcance del de arriba pero l o que hace subsonic/navidrom y replaygain tambien hay que hacer no importa que sea en otra versión

Interpretation: S1–S13 and S16–S17 are v1; S14 and S15 are required by the project but may ship in later versions.

**L4** (environment finding): `cargo` and `rustc` are absent from this machine; no `~/.cargo`, no rustup toolchains, no Homebrew rust formula. Rust must be installed before any task can be built or verified. `mise` 2026.11.2 is present and already manages Node on this machine.

**L5** (toolchain): installed as `mise use -g rust@stable` → rustc 1.99.1, cargo 1.99.1, clippy, rustfmt, in `/Users/alvaldes/.local/share/mise/installs/rust/`. Pinned in the repo by `.mise.toml` (`rust = "1.99.1"`) and `mise trust`.

**L6** (dependency findings): `symphonia` 0.6.1 renamed its public API (`codecs::audio::AudioDecoder`, generic audio buffers) and, like 0.5.4, ships **no Opus decoder**. The spike pins `symphonia@0.5` so the risk stays in the gapless join rather than in API archaeology; T15 handles the migration. Resolved versions: `cpal` 0.18.2, `rtrb` 0.4.1, `symphonia` 0.5.x.

**L7** (functional verification, T1): decoding works for FLAC, MP3, OGG Vorbis and WAV. Two real bugs surfaced and were fixed or tracked: (a) **M4A/AAC failed at open** because the container declares no channel layout until the decoder parses the first packet — `TrackSource::open` now decodes the first block and uses it as `pending`, with the spec taken from the decoder instead of the container (`tests/formats.rs`); (b) **AAC priming is not trimmed** by symphonia 0.5 for isomp4 (an MP3 of the same 2.00s trims to exactly 88 200 frames; the M4A decodes ~1 024 frames long), so AAC joins are not yet gapless — tracked as T16. Opus is rejected at probe time, as expected until T14.
