# audio-device — the output device picker (`d`, issue #44)

The output device is whatever was picked at launch, and velo never says what
it picked or lets you move. `d` lists the machine's audio outputs, marks the
one in use, `enter` moves the playback there without losing the position, and
the choice is remembered in `config.toml`.

Work happens on `feat/device-picker`, in the velo-device worktree. This
logbook and the files it names are ours; everything else belongs to whoever
is already editing it.

## Specs

**S1 — `d` opens a picker listing the machine's audio outputs.**
cpal's output devices, one row each, duplicates by name dropped (two
same-named entries open the same first match), the one in use marked with the
live dot the radio's footer uses. `j`/`k`/arrows move and wrap at the ends,
`enter` switches, `esc` closes without changing anything. A machine with one
device still lists it; a machine with none says so in one status line instead
of opening an empty card.

**S2 — `enter` moves the playback and keeps the position.**
A playing stream is rebuilt at the position it reached — the same rebuild a
seek does (`start_track_at`'s path), so a file reopens at the offset the old
stream had reached and a live station reopens from the current moment, never
from its start. Every later rebuild of the session (seek, skip, new stream)
stays on the chosen device.

**S3 — the choice is remembered as `[audio] device = "<name>"`.**
Written through the surgical `set_value` the other keys use, and only when it
changed — the way shuffle and the visualizer write only on a change. A
launch with a configured device opens it; a name that is no longer present
falls back to the default device and says so once through the player's
notices, never refusing to start. A blank value is no device, the way a
blank theme name is no theme.

**S4 — a device that cannot be opened leaves the current output playing.**
The switch opens the new player *before* the old one is stopped, so a failed
open costs one status line naming the error and never the music.

**S5 — `bit_perfect` keeps its meaning.**
The switch rebuilds the player through the same `play_gapless_at_full`, so
the chosen device is the one reopened at each track's own rate, the
StreamOut producer keeps the chosen device for the mid-sequence reopens, and
a device that cannot take a rate falls back to converting with the same
one-line notice as at launch.

**S6 — nothing else changes.**
The queue, the radio, the history, the playlists, the visualizers, the other
settings and the other keys are untouched. `d` was unbound in the plain
keymap (keymap T2) and `ctrl+d` keeps its paging.

## Tasks

| ID | Spec | Route | Task | Commit |
|:--|:--|:--|:--|:--|
| T1 | S1, S3, S4 | worker | device list + `open_target` resolution in `src/audio/output.rs`, `[audio]` table in `src/config.rs`, picker + switch in `src/ui/mod.rs`, both launch paths in `src/bin/velo.rs`, tests | this delivery |
| T2 | S2 | worker | position-keeping switch (rebuild at offset) — landed inside T1 | this delivery |
| T3 | docs | parent | README `d` row, `[audio]` table and audio prose — **blocked: README.md is empty at the base commit** (see L4) | |

## Log

**L1** — user, in issue #44: "The output device is whatever was picked at
launch." The report that sent the worker here adds: implement S1–S4 with
tests, the list/mark/selection/refusal/config-write testable over an
injected device list, and that sound actually comes out of the chosen hole
is not testable — the author's ears are the evidence, recorded here as
**pending**.

**L2** — the seam is a function field on `App`, `device_lister:
Box<dyn Fn() -> DeviceList>`, installed with `set_device_lister` — the same
shape as `set_url_opener`, which no test lets touch a browser. `DeviceList`
(names + default) and the enumeration (`output_devices`) live in
`src/audio/output.rs`; cpal 0.18 has no `DeviceTrait::name()` any more, the
device's own `Display` is its name.

**L3** — the resolution the launch performs is pure and unit-tested:
`open_target(requested, &DeviceList)` answers the requested name when the
machine has it, else the default with the once-notice "audio device "X" is
not present — using the default output", else an error that names the
device. The answer borrows from the list, never from the request.

**L4** — **README is blocked, and it is not ours to fix alone.** The brief
says to add the `d` row to the keys table, the `[audio]` table and a
sentence of audio prose — but `README.md` is empty (0 bytes) at the base
commit of this branch: commit `ed8e78d` ("feat(ui): `ctrl+i` says what the
playing file is", already on `origin/main`) deleted all 790 lines of the
README alongside its feature, which the commit message does not mention.
Restoring 790 lines is a decision about someone else's accident, so the
worker stopped and asked instead of guessing; T3 waits for the answer.

**L5** — the switch reuses the seek's rebuild instead of touching the
producer: `switch_audio_device` opens a candidate `Player` at
`(current_index, position)` on the chosen device while the old one still
plays, stops the old one only on success, and shows the error otherwise
(S4). The Limits clause's condition held: keeping the position needed no
change in the producer bigger than reopening at an offset — the offset path
already existed. A station is refetched (the preopen is a player property,
not a session one); that *is* the spec's "reopens from the current moment".

**L6** — `enter` on the device already in use closes the picker and says
`output: <name>` but rebuilds nothing and writes nothing: a gap for nothing
is not a switch. The theme picker's precedent is "write only when changed";
here the rebuild itself is what is skipped, because the cost is audible.

**L7** — test evidence (worker session): `cargo test --test audio_device`
12 passed, 0 failed; `cargo test --lib audio::output` 10 passed, 0 failed
(after one correction: the error assertion now matches `AudioError`'s own
"audio device: " prefix exactly as a status line prints it). Pending on the
author's ears: that the chosen device actually sounds, and that a switch
keeps the music unbroken enough to be pleasant — neither is testable
headless, and no test builds a `Player`.
