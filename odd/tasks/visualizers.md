# velo — the visualizers

Issue: https://github.com/alvaldes/velo/issues/40 — the spectrum's band area
can take nine shapes: the bars it has always drawn, and eight more, chosen
with keys, a picker or the config file. The author:

> yo lo que quiero es que tambien se pueda tener esto y elejir entre
> diferentes formas de spectrum (recuerda usar sus keys, ctrl+v, v,..., V)

## Specs

**S1 — eight visualizers with genuinely different shapes, plus `none`.**
`bars` (filled bars per band), `bars-outline` (the same as an outline),
`wave` (one line undulating with the envelope), `scatter` (a dot per band at
its height), `flame` (each band's peak falling back over time), `matrix`
(columns raining down with history), `scope` (the raw waveform), `mirror`
(bars mirrored from the centre). Each draws into the same band area the
spectrum uses today; `none` draws nothing and gives the rows back, exactly
as the low-power mode already does.

**S2 — no random source anywhere.** Falling peaks and rain are driven by
elapsed time and the band values, so the same input produces the same frame.
Every visualizer is a pure function of (bands, waveform window, elapsed
time, area) so it can be tested without a terminal.

**S3 — the keys.** `v` cycles to the next visualizer (wrapping, `none`
included); `Ctrl+V` opens a picker overlay that lists `none` and the eight
in a stable order, applies the focused one immediately so the preview is
the interface, `enter` keeps it and writes the choice, `esc` restores what
was in force. Built on the theme picker's shape and key handling.

**S4 — `V` takes the visualizer to the whole body** (no list, no
now-playing block), `V` or `esc` returns to exactly the view it came from,
and the transport keys keep working while it is full screen: `space`, the
arrows, `n`/`p`, the volume and `q` are not swallowed. The header and the
footer stay.

**S5 — the choice is remembered as `[ui] visualizer = "<name>"`.** The old
`[ui] spectrum = false` still means `none`: when the new key is absent, a
`spectrum = false` in the file resolves to `none` and `spectrum = true` to
`bars`. The settings card's spectrum row reflects the new setting.

**S6 — nothing else changes.** Playback, the queue, the radio, the lists,
the layout of the band when it is in place, and the low-power rule that
leaves the visualizer out (low power still wins over the visualizer: it
draws nothing).

## Tasks

| ID | Spec | Route | Task | Commit |
|:--|:--|:--|:--|:--|
| T1 | S1, S2 | worker | `src/ui/visualizer.rs`: the nine shapes as pure functions of (bands, waveform, elapsed, area); the waveform window kept from the tap in `src/ui/mod.rs` | |
| T2 | S3, S5 | worker | `src/ui/mod.rs`: `v`, `Ctrl+V` and the picker (live preview on move, write-on-change on `enter`, restore on `esc`); `src/config.rs`: `[ui] visualizer` and the legacy `spectrum` resolution; the settings card's spectrum row | |
| T3 | S4, S6 | worker | `V` full screen over the body, `V`/`esc` back to the view it came from, transport keys untouched; low power still wins; tests, README, this logbook | |

## Log

**L1** — the author's own words: "yo lo que quiero es que tambien se pueda
tener esto y elejir entre diferentes formas de spectrum (recuerda usar sus
keys, ctrl+v, v,..., V)".

**L2** (writer, T1–T3, S1–S6). All six specs landed in one pass. The nine
shapes live in `src/ui/visualizer.rs` behind `Visualizer`, each `draw` a
pure function returning one line per row of its area; the tap's newest
1024 samples are kept beside the analyzer's bands so `scope` has a
waveform to read, and `flame`/`matrix` read the session's elapsed time
(`started: Instant`) — no random source, pinned by a determinism test.
`v` cycles and writes, `Ctrl+V` opens the picker on the theme picker's
shape (preview applies on every move and writes nothing, `enter` writes
`[ui] visualizer` once, `esc` restores through a saved committed value the
way `theme_name` works for themes), and `V` remembers the view it came
from so `V`/`esc` land back exactly there while every key but those two
falls through — `space`, the arrows, `n`/`p`, the volume and `q` all
observed working full screen.

Three decisions beyond the brief, recorded here:

- **Silence draws nothing for every visualizer, `bars` included.** The
  spectrum's quiet `▁` baseline on a silent band among loud ones stays
  (pinned by the inline `spectrum_bars_fill…` test), but full silence —
  every band at zero — renders nothing, as the brief's test rule asks.
  Three existing ui_render tests asserted the baseline of a session with
  nothing playing; they were rewritten around the same properties (the
  band sits between the clock and the progress bar; low power gives the
  rows back) without the baseline.
- **`UiParts.spectrum` (the runtime bool) is gone.** With the visualizer
  row deciding both the shape and the rows, the bool was either dead or
  contradictory (a file saying `spectrum = false` alongside
  `visualizer = "scatter"` would have hidden the scatter). The `[ui]
  spectrum` *file key* stays: absent, it still resolves `none`/`bars` per
  S5; present, a `visualizer` key wins. The settings card's spectrum row
  now shows the visualizer's name, steps it with the arrows, and opens
  the picker on `enter`, exactly the theme row's pattern.
- **An unknown `visualizer` name in the file falls back to `bars`**, not
  a config error — the choice between refusing the whole file and drawing
  something went to drawing.

The default config file was left untouched: the generated `[ui]` still
documents `spectrum = true`, and a `visualizer` line appears only when a
choice is actually made, the way `[theme] name` does.
