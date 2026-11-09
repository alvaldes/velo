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

**S5 — The volume knob is a perceptual curve, and bit-perfect skips it
(issue #39).**
The stored and shown volume stays `0..=100` and `[state] volume` keeps its
meaning; only the mapping to amplitude changes. The samples carry
`(percent / 100)³` — one function, `volume_amplitude`, read by the callback's
ramp target — because hearing is roughly logarithmic in amplitude and the
linear knob squeezed everything audible into its last few points. The table
the decision recorded:

| percent | amplitude | dB      |
|--------:|----------:|---------|
|     100 |     1.000 |       0 |
|      75 |     0.422 |    −7.5 |
|      50 |     0.125 |     −18 |
|      25 |     0.016 |     −36 |
|      10 |     0.001 |     −60 |
|       0 |     0     | silence |

The anti-click ramp is untouched: it still fades toward the new amplitude, it
does not jump. With `bit_perfect` the samples are never scaled, so the curve
does not apply — the volume is pinned to 100 there anyway, where the amplitude
is exactly 1.0. ReplayGain, mono and the pause/resume path are untouched: the
curve is the volume knob only.

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

**L2** — issue #39: the volume knob was linear in amplitude, so 100→90 was
about a decibel and 10→0 was twenty — the audible range lived in the last few
points. Decided with the author: amplitude becomes `(percent / 100)³`, one
function (`volume_amplitude` in `src/audio/output.rs`, table under S5),
applied at the callback's ramp target — the only place the percentage becomes
an amplitude; there is no separate fade-target computation. The stored and
shown number stays 0–100, the anti-click ramp keeps its shape, and
bit-perfect is untouched (its samples are never scaled; the volume is pinned
to 100, whose amplitude is exactly 1.0). Pinned by `tests/volume_curve.rs`.
