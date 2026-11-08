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
Seek is a no-op and progress shows `live`.

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
`R` opens/closes it, `/` filters the focused list, `enter` enters a country or
tag and plays from the selected row, `a` queues without leaving, `f` favourites
a station or pins a country, `N` opens the country list, `Ctrl+R` refreshes the
directory, `tab` alternates radio and queue, `1`–`4` stay library views. UI copy
is English. Footer shows `● live` instead of a position and total.

## Tasks

| ID | Spec | Route | Task | Commit |
|:--|:--|:--|:--|:--|
| T1 | S4 | worker | `src/radio/icy.rs`: ICY length-prefixed demux over a `Read`, `StreamTitle` extraction, passthrough when no metaint | `b9bb2c2` |
| T2 | S7 | worker | `src/radio/lists.rs`: PLS and M3U parse to the first playable entry | `f804a64` |
| T3 | S1, S8 | worker | `src/radio/{client,directory}.rs`: Radio Browser client, indexes, station search, codec filter from symphonia features | `a0f946a` |
| T4 | S5 | worker | `src/radio/store.rs`: atomic `radio.toml` for favourites, pins and hand-added stations | `654f348` |
| T5 | S2, S3 | parent | stream engine: `MediaSource` over the HTTP body, queue entry `File \| Station`, live semantics; **blocked on the other session's T15 (symphonia 0.6)** | || T6a | S9 | worker | `src/ui/radio.rs`: the view's state machine, sections, fuzzy filter and order card, hooked with 41 additive lines in `src/ui/mod.rs` | `9e3e835` |
| T6b | S9 | parent | wiring: `RadioStore` load, `country::detect`, directory fetch for countries/tags/stations, refresh, catalog handoff | |
| T6c | S9, S3 | parent | playing a station and the `● live` footer; **needs T5**; also README `## Keys` and the help card | |
| T7 | S5 | worker | country from the locale, asked once | `fc650a3` |

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
