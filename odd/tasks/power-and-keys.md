# power-and-keys — low power and the OS's media keys

Two features that share the event loop, delivered together: a low-power
mode asked for by a flag and remembered by a key, and velo registered as
a now-playing client so the OS's media keys reach the actions the
keyboard already performs. Work happens on `feat/low-power-media-keys`,
in this worktree.

## Specs

**S1 — `--low-power` starts a session in low-power mode; `[ui]
low_power = true` remembers it; the `P` key toggles it live and writes
the key, the way `s` writes `shuffle`.**
The flag and the config key are one session switch the interface reads:
either source turns it on — the flag is a request, never a refusal of
the file's choice — and `P` writes `[ui] low_power` once per change,
against what the file already said, exactly the contract
`[state] shuffle` keeps.

**S2 — low power lowers the redraw cadence and leaves the spectrum out
entirely.**
The loop already asks the app for an interval; low power draws at four
times whatever the ordinary rate would be (400 ms instead of 100 ms,
132 ms instead of 33 ms while bars were animating). The spectrum tier is
not drawn: the now-playing block keeps its seven ordinary rows and its
rows go back to the list. The tap stays drained every tick, shown or
not, so returning never plays half a second of old audio.

**S3 — nothing else changes.**
Playback, the clock, the volume, the seek and every other key behave
exactly as they do today; the screen keeps the same layout with the
spectrum row gone. The settings card gains no row and the keymap moves
nothing: `P` is a new global key, and everything it does is behind one
`Action::ToggleLowPower` arm.

**S4 — `souvlaki` (approved by the human) registers velo as a
now-playing client.**
Play/pause, next and previous reach the same actions the keys already
perform: one mapping, `command_of` then `action_of`, lands each OS event
on `Action::TogglePause`, `Action::Next` or `Action::Previous` through
the same `apply` the keyboard uses. Every other media event — seek,
volume, quit — is left for the OS to forget.

**S5 — publish what is playing.**
A track publishes title and artist (title from the index's tags, else
the file name; no artist is invented), a live stream publishes the
station's own name — or the ICY title it is announcing when there is
one — and nothing playing publishes nothing, which the OS shows as
stopped. A tick that finds the same answer sends nothing.

**S6 — never hurt a launch that cannot have it.**
Starting the client is best effort: one status line at most, never a
panic, never a wait. The client lives on its own thread; the tick drains
its command channel and publishes state across another, so no network
and no polling ever run inside the loop. Headless, no session bus, a
client that refuses: the session starts all the same, silently or with
one line.

**S7 — no key changes.**
The media keys are the OS's, not the terminal's: the keymap is untouched
and no terminal key was added, moved or removed. `P` belongs to S1, not
to this feature's key surface.

## Tasks

| ID | Spec | Route | Task |
|:--|:--|:--|:--|
| T1 | S1, S2, S3 | worker | `--low-power` on `Cli`, `[ui] low_power` in the config, `P` toggling and writing, `cadence` ×4, the spectrum tier left out |
| T2 | S4, S5, S6, S7 | worker | `src/media_keys.rs`: souvlaki client on its own thread, command/publish channels, best-effort start |
| T3 | S4, S6 | parent | both launch paths spawn the client; the tick drains commands and publishes |
| T4 | S1–S7 | worker | README (flag, `P` row, media-keys note, `[ui] low_power`) and this logbook |

## Log

**L1** — the issues, in their own words. #11: "A low-power mode, asked
for by a flag and remembered by a key, that lowers the redraw cadence
and leaves the spectrum out. Playback unchanged, clock unchanged,
everything else on screen unchanged." #12: "Register as a now-playing
client and map play/pause, next and previous onto the actions the keys
already perform, publishing what is playing (title and artist, or the
station's name) so the OS can show it."

**L2** — souvlaki 0.8.3 builds on this machine (macOS, arm64). It pulled
in its macOS backend's stack — `objc`, `objc-foundation`, `objc_id`,
`cocoa`, `core-graphics`, `block`, `dispatch`, `once_cell` — and cargo
reports a *future-incompatibility* warning for `block v0.1.6` and
`objc`-era crates owned by souvlaki, not by velo: a note that a future
Rust will reject their code, not an error today. Everything velo-side
compiles warning-free under `-D warnings`.

**L3** — the pause a media key asks for is, headlessly, a no-op: no
player, nothing to pause. That is the space key's own answer with no
player, and the test pins the parity instead of inventing a fixture
that would lie: the command routes to `Action::TogglePause` (asserted
the way the keymap tests assert) and an action that cannot act publishes
nothing new.

**L4** — the help card gained one row (`P`), placed at the end on
purpose: the card's first screenful is pinned by tests that read `u` and
`q` on a small window, and a row added above them scrolls them off. The
README's keys table keeps `P` beside `M`, where it belongs.

**L5** — the media client is created on its own thread, never on the
main thread, and its handle's join is dropped unread (detached): the
thread serves the session for the interface's length and ends when the
channels close. A failed `MediaControls::new` or `attach` says one line
through the init channel; the thread then simply ends, and the session
never learns anything more.

**L6** — `ctrl+z` suspends velo the way a shell's job control does
(issue #66). The keymap gained `Action::Suspend`: a `ctrl+z` arm sits in
the ctrl block, and the plain `z` shuffle arm now refuses CONTROL, so the
modifier is never swallowed (issue #38's `ctrl` pairs stay apart from the
plain arms). The event loop answers `Suspend` by taking the terminal
guard down in the order `with_terminal` put it up — raw mode off,
alternate screen left, cursor shown — then `rustix::process::kill_process`
with `SIGTSTP` through the safe API (`rustix = { version = "0.38",
features = ["process"] }`, resolved by the lock's existing 0.38.44; the
crate keeps `unsafe_code = "forbid"`). That call does not return until
the shell resumes the process, so the guard goes back up straight after
it in the same function, ending with a full-frame repaint via
`terminal.clear()`. Raw mode is off before the stop, so a failed stop
never wedges the terminal: the error propagates and `with_terminal`
restores again on the way out. A suspended process runs no threads, so
the audio stream stops by itself; there is no pause/resume logic here —
that is the daemon's problem, and velo has none. What is deterministic
is tested (`ctrl_z_suspends_instead_of_toggling_shuffle`,
`bare_z_still_toggles_shuffle` in `tests/ui_state.rs`); the terminal
itself is not testable here, and the doc comment on
`suspend_and_resume` says so plainly. Verified: `cargo fmt`,
`cargo test --test ui_state --test cli --test ui_render` (143 + 32 + 63
passing), `cargo clippy --all-targets` — no warnings from this change;
the remaining ones are pre-existing (clippy 1.99 lints on untouched
code, souvlaki's `block` future-incompat from L2).
