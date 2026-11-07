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
Country is opt-in: `TZ`, then `/etc/localtime`, then `/etc/timezone`, then
locale; no geo-IP; stored as `country = "NO"` or `country = "none"`.

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

**S9 — The radio view follows the sketched layout.**
`R` opens/closes it, `/` filters the focused list, `enter` enters a country or
tag and plays from the selected row, `a` queues without leaving, `f` favourites
a station or pins a country, `N` opens the country list, `Ctrl+R` refreshes the
directory, `tab` alternates radio and queue, `1`–`4` stay library views. UI copy
is English. Footer shows `● live` instead of a position and total.

## Tasks

| ID | Spec | Route | Task | Commit |
|:--|:--|:--|:--|:--|
| T1 | S4 | worker | `src/radio/icy.rs`: ICY length-prefixed demux over a `Read`, `StreamTitle` extraction, passthrough when no metaint | |
| T2 | S7 | worker | `src/radio/lists.rs`: PLS and M3U parse to the first playable entry | |
| T3 | S1, S8 | worker | `src/radio/{client,directory}.rs`: Radio Browser client, indexes, station search, codec filter from symphonia features | |
| T4 | S5 | worker | `src/radio/store.rs`: atomic `radio.toml` for favourites, pins and hand-added stations | |
| T5 | S2, S3 | parent | stream engine: `MediaSource` over the HTTP body, queue entry `File \| Station`, live semantics; **blocked on the other session's T15 (symphonia 0.6)** | |
| T6 | S9 | parent | radio view in `src/ui/mod.rs`; **serialised with the other session's artwork work** | |
| T7 | S5 | worker | country from TZ/locale, asked once | |

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
