# radio — internet radio sources for velo

Third audio source beside the local library and Subsonic: a public station
directory (Radio Browser) plus live stream playback with ICY now-playing.
Plan and evidence: `~/Documents/second-brain/00 - Inbox/velo - radio.md`
(10 decisions resolved 2026-11-07, all in favour of the recommended option).

Work happens on `feat/radio`, in this worktree. `src/radio/**` and this logbook
are ours; `src/audio/**`, `src/subsonic/**`, `src/library/**` and `src/ui/mod.rs`
belong to whoever is already editing them (see L4).

## Specs

**S1 — The directory is Radio Browser, queried as CLIAMP queries it.**
Base `https://de1.api.radio-browser.info/json`; `User-Agent` on every request.
`/stations/search` with `limit`, `offset`, `order`, `reverse`,
`hidebroken=true`, `name`, `tag` + `tagExact=true`, `countrycode`, `state` +
`stateExact=true`. Orders: `votes`, `clickcount`, `clicktrend`, `name`,
`random` — `reverse=true` for every order except `name` and `random`. Indexes:
`/countries` (`iso_3166_1`, `stationcount`), `/states/<country>/`,
`/tags?order=stationcount&reverse=true&hidebroken=true&limit=100000`.
Playback URL is `url_resolved`, never `url`. At most 200 stations load per
place, replacing the queue.

**S2 — A live station plays as a stream, never as a download.**
The producer thread opens the `http(s)` body and decodes as bytes arrive; no
disk cache. A station has no duration and no seek. Cut, stall or 30 s of silence
is one retry after 2 s, then the queue advances and the reason shows in the
status line.

**S3 — Live is a property of the entry, not of the queue.**
`next`/`prev` over a station is the next/previous **station**. Repeat does not
repeat a station. Gapless does not apply. ReplayGain and bit-perfect are skipped.
Seek is a no-op and progress shows `live`. *(Revised in #17, after the human's
decision — L29: the queue keeps stations, and `next` over a station is no longer
the next station but the first track of the queue.)*

**S4 — The station's now-playing comes from ICY.**
Request `Icy-MetaData: 1`; the server then sends `Icy-Metaint: N`. The stream is
`N` audio bytes, then one length byte `L`, then `L × 16` metadata bytes (zero
when nothing changed). The payload is `StreamTitle='Artist - Title';`. Same title
repeated block after block must not repaint. No `Icy-Metaint` means pure
passthrough.

**S5 — Station preferences live in their own file.**
`~/.config/velo/radio.toml`: favourites, pinned countries/regions and hand-added
stations, deduplicated by station URL, written whole and atomically (write +
rename). Never rewritten after a failed read. `config.toml` is untouched:
`set_value`/`remove_value` edit it line by line and cannot carry arrays of tables.
Country is opt-in and read from the **locale alone** — `LC_ALL`, then
`LC_MESSAGES`, then `LANG` — with no geo-IP and no timezone: a timezone gives a
city and would need an untested timezone→country table, so it is not consulted.
Stored as `country = "NO"` or `country = "none"`; when nothing yields a country
there is no "near you" row and nothing else in the feature is gated on it.

**S6 — A station's identity is the URL the directory listed.**
Normalised only in scheme case, host case, default port and trailing slash.
Unwrapping a wrapper playlist is a playback step and never changes identity.
Stations are **not** in the SQLite index: never in browse, search, `sync` or
`artwork`; no `source='radio'` rows.

**S7 — A station URL may be a wrapper playlist.**
`.pls` (`[playlist]`, `File<n>=`) and `.m3u` (`#EXTM3U`, `#EXTINF:<secs>,<title>`,
URL line) resolve to their first playable entry. A wrapper that yields nothing
playable is an error naming the file, not `unsupported format`.

**S8 — Undecodable stations are filtered before they reach the queue.**
`codec` from the directory decides. The accepted set comes from the features
compiled into symphonia, never a hardcoded list, because T14 may enable `opus`.
A spelling the table does not recognise (blank, `unknown`) is **allowed**: the
directory omits or fudges it often enough that hiding those stations would throw
away playable ones, and a failure that names the codec is the better outcome.

**S9 — The radio view follows the sketched layout.**
`R` opens/closes it, `/` searches the directory by station name (any country,
issued on `enter`, never a filter of what is loaded), `F` filters the focused
list already loaded, `enter` enters a country or tag and plays from the
selected row, `a` queues without leaving, `f` favourites a station or pins a
country, `N` opens the country list, `Ctrl+R` refreshes the directory, `tab`
alternates radio and queue, `1`–`4` stay library views. UI copy is English.
Footer shows `● live` instead of a position and total. *(Revised in #15: `/`
used to filter the loaded list, which made one key mean two things and left a
name search unreachable.)*

## Tasks

| ID | Spec | Route | Task | Commit |
|:--|:--|:--|:--|:--|
| T1 | S4 | worker | `src/radio/icy.rs`: ICY length-prefixed demux over a `Read`, `StreamTitle` extraction, passthrough when no metaint | `b9bb2c2` |
| T2 | S7 | worker | `src/radio/lists.rs`: PLS and M3U parse to the first playable entry | `f804a64` |
| T3 | S1, S8 | worker | `src/radio/{client,directory}.rs`: Radio Browser client, indexes, station search, codec filter from symphonia features | `a0f946a` |
| T4 | S5 | worker | `src/radio/store.rs`: atomic `radio.toml` for favourites, pins and hand-added stations | `654f348` |
| T5 | S2, S3 | parent | stream engine: `MediaSource` over the HTTP body, queue entry `File \| Station`, live semantics; **blocked on the other session's T15 (symphonia 0.6)** | || T6a | S9 | worker | `src/ui/radio.rs`: the view's state machine, sections, fuzzy filter and order card, hooked with 41 additive lines in `src/ui/mod.rs` | `9e3e835` |
| T6b | S9 | worker | wiring: `RadioStore` load, `country::detect`, directory fetch for countries/tags/stations, refresh, catalog handoff | `a2b4b78` |
| T6c | S9, S3 | parent | playing a station and the `● live` footer; **needs T5**; also README `## Keys` and the help card | |
| T6d | S5 | parent | `/states/<country>/` for the home country, so a region can actually be pinned — today only countries are reachable | |
| T7 | S5 | worker | country from the locale, asked once | `fc650a3` |
| T8 | S9 | parent | `Place::Search`: `/` asks the directory for a station name from any country, `F` keeps the fuzzy filter of the loaded list; help card, README and spec revised | |

## Log

**L1** — user, in the plan note: "Agregar fuentes radio como lo hace CLIAMP."

**L2** — user: "resolvi todas las decisiones y me quede con lo recomendado" —
the 10 decisions in `velo - radio.md` are closed as recommended: real streaming
(no download), typed queue entry, two separately reviewable deliverables, own
ICY parser, separate `radio.toml`, per-operation timeouts, only already-compiled
codecs, opt-in country, `radio.cliamp.stream` as a manual test target only, and
station identity = original URL with no SQLite rows.

**L3** — verified against the live service (2026-11-07), not against docs: cliamp
radio serves 15 channels in `/stations` and `/streams.m3u`, songs as finite files
(`/<id>/tracks.m3u`, `/<id>/tracks/<id>` with `Accept-Ranges`, no duration in any
of the 1134 tracks), live MP3 128 kbps / 44.1 kHz with `Icy-Metaint: 8192` only
when `Icy-MetaData: 1` is sent, and `Icy-Url` advertised but never sent. Details in
`📡 cliamp Radio Service — Especificación Técnica y Documentación`.

**L4** — user: another Pi session is working on `main` right now with T16 (AAC
priming, `M src/audio/mod.rs` + `src/audio/mp4.rs`), T15 (symphonia 0.6), T14
(opus), T13 (ReplayGain writer) and a pending artwork visual check. Agreement
sent to that session: radio owns `src/radio/**` and this logbook; nobody changes
`src/audio/output.rs` (the queue) until T5 lands; T5 waits for T15 so the seam is
not written against symphonia 0.5; `src/ui/mod.rs` writes are serialised.

**L5** — user: "Cuando puedas y estes listo ya puedes crear el worktree y comenzar
a trabajar" — worktree `.worktrees/radio` on branch `feat/radio`, started from
`46ca28a`.

**L6** — T1 closed (`b9bb2c2`, 363 lines in four files: 13 tests green, clippy
`-D warnings` clean, full suite unchanged). Two corrections came out of it:

- The delegation asked for an `InvalidData` error on a metadata block larger than
  4080 bytes. That cannot happen: `L` is one wire byte, so `L × 16 ≤ 4080` by
  construction. The worker refused to fake a RED test for it and pinned the
  reachable boundary (`L = 255`, 4080 bytes) instead; the parent then removed the
  unreachable guard rather than ship a branch no test can enter. **The bound to
  validate is a corrupt `Icy-Metaint` header, and that belongs to T5.**
- `metaint = 0` is treated as passthrough: a zero interval would never advance.
  Documented in the code, not requested.

**Slice boundary** — slice 1 is this logbook plus T1 (`444db77`, `b9bb2c2`), 475
authored lines: past the ~400-line review slice, so slice 2 starts at T2 and the
two are offered as separate reviews. Each task keeps its own commit, tests and
docs in that commit, and no task is split across slices.

**L7** — T2 closed (`f804a64`, 282 lines: 13 tests green, clippy `-D warnings`
clean, 24 test binaries all passing). Sniffing is by content, not by extension or
`Content-Type`, because both lie for wrappers in the wild. Three things to carry
forward:

- **HLS is detected only in the M3U branch.** A PLS that points at an `.m3u8`
  manifest therefore resolves to the manifest URL. One level of unwrapping is the
  contract (CLIAMP resolves one level too), so this is accepted rather than
  chased: **T5 must sniff the resolved URL's body for `#EXT-X-` before handing it
  to symphonia**, and fail with the HLS reason instead of a decode error.
- `NumberOfEntries` is parsed for but not required: a `[playlist]` with `File1=`
  and no count still resolves, which is more lenient than rejecting and matches
  the files that exist.
- PLS detection wants `[playlist]` as the first non-blank line. A file that opens
  with a `;` comment before the header is `NotAPlaylist`; no such file was found
  in the wild and leniency there was not worth untested code.

**Slice 2** closes with T2 (`f804a64`, 282 lines). **T3 opens slice 3.**

**L8** — T3 closed (`a0f946a`, 853 lines in four files: 19 tests green, clippy
`-D warnings` clean, whole suite untouched). Parent review changed two things and
added one obligation:

- **S8 rewritten**: an unrecognised `codec` (blank or `unknown`) is now allowed,
  not filtered. The worker had it rejecting them; the directory omits or fudges
  that field often enough that a whitelist would hide playable stations, and the
  cost of being wrong is one decode error that names the codec. Known codecs
  still answer from the registry.
- **`opus` became a table row** (`CODEC_TYPE_OPUS` exists in symphonia-core
  0.5.5 even though no opus decoder is compiled in), so the filter answers false
  for it today and flips by itself the day T14 enables the feature — which is what
  "never a hardcoded list" was supposed to mean. Pinned by a test.
- **The transport is deliberately duplicated.** `subsonic::client::HttpTransport`
  is reachable, but it returns Subsonic's error type and sends no `User-Agent`, so
  meeting the radio contract through it would mean editing a file another session
  owns. `src/radio/client.rs` carries its own same-shape `Transport` trait.
  Follow-up: unify the two once `src/subsonic/**` is free again.
- **Obligation for T5**: the JSON client's agent uses `timeout_global(10 s)`, which
  is right for one-shot GETs and fatal for an endless body. T5 must not reuse this
  agent for audio; S2 already requires per-operation timeouts there.

**Slice 3** is T3 alone (`a0f946a`): 853 authored lines, over the ~400-line
review slice. It is offered as one review on purpose — the client contract and the
codec seam are one behaviour pinned by one test file, and splitting the file would
have cost more than it bought — but it is recorded here so the reviewer knows the
size before opening it. **T4 and T7 stay small; the next task over ~400 lines gets
split at delegation time, not after.**

**L9** — T4 closed (`654f348`, 601 lines in three files: 11 tests, 476 green in
the whole suite, clippy `-D warnings` clean). This is the first unit that touches
user data, so the two tests that matter are the ones about not losing it: a
successful `save` leaves exactly `radio.toml` in the directory (no temp), and a
corrupt file is never truncated — `load` only ever reads, `save` only ever goes
through `create_new` + `rename`.

- **Parent change: a toggle on a key match now unmarks.** The worker produced
  `Toggled::Already` when a favourite arrived under a differently written URL
  (`…/stream/` stored, `…/stream` pressed) and flagged the asymmetry itself. That
  contradicts the rule the UI needs — pressing `f` on a marked station unmarks it,
  whatever shape the URL arrives in — so the toggles now decide on
  [`station_key`](src/radio/store.rs) alone and `Already` survives only for
  `add_station`, where "already there" is the truth. Two tests were rewritten,
  not weakened: the dedup case now asserts the station comes off and the list is
  empty.
- **Known limit**: `station_key` is hand-rolled URL parsing (no `url` crate in the
  tree); userinfo and IPv6 authorities pass through unnormalised. Directory
  stations are plain `host/path` URLs, so this is a documented edge, not a bug.
- `Toggled::needs_save()` stays useful: `Already` is the only answer that needs
  no write.

**Slice 4** is T4 alone (`654f348`, 601 lines). Same call as slice 3 and for the
same reason: one behaviour (the preferences file) pinned by one test file.
**T7 is small by design; if anything here grows past ~400 lines it gets split at
delegation time.**

**L10** — T7 closed (`fc650a3`, 199 lines in three files: 21 tests, 497 green in
the whole suite, clippy `-D warnings` clean). **S5 was rewritten: the timezone step
is gone.** Decision 8 said "`TZ`, then `/etc/localtime`, then `/etc/timezone`, then
locale", taking CLIAMP's order; a timezone yields a *city*, and turning a city into
an ISO code needs a timezone→country database, which without a dependency is a
hand-maintained table of hundreds of rows that no test would cover. A wrong "near
you" row is worse than no row, so the country now comes from the locale alone,
which carries it directly, and the code is validated against the directory's
`/countries` by the caller. The worker flagged that S5 still claimed the old path
while implementing the narrowed one — correct call, and this entry is the fix.

A curated `TZ`→country table remains possible as **T7b** if a real case appears;
it is not worth untested guessing now.

**Slice 5** is T7 alone (`fc650a3`, 199 lines), back inside the review slice.
**Status of the cross-session pair.** T5 (the stream engine) still waits for the
other session's T15 (symphonia 0.6) so the `MediaSource` seam is not written
against 0.5 and rewritten. That session has since committed T16 (`main` moved to
`1e7e36d`), ceded `src/audio/output.rs`, released `src/ui/mod.rs`, and — at the
human's request — moved T15 ahead of its ReplayGain writer, so the wait is now a
single migration rather than a hidden queue.

**L11** — T6a closed (`9e3e835`, 1439 lines: 13 tests, 510 green in the whole
suite, clippy `-D warnings` clean). The peer released `src/ui/mod.rs` — its
artwork check is look-and-confirm and it will warn before touching
`draw_artwork`/`pane`/`refresh_artwork` — so the view went ahead while T15 is
being migrated.

- **The view is a new module on purpose.** Almost everything is in
  `src/ui/radio.rs` (888 lines), and `src/ui/mod.rs` took 41 purely additive lines:
  one module declaration, one `Option<RadioView>` field with its two initialisers,
  an early key dispatch that is inert while the view is closed, one `R` arm, one
  outcome-mapping function, and one draw call over the body area. Nothing in
  `draw_artwork`, `pane` or `refresh_artwork`.
- **This is the biggest slice of the feature and the honest record is that I
delegated it too wide.** The rule is to split at delegation time; T6a should have
  been two tasks (state machine, then render and overlay). It is not split now
  because the cost moved: the file is written and green, and cutting it would
  churn more than it saves. The reviewer gets a map instead — read the 41-line
  `src/ui/mod.rs` diff first (that is the risk to existing behaviour), then the
  state machine in `radio.rs`, then treat `render` as mechanical.
- **Decisions taken during review:** `o` opens the order card while radio is
  open and stays the settings card when it is closed (documented dual meaning);
  favourite rows show the codec, not a bitrate, because `store::Favorite` has no
  bitrate — only catalog rows have one; hand-added stations and favourites are
  top-level rows rather than sub-levels; `tab` leaves radio for the queue.
- **The view opens empty today, deliberately.** `R` creates `RadioView::empty()`:
  static browse rows, no indexes, no store, no catalog. Nothing fakes data and
  `enter` on a station only holds the selection. That is why the rest of the UI
  work splits: **T6b** wires the store, the country, the directory and the
  refresh; **T6c** plays a station, draws `● live`, and updates the README `## Keys`
  and the help card — both of which the worker correctly flagged as unreachable
  from its surfaces and therefore still undocumented.

**Slice 6** is T6a alone (`9e3e835`, 1439 lines) — recorded as oversized, with the
review map above.

**L12** — T15 landed on `main` (`b2187ad` + `38c775f`, symphonia 0.6.1, 444 tests
in that branch) and `feat/radio` was rebased onto it: 13 commits replayed with
zero conflicts, which is what the file split was for.

- **The bump broke exactly one function.** `is_playable_codec` stopped compiling —
  `CODEC_TYPE_*` became `codecs::audio::well_known::CODEC_ID_*`, `CodecType` became
  `AudioCodecId` and `CodecRegistry::get_codec` became `get_audio_decoder`. One
  function, one commit (`eca04c2`), nothing else in radio touched. That is the
  whole payoff of keeping the symphonia knowledge behind that one name, and it is
  worth remembering when T5 wants to call symphonia from somewhere else.
- **Opus is still not compiled** in 0.6.1, so the table row still answers false and
  T14 remains the thing that flips it.
- **What T5 inherits from the peer's migration report** (all recorded before writing
  a line): packet timestamps are now `i64` and may be **negative** (an MP3 starts at
  `-delay`), so a stream source must not assume `u64`; `read_block` returns `None`
  **only** at the end (`Ok(None)` from `next_packet` is EOF, not an error); the
  gapless gate moved from the reader's `FormatOptions` to the decoder's
  `AudioDecoderOptions`, which is convenient here because a live stream wants no
  gapless; and `copy_to_vec_interleaved` resizes the destination, so each packet
  needs a fresh `Vec` or the tail keeps stale samples.

**T5 splits into three units at delegation time** (the lesson from T6a applied
before, not after): **T5a** the audio seam in `src/audio/mod.rs` — open from a
`MediaSource` with no extension hint and no gapless, tested against the existing
MP3 fixture through a `Cursor`; **T5b** the HTTP stream source in
`src/radio/stream.rs` — per-operation timeouts (never the JSON client's
`timeout_global`), `Icy-MetaData: 1`, `Icy-Metaint` validation, HLS sniff after the
wrapper unwrap; **T5c** the queue in `src/audio/output.rs` — `Entry::File \|
Station`, resolver only for files, seek a no-op for stations, one retry then skip.

**Slice 7** is T6b (`a2b4b78`, 1168 lines): the biggest yet, and AGAIN because I
delegated it too wide. Splitting T6a/T6b/T6c by *layer* was not enough — the test
file alone is 591 lines. The lesson to apply at the next delegation: split by
behaviour, and split the test file with it.

**L14** — T6b closed (`a2b4b78`, 1168 lines: 11 tests, **545 green** across 30
binaries, clippy `-D warnings` clean). The view now has real data: store beside the
config file, locale country, lazy directory requests, `f` persisting, `Ctrl+R`
re-requesting only what the current place needs.

- **Lazy is proven, not asserted**: a counting transport records zero calls after
  view construction, five polls and navigation keys at the top; one call per
  section entry; caching on re-entry; `Ctrl+R` re-requests. The worker wrote that
  test first and it is the one that keeps the feature honest.
- **Nothing blocks the loop**: one detached thread per request, a single in-flight
  slot, `try_recv` only — the same shape as the library indexer, and a disconnected
  thread lands as one status message instead of a hang.
- **Corrupt `radio.toml` refuses all saving for the session**, not just the first
  write. Stronger than the spec, and the right reading of "never overwrite what you
  could not read".
- **Gap found and NOT hidden**: `/states/<country>/` is never fetched, so a region
  cannot be reached and cannot be pinned, which is what S5 promised ("pinned
  countries/regions"). Countries work; regions do not. **T6d** exists for exactly
  this and is not optional bookkeeping — it is the missing half of a spec line.
- Small accepted behaviours, all documented in the code: `Ctrl+R` mid-flight is
  consumed (single slot); an order change applies to the next load rather than
  reloading the open place; browse rows show before their indexes load, which is
  what makes lazy entry possible at all.
- **A process note about this session**: the worker hit a transient compile failure
  because I rebased the worktree under it mid-task. It recovered by re-reading, but
  the rule stands — no rebase while a worker is running in the same checkout.

**L13** — the exact 0.6.1 API for the seam, read by the other session out of
`symphonia-core-0.6.1` rather than remembered. Four things, and the first one
contradicted what this logbook had assumed:

- **`FormatOptions::enable_gapless` no longer exists.** It moved to the decoder:
  `AudioDecoderOptions { gapless: bool, verify: bool }` (non-exhaustive), with
  `gapless` defaulting to **true**. Files keep `default()`; a station gets
  `AudioDecoderOptions { gapless: false, ..Default::default() }`. That is the right
  default for a live stream twice over — no gapless, and no trimming of delay that
  a stream does not declare.
- **No adapter to write**: 0.6 ships `ReadOnlySource<R: io::Read + Send + Sync>`,
  whose `MediaSource` impl reports `is_seekable() == false` and
  `byte_len() == None`. The `IcyReader` goes inside it and a non-seekable source is
  a first-class path, not an `unimplemented!`. `Seek` stays a type requirement and
  symphonia simply does not call it.
- **`MediaSourceStreamOptions::buffer_len`** kept its name and now defaults to
  64 KiB. Plenty for a body arriving at ~1.27× real time; raising it buys headroom
  at the cost of memory and startup latency, not correctness.
- **The probe risk that decides T5a's tests.** An empty `Hint::new()` is accepted
  and the probers fall back to content sniffing, but **MP3 is the weak case**: no
  magic number, only a frame-sync heuristic. If the body starts with garbage or
  with metadata that leaked past the demux, the probe can fail. So T5a's fixture
  must be an **ICY-wrapped MP3** — `tests/fixtures/silence-then-tone.mp3` with
  metadata blocks interleaved, pushed through `IcyReader` and `ReadOnlySource` — and
  the seam is not called done until it plays a real station by hand. If the probe
  fails there, the fix is to probe from the first clean frame after the demux, which
  is exactly what `icy.rs` already knows how to find.

**L15** — T5a closed (`db5712a`, `src/audio/mod.rs` +94/−20 and 168 lines of tests:
4 seam tests, **549 green over three consecutive full runs**). `TrackSource` now has
`open_stream(Box<dyn MediaSource>)`; `open`/`open_at` and the stream all build
through one `build()`, and the decoder comes from one `make_decoder(params, gapless)`
— the single place `AudioDecoderOptions` is decided, which is where the other
session will add Opus.

Two findings, one of them a bug the plan predicted and would have shipped:

- **`duration()` lied about live streams.** symphonia 0.6's MP3 reader reads the
  LAME tag and reports `num_frames` **even from a non-seekable source**, so a stream
  first answered `Some(2s)` — exactly what S2/S3 forbid, and it would have drawn a
  progress bar that counts down over a station that never ends. Fixed by forcing
  `n_frames = None` on the live path. A test now pins it.
- **The priming delta is real and is not a leak.** The stream keeps 1656 frames the
  file path trims (LAME delay 1105 + padding 551): gapless off means no trim, and a
  station declares no priming to trim anyway. The literal "identical `Vec`" from the
  delegation was therefore wrong for a tagged MP3, and the test pins the stricter
  relationship instead — same `spec()`, samples equal from the declared offset,
  total = file + delay + padding. Weakening it would have hidden both facts.
- **API corrections for anyone else touching this:** `AudioDecoderOptions` is
  `#[non_exhaustive]`, so `{ gapless, ..Default::default() }` is refused across the
  crate boundary — the builder `AudioDecoderOptions::default().gapless(g)` is the way
  (the other session's T14 plan used the struct literal). `MediaSourceStream` also
  carries a lifetime in 0.6. And the MP3 probe heuristic **survived** an ICY-wrapped
  body, so L13's fallback (probe from the first clean frame) is not needed yet.

**L16** — a flaky test I shipped in T6b, found by running the whole suite for real:
`a_failing_transport_reports_and_leaves_the_view_usable` waited on the transport
*having recorded the request* instead of on the answer *having been delivered*, so
under the load of 31 test binaries it panicked on a missing status message roughly
one run in N (it passed ten times alone). Fixed in the shared `settle` helper
(`!data.loading()` must hold as well), and re-verified with three consecutive full
runs. The lesson is general: a thread-plus-channel test that asserts on a delivered
message must wait for delivery, not for the trigger. `1094354` is that fix, kept as
its own commit so it never hides inside a feature.

**Slice 8** is T5a (`db5712a`, 242 authored lines) — back inside the review slice,
and the first unit in this feature that changes shared code (`TrackSource::open`)
rather than only adding to it: the file path keeps its old behaviour, proven by the
pre-existing suite passing unmodified.

**Log order note**: entries are appended as they happen; L13 and L14 were written in
the reverse order of their numbers because L13's content (the 0.6.1 API facts)
arrived after L14 was already recorded. Numbers, not positions, are the order.

**L17** — T5b1 closed (`a077642`, `src/radio/stream.rs` ~260 lines and
8 tests: **582 green across 33 binaries**, clippy clean). A station URL now becomes a
playing `StationStream`: one GET with `Icy-MetaData: 1` sent before the body, the
`Icy-Metaint` interval validated, the body demuxed by `icy.rs`, wrapped in
`ReadOnlySource` and handed to `TrackSource::open_stream`.

- **The timeouts are the point.** Three knobs and no others: `timeout_connect` and
  `timeout_recv_response` (both finite one-shots — headers do end) plus
  `timeout_per_read`, which ureq restarts on every body read. `timeout_global`,
  `timeout_per_call` and `timeout_recv_body` are total budgets over a body that never
  ends, so they are set nowhere and the module doc says why.
- **`Body::into_reader()` after `into_parts()`**, not `as_reader()`: the latter borrows
  a response that `open()` drops, and the seam needs a `'static` reader.
- **Title publication is a mirror, not a second parser.** The demux is boxed inside
  `TrackSource`, unreachable from the UI, so `Observed` copies `title`/`title_seq` out
  of the demux's own accessors after each read, into an `Arc<Mutex<…>>` (uncontended,
  once per 1024-frame block). No metadata is parsed twice.
- **`Icy-Metaint` boundaries**: absent means passthrough with no error; `0`, a
  non-number and anything past 64 KiB are rejected with the offending value IN the
  error, because "invalid metaint" alone tells a user nothing. 65536 stays legal.
- **Two test-side facts worth keeping.** ureq lowercases header names on the wire, so
  the request assertion is case-insensitive (the header was always sent). And the
  stalled-read test has to declare a `Content-Length` larger than the bytes it sends:
  ureq's `LimitReader` otherwise answers EOF from its own bookkeeping and the stalled
  socket is never touched — a test that would have proven nothing.
- Wrapper playlists and HLS are **not** here by design: such a body fails as
  `NotAudio` and T5b2 resolves or rejects it. ureq is declared `3.1` in `Cargo.toml`
  but resolves to 3.5.1; the three knobs exist in both.

**L18** — T5b2 closed (`fee6def`, 14 tests in `radio_stream`: **588 green across 33
binaries**, twice, clippy clean). A station whose URL is a one-entry `.pls`/`.m3u`
now plays instead of dying as `NotAudio`, which was a message that told a user
nothing.

- **Bounded inspection, no lost byte.** The first 16 KiB of the body are read into
  a buffer for sniffing and then chained back with `Cursor::new(prefix).chain(body)`
  when the body turns out to be audio. Discarding the prefix would have decapitated
  the first frames — the classic way a station "plays but crackles".
- **One level, proven by counting.** The resolved URL is requested exactly once
  (`["/station.pls", "/audio"]`); a nested wrapper is `["/one.pls", "/two.m3u"]` and
  never `/audio`; an HLS manifest records `["/stream.m3u8"]` and **no** segment,
  with a `/seg0.ts` route registered so a stray fetch would have answered 200.
  Counting beats asserting.
- **HLS fails before anything decodes**, with its own `StreamError::Hls`, instead of
  symphonia's "no suitable format reader found" — the RED output shows that exact
  string, which is what a user would have read.
- **A required test rewrite, disclosed**: the T5b1 test that asserted "not audio" used
  an M3U pointing at `http://example.com`. Under this change that body resolves by
  design, so the test would have fired a **real network request**. It now feeds junk
  bytes and keeps the same invariant; the wrapper case is covered properly by the new
  tests.
- **A real platform flake, found and fixed**: on macOS a socket accepted from a
  non-blocking listener inherits `O_NONBLOCK`, so the test server read an empty
  request and answered 404 before the request arrived. Fixed test-side
  (`set_nonblocking(false)` after accept, read until headers) and confirmed with ten
  consecutive full-suite runs. Worth remembering for any future fake server here.

**L19** — closed as `47a95b3` (24 tests in `radio_directory`, 13 in `ui_radio`, **601 green
across 34 binaries**, clippy and `cargo fmt --check` clean). Two things landed
together because they are one decision: *what counts as playable is the build's
answer, not this code's opinion.*

- **The codec table stopped having an opinion.** `is_playable_codec` translates
  spellings and then asks two decoders: symphonia's registry and
  `crate::audio::opus::decodable`. Its test no longer pins a value, it asserts the
  **invariant** (`is_playable_codec("opus") == decodable(CODEC_ID_OPUS)`), so
  compiling a decoder flips the answer without a line changing here.
- **Four T6a tests were pinning a state, not a rule.** They used a station called
  `Opus One` as the "unplayable" fixture; the moment opus became decodable the `✗`
  marker vanished and they failed. Restoring the snapshot would have been the wrong
  fix. The marker now has a reason that cannot evaporate: **an HLS manifest**
  (`is_playable_station` = decodable codec AND not a `.m3u8` path), which is true
  today, true after every future codec, and refuses exactly what nothing here can
  decode. The fixture is `Hls One` and no assertion was weakened.
- **A pattern worth naming, third time in this feature**: tests that assert on a
  *capability of today's build* (opus undecodable, one codec missing) rot the moment
  the build changes, and they rot silently until someone compiles the thing. Assert
  the rule, or assert against the capability accessor — never the snapshot.
- `cargo fmt` is now part of every delegation's verification list: sixteen of my
  files were unformatted because every worker ran clippy and none ran fmt, and that
  is what produced the conflict when `main` came back with a formatting-clean
  `mod.rs`.

**L20** — T5c1 closed (`c996545`, 213 insertions / 63 deletions across ten files: **602
green across 34 binaries**, three consecutive runs, clippy and `fmt --check` clean).
The queue has a type now and **no behaviour changed** — the untouched 601-test suite
is the proof, and the one new test is the mechanism T5c2 will rely on.

- **`pub enum Entry { File(PathBuf) }`** with `SharedQueue = Arc<Mutex<Vec<Entry>>>`.
  Only `File`, deliberately: no `Station` variant with placeholder behaviour, because
  a variant that always errors is worse than a variant that does not exist yet.
- **The resolver dispatch is structural, not commented**:
  `fn resolve_entry(resolver, entry) { match entry { Entry::File(path) => … } }`.
  T5c2 adds one arm; nothing else in the producer moves. That single match is why a
  station will never enter the Subsonic path.
- **A workaround removed before it could become a bug.** `shuffle::restore_order` was
  concrete over `PathBuf`, so restoring the shuffled order had to round-trip entries
  through paths. Lossless for files, **destructive for stations** — a station has no
  path, and a round trip would have silently dropped it. The function is now generic
  over `T: PartialEq` and the workaround is gone. Decisive evidence that it really was
  behaviour-identical: **no shuffle test needed a single line changed.**
- **`impl FromIterator<PathBuf> for Vec<Entry>` is impossible** (orphan rule), so the
  convenience is `impl From<PathBuf> for Entry` plus `.map(Entry::from).collect()` at
  the call sites. Worth knowing before someone tries it again.
- **My delegation's surface list was too narrow, and the worker did the right thing**:
  it stopped, listed the seven construction lines the compiler demanded (compiler
  proven, not guessed) and waited. When a shared type changes, the surface list has to
  include every construction site *and* the imports they need. Approved and done as
  construction-only.
- Known and intended: `Entry::as_path()` cannot survive a `Station`; T5c2 widens the
  consumers rather than faking a path.

**L21** — `main` synchronised (5964afb, the merge) and the doc corrected (`3763ee9`): **604
green across 34 binaries**, clippy and `fmt --check` clean, and `radio_seam` — the
ICY-wrapped MP3 — still passes.

- **The merge brought T14b with zero conflicts**, and `build()` needed no work from
  me: T14b adapted its own call site to `make_decoder` returning `(decoder,
  symphonia_built)` and threaded the boolean into `declared_priming`. The
  `merge-tree` prediction ("git will not ask you for anything") was right about git
  and, this time, also about the compiler; it was still the wrong thing to trust, and
  `cargo test` was the check that mattered.
- **The fix that came out of my question is in and does what it promised**: for a
  live stream the encoder padding is kept out of the trim while the normative Opus
  pre-skip is still removed, because `OpusHead` travels inside the body. The
  peer proved its test discriminates by stashing the fix and watching it fail by
  exactly 312 samples (48000 vs 47688) — a test that only runs after the fix proves
  nothing.
- **My own comment was the last lie in the file** and it is now corrected: it claimed
  a live stream "declares no priming", which stopped being true the moment the
  pre-skip was fixed. A comment that describes the old behaviour with the authority of
  sitting next to the code is worse than no comment, and this is the second time in
  this feature that a doc outlived its truth.

**L22** — T5c2a closed (`e2f31dc`, 9 files, ~1000 lines: **610 green across 35 binaries,
twice**, clippy `--all-targets` and `fmt --check` clean). A station now sits in the queue
and plays.

- **The two seams never cross, and the tests count instead of asserting prose.** A
  counting `PathResolver` and a recording `StationOpener` run through the same queue:
  for `[Station, File]` the resolver records only the file path and the opener only the
  URL, mirrored in the other order. Structurally, `resolve_entry` is reachable only from
  the `Entry::File` arm, so a station cannot enter the Subsonic path even by mistake.
- **`as_path()` became `Option<&Path>`** and `label()` was added, because a station has
  no path and the tempting fix — an invented empty path — is the kind of shortcut that
  reappears months later as a mystery in the queue.
- **`TrackFailure` is honest now**: `{ label, message }`, where `label` is exactly what
  the status line prints (a file's name, a station's own name; never a URL, never the
  hash-named cache file). That made the T12c doc true as a by-product: a post-resolve
  failure used to name the resolved cache path. One approved assertion changed value
  (`"subsonic://music.example.com/dead"` → `"dead"`) because the old one pinned an
  internal field the listener never saw, while the UI had always printed `dead`.
- **The CLI line is byte-for-byte unchanged for a skipped file**, proven the only way it
  can be: `tests/cli.rs` untouched and passing. A skipped station prints its name, and a
  test asserts the URL never appears.
- **Seek on a live station says so**: "velo: nothing to seek in a live station", instead
  of a rebuild-and-refetch that would look like a seek and behave like a reconnect.
- **Four rounds of surface extensions, and the cost was mine.** My first surface list
  omitted the test files that construct the queue, and the `TrackFailure` rename I
  demanded pulled in three more. The worker stopped every time with compiler-proven line
  lists instead of inventing scope. Lesson for the next shared-struct change: name the
  files that *construct* it in the delegation, not after the compiler complains.

**Slice 9** is T5c2a alone (`e2f31dc`): oversized at ~1000 lines across nine files, recorded
as such. It is one behaviour (a station in the queue) but it carries a type change and a
struct rename; if review wants smaller, the rename is separable in the history by file.

**L23 — T5c2b was not started, and the reason is tooling, not design.** Two attempts
were delegated and both timed out: the first stalled while exploring and wrote nothing;
the second never received a model run event at all. The tree was verified clean after
each (`git status` empty, `HEAD` unchanged), so there is nothing to revert and nothing
half-done.

Decided not to implement it inline in this session: the unit needs plumbing at **three**
open sites in `src/audio/output.rs` (~1900 lines) — the first-entry open at the player
constructor (`LiveOpener.open` around line 606), the producer's per-entry dispatch
(around line 1376), and the read-error arm inside `play_station_turn` (around line 1690)
— plus a delay that has to reach all three and an eleventh parameter on
`drain_headless_with_opener`. That is precisely the kind of edit that goes wrong at the
tail of a long context, and the whole feature's value so far came from doing these
small and verified.

**Design sketch for whoever resumes, so it is not re-derived:**

- The policy: a station gets **exactly one** retry after an injectable delay (2 s in
  production), then the queue advances with a `TrackFailure` naming the station. Files
  never retry.
- Open failures: a small helper `open_station_retrying(opener, url, delay)` used at both
  open sites.
- Read failures: hand `play_station_turn` a **reopen closure** that already contains
  the sleep, so the read-error arm reassigns `source` and continues instead of failing;
  the converter, gain and title state stay as they are. One extra parameter, no new
  timer.
- No silence watchdog: T5b1's per-read timeout already turns a stall into an error, and
  two overlapping timeout mechanisms produce the bug where the inner one fires and the
  outer one lies about why.
- The delay must be settable from tests without wall-clock sleeping, which means it
  travels from the caller, not from a constant read deep in the file.

**Not part of T5c2b but owed by the plan** (both small, both listed in the task table):
T6c (play-from-the-view, `● live` footer, README `## Keys` + help card) and T6d
(`/states` so a region can be pinned). T5d — the manual check against a real station —
is the gate for calling the engine done, and it needs the human's ears.

**L24** — T6d closed (`e4a8031`, three files: 16 tests in `ui_radio_data`, **620 green across 35
binaries**). S5's "pinned countries/**regions**" is whole now: the client always knew how
to ask for `/states/<country>/` and `Pinned` always carried a `state`, but nothing ever
fetched the index, so a region could be neither reached nor pinned. Countries worked;
regions did not.

- **Home country only, and only on demand.** The states index is requested when the
  countries section is entered, behind `/countries` in the single in-flight slot, and
  never for any other country: the directory records a region for ~44% of stations, so
  the rest of the world's regions are not worth a request. With `country = "none"` or
  unset there is no request, no row and no prompt — the opt-in stays opt-in.
- **A region row is a place like any other**: it opens `Place::Country { code, name,
  state }`, the search carries `countrycode` + `state` + `stateExact`, and `f` pins it
  through the same `Pinned` arm as a country. A pinned region survives a restart.
- **The deliberate trade**: region rows appear at the top only after the countries
  section was entered once (or `Ctrl+R` there), because the top of the view must stay
  request-free. Recorded here so nobody "fixes" it into an eager fetch later.

**On verification, disclosed**: this unit could not be gated by the full-tree
`clippy --all-targets` + `fmt --check` because a concurrent worker was mid-edit in
`src/audio/output.rs` (`unused variable: station_retry_delay`). Its own surfaces were
verified for both, its 16 tests and the whole suite pass, and the full gate is re-run at
the next commit boundary. That is the price of two writers in one tree, and the price is
paid in verification order, not in correctness of the code committed here.

**L25** — T5c2b closed (`fbe75b5`, two files: 12 tests in `radio_queue`, **622 green across
35 binaries**, clippy `--all-targets` and `fmt --check` clean, first time all three gates
ran on the same tree). A live station gets exactly one retry before the queue moves on.

- **The policy is data, not a hidden behaviour.** `Retry { attempts, delay }` with
  `NONE` (one attempt, no wait) as the default and `ONCE` (two attempts, 2 s) requested
  explicitly by production. That split came from the other session's review, and it is
  why the opener stays a pure closure and the sleep is a real no-op in tests instead of
  a promise they have to believe.
- **The budget is per turn, not per episode** — the bug neither of us had seen. An opener
  that answers `Ok` and a stream that dies 200 ms later would loop forever under a
  per-episode counter, because every successful reopen restored the budget. It is spent
  per failure and restored **only by audio actually emitted**; "the open worked" is not
  progress.
- **Three tests make the rule falsifiable**: always-`Ok` opener with a first `read` that
  always fails ends in `Turn::Failed` with exactly **two** opens; two "audio then failure"
  episodes take **three** opens; and a retry neither resets the turn nor repaints the
  title (a `None` after a reopen means "not announced yet", not "the title went away").
- **Disclosed, and the only unobserved evidence in this feature**: the RED for the last
  two tests is argued against the previous shape (a single `retried` flag), not observed —
  they were written once the final shape existed. Everything else was seen failing first.
- Deviations kept: `#[expect(clippy::too_many_arguments)]` at eight parameters, matching
  the five functions in the file that already do; the five test-support drains build
  `Sequence` with `Retry::NONE` (no `src/` caller), production asks for `ONCE`; and the
  failure message shown after an exhausted budget is the last error, not a composition.

**Slice 10** is T5c2b alone (`fbe75b5`), ~300 lines across two files — inside the slice.

**L26** — T6c-i closed (`260c62b`, three files: 23 tests in `ui_radio`, **632 green across 35
binaries**, and the three gates — test, `clippy --all-targets`, `fmt` — each with its own
exit code). A station chosen in the radio view now reaches the queue and plays, which was
the last unwired seam of the feature.

- **The call path, for whoever reads it next**: `enter` → `RadioView::key` → `enter()` →
  `selection()` → `station_of_row` → `RadioOutcome::Play { url, name }` →
  `App::radio_key` pushes `Entry::Station` through `edit_queue` → `Action::PlayQueue { from }`
  → the same `start_stream` a library queue row takes → the producer's `Station` arm →
  `LiveOpener`. No second way into the engine.
- **A station reads as live**: the clock row says `● live` in the accent with no position,
  no total and no filled bar (the duration is `None`, by contract from `open_stream`), the
  now-playing block prefers the player's ICY title and falls back to the station name, and
  `seek` refuses with the message the audio layer already used. A file's row is untouched.
- **A deliberate behaviour change**: `a` while the radio view is open is now consumed by
  the view on every row (it appends a station), where it used to fall through to the
  library's append. Recorded because it is the kind of change a reviewer should meet in a
  commit message and not by surprise.
- **One design judgement to inspect**: no test can hold a `Player` (it opens a device), so
  the transport tests drive two `#[doc(hidden)]` hooks (`show_radio_catalog`,
  `show_playing_for_tests` + `TransportFixture`) that stand in the values a real player
  reports. Production never sets the fixture and every reader asks the real player first —
  the seam is test-only, but it is a seam, so it is named here rather than buried.
- The help card and the README were left untouched on purpose: they are T6c-ii, which is
  documentation, and it comes after the feature is merged with `main` so the text is
  written once against the final state.

**L27** — T6c-ii closed (`2d6f11b`, three files, **635 green across 35 binaries**, clippy and
`fmt` at zero). The feature is documented where a user looks: a `## Radio` section in the
README (what it is, why a station has no position, the one-level wrapper rule, why HLS is
refused, where favourites and pins live, that the country is opt-in and read from the
locale, and what it deliberately does not do), four rows in `## Keys`, and four rows in the
in-app help card.

Two things had to change besides text, and both are the session's recurring defect:

- **Two render tests pinned the help card's size, not its behaviour.** One asserted the
  literal `"1–12 of 25"`, the other that the card "fits" at 80x30. Adding four rows made
  both fail — correctly, because the card grew. The fix was not to bump the numbers but to
  assert the property: the total is **read from the card** (`position_range` parses the
  `N–M of T` line), and the scroll test now asserts that `j` moves the window by one, `G`
  lands on the last row, and `g` returns to the first. A section added to the card must not
  fail it; a card that stops scrolling must.
- **`" of "` is not unique on a screen.** My first parser took the first occurrence and hit
  the label *"shuffle what is left of the queue"*. It now tries every occurrence on every
  line and answers only the one shaped like a range. Same lesson as the `ls-remote` and the
  same as the `("q", "quit")` anchor that appeared four times and put four rows into the
  footer hints instead of the card: **verify the effect, not the intent** — presence of an
  anchor is not uniqueness, and one match is not the right match.

With this, the radio feature is complete and documented on the branch. What remains is not
code: `T5d` (a real station, end to end, which needs the human's ears), the merge into
`main`, and the version that follows it.

**L28** — **T5d passed, by the human, on a real station.** The engine decoded a live stream
end to end on their machine: it played, the clock row read `● live`, the ICY title changed
with the songs, and nothing stuttered. That is the only evidence in this logbook that the
radio engine *works* rather than merely *does not break*, and it came from outside the test
suite.

One case went unobserved: no station in the catalogue they browsed was marked `✗`, so the
unplayable-station path (HLS, or a codec this build lacks) was not exercised by hand. It
stays covered by the directory tests and by `is_playable_station`; a manual sighting can
wait for a station that deserves it.

**The next direction is issue #3**, not work: velo should open on a **sources** view
(`Local library` / `Radio`) instead of dropping straight into the library; radio should
keep its countries, regions, genres and tags, gain a section for stations that belong to no
country, and put favourites at the top; and `esc` should walk back to the sources view.
Filed as https://github.com/alvaldes/velo/issues/3 at the human's request — the standing
rule from now on is that a reported problem becomes an issue first, and only an issue
produces a pull request.

**L29** — issue #17 asked to remove stations from the queue; the human decided
otherwise, and the decision is recorded here before any code change. Keeping
station queueing, verbatim: "Sí, pero con la fila marcada como vivo en la cola".
And on `next` while a station plays, verbatim: "Ir a la primera pista de la
cola" — with a track in the queue, `next` jumps to the FIRST track of the queue
and plays it; with no track in the queue it refuses with one status line and
leaves the station playing.

- **Decisión 2 of the ten (the typed queue entry) stands**: the queue stays
  typed and `Entry::Station` stays, because the real case is lining up several
  stations for an evening. The old S3 rule — `next` over a station is the next
  station — is retired, not deleted: the revision is recorded on S3 above, and
  the decisions' history is untouched.
- What the code does about it: a station row in the queue view shows the
  station's name and `● live` where a track shows its position and total (the
  same fact the transport shows for a live stream); the playing entry keeps its
  protections — `d`, `J` and `K` refuse on it, station or track alike; and
  `next` follows the human's rule in the queue's own one place, so the answer
  is the same on every screen.

**L30** — **A fresh install finds a `radio.toml` that documents itself.** The
store was only created when the first favourite, pin or hand-added station was
saved, so the format was invisible until the user had already used the feature.
It is now written on the first launch, every line a comment (so it loads as an
empty store), and never over a file that is already there — the human's call:
"al instalar se tienen que crear estos archivos y tienen que tener algo adentro".
`history.toml` is deliberately not one of them: it appears when the first track
does, because an empty history says nothing.


**L31** — issue #35: with the radio open on a short list — no favourites, or
a place whose stations have not arrived — the screen kept showing the header
and the last rows of whatever was underneath: `▸─ Sources  [5 sources]` above
the radio's own header, and the source list's rows beneath the radio's rows.
The cause: `App::draw` drew the current screen's header and body first and
then let `RadioView::render` paint over the body, and the default theme's
background is the terminal's own (`reset`) — a background that is the
terminal's own covers nothing, so every cell the radio left empty kept what
was drawn before. The rule now is that **the radio owns the body while it is
on screen**: `draw_body` draws nothing and `draw_header` does not title the
screen when `radio_on_screen()` is true. The radio does not paint an opaque
background of its own — with `background = "reset"` there is no colour to
paint, and the fix belongs where the wrong thing was drawn. The queue keeps
its own rule (issue #23): when the queue is on screen, the queue is the body
and the radio draws nothing.

**L32** — **issue #65: switching to a station is a crossfade, not a cut.** The
defect: starting an uncached station while something played cut the old stream
first and then spent the station's open (connect, headers, first bytes, about
a second) in silence, because the old player was stopped *before* the new one
was built. The fix keeps the old player running through the open and hands
over with a 200 ms crossfade: the old source recedes to zero across the window
while the new one rises from zero, and the device is never silent. The two
players run as two device streams the OS mixes; each stream's fade runs in its
own audio callback, on its own clock, armed through a one-shot mailbox
(`CrossfadeCell`) the interface writes and the callback consumes — the arm
never touches the callback's samples and the callback never waits.

- **The window is derived, never hardcoded**: `crossfade_samples(spec)` is
  200 ms of interleaved samples at the stream's own rate
  (`CROSSFADE_WINDOW`), so a 44.1 kHz stereo stream and a 48 kHz mono one
  fade over equally long walls of time and different counts of samples.
- **The overlap must be two sources, never two fades of silence**: the old
  stream's fade-out is armed only once the new stream has started sounding
  (`Player::stream_started`, published by the preroll rule), and the old
  player stops only when its fade has fully fallen (`crossfade_out_finished`
  polled by the tick) — exact silence reached, never a fader stuck half-way.
- **Gapless track→track advance is untouched**: the crossfade is a source
  switch at the interface, and its decision (`crossfade_wanted`) names a
  station as the first entry — a file start, a seek, a skip to a file, and
  the producer's own track boundary all keep today's plain path.
- **Bit-perfect refuses the fade, twice**: the interface decision answers no
  under bit-perfect (the plain switch, pinned by test), and the player's own
  `begin_crossfade_in/out` refuse too — a fade multiplies samples, which is
  exactly what bit-perfect exists to prevent, so the guard is true by
  construction, not by convention.
- **A failed open costs a status line, never the music**: the old player is
  only taken over on success; an open failure leaves it untouched and playing,
  and the line says both facts — `velo: <error> — the stream that was playing
  goes on`. A new source that opens but never sounds (a dead station behind a
  live connection) never arms the fade-out, so the old stream plays on until
  the user starts something else.
- **A handover still falling is replaced outright** by a new start, a device
  switch or quit (`drop_crossfade`), and a station reached by `skip` hands
  over exactly as a fresh start does.

The tests pin the math and the decision: the window derived from the rate,
the curves' exact ends and unity sum, both sources sounding strictly inside
the window with the old at exact silence exactly when the new reaches unity,
and the handover decision (station over a sounding stream yes; file, paused,
stopped, bit-perfect no). What no machine test can reach is the handover
itself — two real streams overlapping on one device needs the device — so
that stays ears-pending like the rest of T5d: start a track, press enter on a
station, and hear the old one recede instead of cut.
