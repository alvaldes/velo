# update — know a newer velo exists

Issue #69. Slice 1 only: on launch, honouring `[update] check` (`never` |
`daily` | `weekly`, default `daily`), ask GitHub's releases/latest for the
newest tag and compare it with the running version. Say it once: one status
line, and a `version` row in the settings card. Off the event loop. Silent on
failure. Nothing stored beyond a timestamp of the last check. Slice 2
(replacing velo's own binary) is explicitly out of scope: nothing is
downloaded, verified or executed.

## Specs

**S1 — The check.** `[update] check = "never" | "daily" | "weekly"`, default
`daily`. A due check is one GET of
`https://api.github.com/repos/alvaldes/velo/releases/latest` on its own
thread, through the radio client's `Transport` seam (no new dependency). Due
means: `never` never; no stored timestamp (fresh install) at once; otherwise
a whole interval old — 24 h daily, 7 days weekly — over an injected clock.
No config file, no check (nowhere to store the timestamp).

**S2 — The comparison.** Pure over injected values
(`check_due`, `newer_version`, `version_triple` in `src/update.rs`). Tags
with or without a leading `v`, compared as a semver triple; only a strictly
newer fetched version reports. A non-semver tag answers no without
panicking; never a downgrade.

**S3 — The saying.** Once: one status line (`velo <newer> is available — you
have <running>`) and a read-only `version` row in the settings card, under a
new `about` header, naming the running version and the newer one when found.
Silent on failure: no error line, no retry storm — the only trace is
`[update] last_check` (unix seconds), written whatever the answer was.

**S4 — Unknown `check` value.** The file's convention for an enum-valued
key, the same strictness `replay_gain` is read with: serde's own
unknown-variant error (names the refused value and every allowed one), the
load fails with file and line.

## Log

**Slice 1 (this change).** New `src/update.rs` (`CheckMode`, `check_due`,
`version_triple`, `newer_version`, `display_version`, `fetch_latest_tag`,
`UpdateCheck::spawn`/`poll`/`loopback`); `[update]` table in `src/config.rs`
(`UpdateFile`, `Settings.update_check`/`update_last_check`, documented in
`default_config_text` and the help text); launch wiring in
`src/bin/velo.rs` (`start_update_check`, both interfaces, before the config
path moves into the app); settings card in `src/ui/mod.rs` (`SettingsKey::
Version`, 18 rows, `update_tick` in the tick, `version_setting_value`,
`set_update_check`); README `[update]` section and settings-card key row.

Existing assertions changed, both because the `version` row now ends the
card: `the_settings_cursor_moves_between_settings_only_and_stops_at_the_ends`
(G/j now stop on `version`; folder reachability pinned with a `k` step),
`enter_on_the_folder_row_opens_the_library_prompt` (G then `k` to folder),
and `the_settings_card_has_one_theme_row_and_no_colour_rows` (row list gains
`version`). No test weakened.

Verification, all exit 0: `cargo fmt`; `cargo clippy --all-targets` (two
pre-existing `too_many_lines` warnings at HEAD remain — `App::browse` 102,
`default_config_text` 119 lines — none new); `cargo test --lib update::` (8
passed); `cargo test --test config --test ui_state --test ui_render --test
cli` (32 + 138 + 59 + 107 passed). Known risk: the check's thread writes
`[update] last_check` with `set_value` (read–modify–write) concurrently with
the interface's own config writes; a collision could lose one write —
accepted for slice 1, one write per interval at most.

## The tick, not the thread, writes `last_check` (issue #69, slice 1 fix)

The slice-1 note above closed with the exact risk this entry closes: the
check's worker thread wrote `[update] last_check` with `set_value`, a
read–modify–write of `config.toml` racing the interface's own writes — a
lost write could be a user's setting, not just the timestamp.

Now no thread writes the config. `UpdateCheck::spawn` lost its
`config_file` argument and every config write: the thread fetches, decides
the `CheckOutcome`, sends it, done. The writer is the loop side: `App::
update_tick` records the timestamp through the same `remember` seam every
user setting uses, on the same thread, whatever the outcome was — a failed
check still records (anti-storm: a refused request waits a full interval).
The recording happens before the status line, so a failed write cannot
mask the newer-version news. If the process exits before the tick polls,
nothing is recorded — accepted, harmless; the next launch checks once.

Tests (RED first, both recording tests observed failing on the old code),
in `tests/ui_state.rs`:
`the_tick_records_the_last_check_when_the_outcome_arrives` (loopback Tag:
timestamp recorded, status still says the version, a later tick adds no
second write), `the_tick_records_the_last_check_even_when_the_check_failed`
(loopback Failed: timestamp recorded, status silent),
`the_tick_writes_nothing_while_the_outcome_has_not_arrived` (loopback with
no send: the config file stays byte for byte). No existing assertion
changed; `the_tick_says_a_newer_version_once_and_a_failure_never` runs with
no config file and still passes as it was.

Verification, all exit 0: `cargo fmt`; `cargo test --test config --test
ui_state --test cli --test ui_render --lib` (32 + 141 + 107 + 59 + 89
passed; config + ui_state + cli + ui_render + lib); `cargo clippy
--all-targets` (the same two pre-existing
`too_many_lines` warnings — `App::browse`, `default_config_text` — none
new). Risk: the timestamp is recorded at consumption time, not ask time —
a check that takes long shifts the interval that far; a whole-interval
granularity makes this immaterial.

## The check you ask for (issue #69: `velo update` and the version row's enter; slice 2 still out of scope)

The author's report: `velo update` did not exist, and the card had no way to
ask. New, still with no download and no self-replacement: the `update`
subcommand (`Command::Update` in `src/config.rs`, dispatched beside the
one-shot modes in `src/bin/velo.rs`) runs `manual_check` at once — ignoring
`[update] check` and `last_check` — and prints the running version, the
newest tag, and one of two verdicts; exit 0 for either verdict, 1 with one
`velo:` line on stderr when the request or the answer failed. "How to get
it" is honest about the install: `install_route` is a pure function of the
executable's own path — `/opt/homebrew/` or `/usr/local/Cellar/` says
`brew upgrade velo`, anything else names the release archive. In the
interface, `enter` on the `version` row starts the same check now
(`App::check_for_update_now`, the card's only check key), and its answer
lands through the same tick the automatic one uses: the status line, the
row, and `[update] last_check` recorded by the loop side — never by a
worker thread. A check already on its way is not replaced. To carry the
failure the manual command says, `fetch_latest_tag` now delegates to
`fetch_latest_reported`; its silence is unchanged and pinned by a test.

Tests: `tests/update.rs` (new; 12) — the install route (Homebrew prefixes,
arbitrary paths), the advice per route, the verdict text (newer × both
routes, equal, a running build ahead of the releases), `manual_check` over
the injected transport (one request to `RELEASES_URL`, success, failing
transport one-line error, non-JSON body), and `fetch_latest_tag`'s silence
preserved. `tests/ui_state.rs` (+2): `enter` on the version row checks now
even with `check = "never"` and a fresh `last_check` — asked, status, row,
refreshed `last_check`, transport injected so no test touches the network —
and no key outside the version row's `enter` asks. `tests/cli.rs` (+1):
`velo update` parses alone. README's `[update]` section documents both
routes in.

Existing tests changed: the `Command` import alias in `tests/cli.rs`
(`HistoryCommand` → `CliCommand`) reads honestly now that the enum carries
two subcommands; no assertion weakened.

Verification, all exit 0: `cargo fmt`; `./scripts/check.sh` (fmt --check,
clippy -D warnings, the whole test suite). Not testable without a network:
the real `velo update` binary end to end — its transport is built inside
the process, and tests must not reach the network — so the failure path is
proven one level down, over the injected transport seam.
