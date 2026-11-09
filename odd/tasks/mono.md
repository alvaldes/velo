# mono — a downmix to mono (issue #8)

Both output channels carry the whole mix: the average of the channels, reachable
by `M` and by `--mono` for a session. The human's words: "Mono después de
ReplayGain; con bit-perfect se rechaza".

## Specs

**S1 — The downmix is a step in the sample path.**
It happens AFTER ReplayGain is applied and BEFORE the samples reach the ring.
Both output channels carry the whole mix — average the channels
(`(left + right) / 2`, never a sum, so the peak does not grow). The device stays
stereo: this is mono content in a stereo stream, not a mono device.

**S2 — How it is reached.**
`--mono` starts a session in mono; the `M` key toggles it while playing; nothing
is persisted anywhere (no config key).

**S3 — Refused under bit-perfect, never applied silently.**
With `bit_perfect` on, mono is refused explicitly. At startup `velo --mono` with
`bit_perfect = true` in the config prints one line on stderr
(`velo: --mono cannot be combined with bit_perfect`) and exits 1 without
playing; pressing `M` while bit-perfect is on leaves mono off and says the same
thing in the status line.

**S4 — The toggle reaches the producer without restarting the stream.**
The audio path already shares cells with the UI for pause and volume; mono
follows that shape: a shared cell the producer reads at every block.

## Slices

One commit, and it is honestly one step: `play_gapless_at_full` grew the `mono`
argument every caller must name, so the downmix, its tests and the interface
that turns it on cannot land separately without leaving the tree uncompilable.
The line count is larger than the usual slice because five new tests and the
logbook ride with it.

## Tasks

| ID | Spec | Route | Task |
|:--|:--|:--|:--|
| T1 | S1, S4 | tests + `src/audio/output.rs` | `MonoCell`, `mono_mix` (the mean per frame), the reads in the file and station sample paths after `gain.apply`, and `drain_headless_with_mono` as the headless proof |
| T2 | S1, S4 | tests + `src/audio/output.rs` | `Player::set_mono`/`mono`, `mono` on `play_gapless_at_full`, the cell shared into `Shared` |
| T3 | S2, S3 | tests + `src/config.rs` + `src/bin/velo.rs` | `--mono` on `Cli`, `Settings.mono` (no config key), the startup refusal (one stderr line, exit 1), `--play` and both interface launches carrying it |
| T4 | S2, S3 | tests + `src/ui/mod.rs` | `Action::ToggleMono`, the `M` key, `toggle_mono` with the bit-perfect refusal in the status line, `set_mono`/`mono`, the help row |
| T5 | — | docs | README keys table, audio prose and exit codes |

## Log

**L1** — the issue's own words: "A downmix to mono, reachable by a key and by a
flag (`--mono`) for a session, so both channels carry the whole mix. It is a step
in the sample path before the ring, and it should say what it does to ReplayGain
and to bit-perfect playback rather than quietly changing both."
