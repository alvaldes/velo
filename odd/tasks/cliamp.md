# velo — the cliamp channels, without shipping cliamp

Issue: https://github.com/alvaldes/velo/issues/29 — "the cliamp radio service is a test
target, not a source". The issue asks for a second radio source beside the Radio Browser
directory; the vault note says otherwise, and the note wins by the author's own rule
(quoted in the issue).

## What the note settles (Decisión 9, `velo - radio.md`)

> "Usar `radio.cliamp.stream` como blanco de prueba del motor (no como feature). …
> Nada de esto agrega una dependencia ni un provider hardcodeado."
> Rejected: "shippear 'cliamp radio' como fuente built-in (es el servicio de otro
> proyecto, atado a su nginx y su caché; si cambia, velo se rompe por algo que no
> controla)".

And the same note already sketched the user-facing shape (Vista 3): with an example
`radio.toml` holding those channels, "la misma vista 1 lo muestra como una sección más.
La diferencia es que un canal **con canciones** se abre como playlist de archivos finitos
(seek y gapless normales) y uno `live` se reproduce como stream".

Verified against the live service (2026-11-07, L3): 15 channels in `/streams.m3u` and
`/stations`; songs as finite files in `/<id>/tracks.m3u` and `/<id>/tracks/<id>`, with
`Accept-Ranges` and **no duration anywhere** (0 of 1134 tracks declare one); live MP3
128 kbps / 44.1 kHz with `icy-metaint: 8192`; no auth.

So the four open questions in the issue are answered: no credential; the channel list is
the `/streams.m3u` the user already turned into a `radio.toml`; a channel's songs are a
playlist of finite files whose durations come from opening them; and it is **not** a peer
of Radio Browser nor a fourth section — it is what the config returns.

## Specs

**S1 — Nothing cliamp-specific ships.** No new source, no provider, no hardcoded host, no
new dependency. A channel exists because the user wrote it in `radio.toml`, and velo never
learns whose service it is.

**S2 — A station whose URL is a playlist of files opens as a playlist of songs.** When a
configured station's URL resolves to a wrapper (`.m3u`/`.pls`) with more than one playable
entry, the station opens as a finite list — one row per song, in the wrapper's order, each
playable and seekable as a track. A wrapper with one entry, or a plain stream URL, plays
live exactly as today. The rule is service-agnostic: any multi-entry wrapper behaves this
way.

**S3 — A song's duration comes from opening it.** The wrapper declares none. The row shows
no duration until the file is opened, and never a fake one; seek uses the file's byte
ranges once it is playing.

**S4 — Live and finite stay distinguishable everywhere.** A channel with songs is finite:
it never shows `● live`, its rows carry positions, and the queue treats it as tracks
(the rule #17 made explicit for the opposite case).

**S5 — A wrapper that cannot be expanded says why.** An unreachable or empty song list is
one status line naming the station and the reason — never an empty place that looks like a
station with no songs.

## Tasks

| ID | Spec | Route | Task | Commit |
|:--|:--|:--|:--|:--|
| T1 | S2, S3, S5 | parent | expand a multi-entry wrapper into a finite list when a configured station opens; rows and playback as tracks; the refusal line | |
| T2 | S1, S4 | parent | the live/finite distinction pinned end to end (transport, queue, marker) and the docs | |
| T1.1 | S1, S2, S5 | worker | `stream::expand` — one fetch, the decision in `expansion_for`: a multi-entry wrapper (relative entries resolved against the wrapper's own URL) expands, one entry or plain audio stays live, no songs / unreachable / bad status / HLS are refusals | |
| T1.2 | S2, S3 | worker | the resolver branch: `RemoteResolver` wraps the session's resolver; an `http(s)://` entry caches into the same track cache (cap and `--cache-clean` unchanged) and resolves to that file; identity stays the URL | |
| T1.3 | S1, S4, S5, S6 | worker | the shell: `RadioOutcome::Play` carries `configured`; `play_station` opens the songs as `Entry::File` rows, refuses with one line, or plays live exactly as before | |
| T2.1 | S1, S4, S6 | worker | the distinction pinned: the queue row of a song never reads `● live`, the station row beside it does; the transport seams (open vs resolve) untouched; docs | |
| T3 | S1, S2, S3, S4 | worker | the wrapper's title reaches the row and the probe is paid once (issue #34): `expand` carries each song's `#EXTINF`/`Title<n>` title beside its URL, the shell fills a session map at the expansion and the verdict cache at the open, `describe` alone consults the titles — `Entry` and the audio path untouched | |

## Live re-check (2026-11-09, authorized by the human)

Verified against the running service, not the note: no credential anywhere.

- `/streams.m3u` — 955 B, `audio/x-mpegurl`, 15 channels as `#EXTINF:-1,Name` plus
  `https://radio.cliamp.stream/<id>/stream`.
- `/stations` — `{"stations":[{"id","name","description","genre","stream","tracks","tracks_url"}]}`,
  3457 B. `tracks` is the channel's song count.
- `/<id>/tracks` — JSON `{"station","name","count","tracks":[{"id","title","artist","album",
  "filename","url"}]}`; `lofi` reports 120.
- `/<id>/tracks.m3u` — 12.7 KB, `#EXTINF:-1,"Artist - Title"` plus `/<id>/tracks/<id>`.
- One song — `audio/mpeg`, `Content-Length`, **`Accept-Ranges: bytes`**, and
  `Range: bytes=0-1023` answers **206 Partial Content**. **No duration is declared
  anywhere**; `Content-Disposition` carries the filename.
- One live channel — `audio/mpeg`, `Icy-Br: 128`, `Icy-Genre`, `Icy-Logo`,
  `Icy-Metaint: 8192`, no `Content-Length`, `Cache-Control: no-cache`.

## Settled at lane start

- **How a song is played** — the human's decision: "Cachear como Subsonic (reusar el
  resolver)". The song resolves to the cache through the same `PathResolver` Subsonic
  uses and the player opens it as a file, so seek, gapless and ReplayGain stay exactly
  as they are and `--cache-clean` already knows how to clean up. Identity — favourite,
  history entry, playlist row — stays the URL, never the cached file.
- **Whether the songs list is one level or a queue**: settled by the note — "playlist de
  archivos finitos", so a level whose `enter` plays from that row onwards, like an album.

## Issue #48 — the cliamp section, shown (reversing Decisión 9)

The author decided otherwise after #29 shipped: "el radio cliamp quiero que se
muestre como opcion \"1. cliamp radio\" dentro del source radio y que se vea como
en la app cliamp". This reverses Decisión 9 by the author's own decision, with
the `[radio] cliamp` switch as the mitigation the old argument keeps — the
service is still another project's, so a dead one needs a config line, not a
release.

**S7 — the section leads.** The radio top level shows `cliamp radio` first,
numbered `1.`, above `Browse all countries`; the directory's sections keep
their order below it. `[radio] cliamp = false` hides the section entirely
(default shown).

**S8 — the channels and their songs.** Opening the section asks
`GET https://radio.cliamp.stream/stations`: the channel name as the label and,
as the row's dim fact, `N tracks` and `● M listening now`, the count from
`GET /statistics` (`stations.<id>.active_listeners`). Entering a channel asks
`GET /<id>/tracks` and lists its songs in the service's order, the title as the
label and `artist · album` as the dim note.

**S9 — a song is a file, never a station.** `enter` plays from it and `a`
queues it, both through the queue as a FILE entry — the URL is the path, the
cache resolver fetches it — so the transport shows a position, not `● live`.
Identity is the URL, as everywhere else.

**S10 — two requests, and the two failures.** Opening the section makes two
requests (`/stations` and `/statistics`) and `Ctrl+R` makes two more; no
polling, nothing on a timer. A failing `/stations` is one status line naming
the reason and leaves the section present but empty; a failing `/statistics`
drops only the `●` counts, says so once, and the channels still list.

**S11 — one host, one switch.** The host is a constant in one place
(`radio::cliamp::HOST`) with a doc comment saying it is another project's
service and what happens when it changes (the section degrades, the rest of
velo does not); the `[radio] cliamp` switch exists so a dead service needs a
config line, not a release.

**S12 — nothing else changes.** The directory, `Place::NoCountry`, favourites
and pins, the queue's live semantics, history, playlists and the keys are
untouched.

## Issue #45 — a remote track can be kept (save out)

The author's words: "A Subsonic track, or a song from a configured channel, is
fetched into a cache and played from there — and `--cache-clean` deletes it.
There is no way to keep one, and a local file has nothing to keep." `ctrl+s`
keeps one.

**S13 — the save is a copy out, off the event loop.** `ctrl+s` on a remote
entry — the one playing, the row under the cursor in the queue, or a row in a
list that holds remote tracks, the same standing rule `ctrl+i` describes with
— asks for a destination in the seek prompt's card, prefilled
`~/Downloads/<the file's own name>`: the title the index or a wrapper carries
when one is known (`/` becomes `-`, a name never leaves one path component),
else the entry's own last URL segment without its query. `esc` cancels; the
copy runs on a worker thread, resolves the entry through the session's own
resolver — the exact call the producer makes, so a cache hit costs no network
and a miss is the cache's ordinary fetch-once, never a duplicated download —
and the status line shows the progress it can honestly report (a percentage
only when the whole size is known) until one last line says where the file
landed. A second `ctrl+s` while one runs is refused in one line, never a
second copy.

**S14 — the refusals and the existing destination.** A local file is refused
with its own line (it is already on disk — copying it is `cp`) and a live
station with its own (a stream does not end, so there is nothing to save).
An answer naming a file that is already on disk is refused in the card with
the name kept — velo never overwrites, so nothing is said silently.

**S15 — a failure says why and leaves no half file.** The bytes go through a
temporary sibling in the destination's own directory, renamed into place at
the end: unreachable, disk full, a destination directory that does not exist
— each is one line with the reason, and whatever was written is removed
rather than half a file. The destination is the user's choice: the library is
never touched and the cache is neither moved nor deleted.

## Slices

One pass, and deliberately small: no new source, no new dependency, no
hardcoded host (S1). The three pieces are the resolver branch (`expand` in
`src/radio/stream.rs`), the cache branch (`RemoteResolver` in
`src/subsonic/cache.rs`, composed with the session's resolver in the binary)
and the shell wiring (`RadioOutcome::Play.configured` → `play_station`). The
"level" the songs open as is the queue itself: one `Entry::File` row per
song, and the queue's `enter` plays from a row onwards, the way an album
does — no new `Place` was needed, and the radio view's own places are
untouched.

Decisions beyond the brief, each taken for the smallest honest slice:

- **Which stations resolve.** Only rows the user wrote into `radio.toml` — a
  hand-added `[[station]]` or a favourite. Directory stations always play
  live through the untouched open path (S6): no extra fetch, no behavior
  change. The rule is still service-agnostic — it keys on the fetched body,
  never on a host.
- **The `a` verb does not resolve.** It queues the station itself, so the
  queue's semantics are exactly #17's (a station in the queue reads
  `● live`); the resolution happens where the station *opens*.
- **An unreachable URL is a refusal, not a live attempt.** The shell asks
  once; `Unreachable`, a non-200 answer, HLS and a wrapper with no song
  entry are one status line naming the station and the reason, and nothing
  is queued (S5). Only a body that stalls before its first bytes — an
  audio-shaped stall the wrapper question cannot answer — falls through to
  the live path, which reports it as it always has.
- **Wrapper bodies are read whole** (bounded at 4 MiB past the 16 KiB
  inspection cap) once they sniff as a wrapper, so no entry is cut in half
  by the cap that keeps audio bodies cheap to inspect.
- **Row labels are the URLs.** `Entry::File` carries no title field, and
  `audio/output.rs` was out of scope, so the `#EXTINF` names are not
  carried into the queue rows; the queue shows the song's URL, which is its
  identity (S3) anyway. Extending `Entry` with a display name is the
  natural next slice. *(Taken up by T3 below — without touching `Entry`: the
  title rides the expansion and a session map, read by `describe` alone.)*
- **Resolution cost.** A configured station pays one probe fetch per play
  (the wrapper, or the first 16 KiB of a stream). A session cache of the
  verdict per station key would remove the repeat cost; not taken now.
  *(Taken up by T3 below: the verdict is remembered for the session — Songs
  verdicts only; failures stay retryable and plain streams keep today's
  behavior.)*

**Issue #34 (T3), one small slice on top:** the wrapper's title reaches the
row. `lists` gained `entries_with_titles` — a PLS's `Title<n>` and an M3U's
`#EXTINF` text ride beside the URLs they name; `unwrap_station`,
`pls_entries` and `m3u_entries` are unchanged and still return URLs only.
`Expansion::Songs` now carries `Song { url, title }`. The shell keeps two
session maps: `song_titles`, filled where the expansion is consumed
(`play_station`'s Songs arm) and consulted in ONE place — `describe`, the
function that already names a path for display, so the queue row, the
now-playing block and every list that shows the song by path read the title,
with a song never expanded this session keeping today's fallback (the URL);
and `expansion_cache`, filled in the `Play` route so the second open of a
configured station fetches nothing. A FAILED expansion is never remembered (a
transient error stays retryable) and a `Live` verdict is never remembered
(a plain stream never needed the probe). `Entry::File` stays path-only,
`src/audio/output.rs` is untouched, and identity — history, favourites,
playlists — stays the URL.

**Issue #48 (S7–S12), one slice on top, reversing Decisión 9:** the section
ships. `src/radio/cliamp.rs` holds the host constant ([`HOST`], with the doc
comment S11 asks for) and the three request shapes over the directory's
existing `Transport` seam — no new JSON crate, `miniserde` parses the
derived structs and the `/statistics` id-keyed map is walked as a
`miniserde::json::Value`. The view gained `Place::Cliamp` and
`Place::CliampChannel { id, name }`, their `RowKind`s, and the two new
outcomes `PlaySongs`/`QueueSong`; the shell turns both into `Entry::File`
rows whose path is the song's URL (S9), the titles feeding the same
`song_titles` map T3 built, so the queue names the songs. The data wiring
asks `/stations` then `/statistics` when the section opens and again on
`Ctrl+R`, one request in flight at a time and nothing on a timer (S10), each
failure its own one-line message; the `[radio] cliamp` switch is read once
beside the directory host (`radio_cliamp_from`) and handed to the wiring.
S12 held: the directory's own tests run unchanged with the fixture hiding
the section, which pins the switch's off state against the old layout.

## Log

**L1** — the issue: "A second radio source beside the Radio Browser directory, so `Radio`
lists both places and its own sections carry what belongs to each". Amended by the note,
which the issue itself makes authoritative.

**L2** — user, when asked whether the vault note could be read and the live service
re-verified: "Sí a las dos: la nota y la red", and "si la nota contradice el issue, gana la
nota". Hence S1: the channels arrive as configuration, never as a shipped source.

**L3** — worker, one-pass delivery of T1/T2. The human's decision quoted at lane start
("Cachear como Subsonic (reusar el resolver)") is implemented literally: the resolver
the player already uses gained one branch — an `http(s)://` queue entry caches into
the same track cache (same cap, same `--cache-clean`) and opens as a file, so seek,
gapless and ReplayGain are the file path's own (S2), and identity stays the URL
everywhere (S3). Nothing cliamp ships (S1): the channel exists only because the user
wrote it in `radio.toml`, and the rule keys on the fetched body alone. Decisions
beyond the brief are listed under `## Slices` above; the one the next lane should
revisit is the queue row's label — the `#EXTINF` titles are dropped because
`Entry::File` carries no display name and `audio/output.rs` was out of scope, so rows
show the song's URL.

**L4** — the issue (#34), in its own words: "a configured channel's songs play, but the
queue names each one by its bare URL — the wrapper already parses `Artist - Title` and
throws it away one step before it is needed", and "a configured station pays one probe
fetch every time it is opened". Both fixed in one slice (T3 above): the expansion now
carries each song's title beside its URL and the shell remembers it per session — the
map filled where the expansion happens, consulted by `describe` alone, so the queue row,
the now-playing block and any list showing the song by path name it, while identity stays
the URL — and the expansion verdict is remembered for the session, so opening a channel
twice fetches the wrapper once. A FAILED expansion is not remembered — a transient error
must be retryable — and nothing is remembered for a plain stream that never needed a
probe. The maps are session-scoped on purpose (S3): no persistence, no eviction beyond
the session, and a fresh session shows the URL until the channel is opened again.

**L5** — the issue (#48), in the author's words: "el radio cliamp quiero que se
muestre como opcion \"1. cliamp radio\" dentro del source radio y que se vea
como en la app cliamp". This deliberately reverses Decisión 9 — by the
author's own decision, the note's old rule yields — and the mitigation the
old argument asked for is the `[radio] cliamp` switch: the service is still
another project's (the host is the one constant, `radio::cliamp::HOST`, whose
doc comment says what happens when it changes), so a dead service needs a
config line, not a release. The section is cliamp-shaped, as the quote asks:
channels with track counts and live listener counts, and a channel's songs
listed `artist · album` like the app's, each song playing and queueing as a
cached file, never a live stream (S7–S12 above).

**L6** — the issue (#45), in its own words: "A Subsonic track, or a song from
a configured channel, is fetched into a cache and played from there — and
`--cache-clean` deletes it. There is no way to keep one, and a local file has
nothing to keep." `ctrl+s` keeps one: the specs above (S13–S15) pin the copy
out, the two refusals and the never-overwritten destination; the choice the
issue left open (say the file is there, or append a suffix) was taken as the
card's refusal with the name kept, because it is the one answer that never
writes anything the user did not ask for.

**L7** — the issue (#59), in the author's words: "ahora si caga pero demora
mucho y ademas no se ve el indicador de cuando esta reproduciendoce en la
lista. Y un indicador de que se esta cargando o progreso no estaria mal". The
list half is this issue: `radio_poll` handed the view only the station that
plays, so a song — queued as a file whose path is its URL — reached the view
as nothing playing and its row never marked. The playing fact now covers any
entry: the view is told the identity of whatever plays and its
[`PlayState`], and marks the matching row — a station by its station key, a
song by its URL — with `▶` while it plays, while a stopped station keeps its
`■` and nothing marks when nothing plays. The queue's `● live` fact is
untouched.

**L8** — the issue (#60), in the same author's words: "ahora si caga pero
demora mucho y ademas no se ve el indicador de cuando esta reproduciendoce
en la lista. Y un indicador de que se esta cargando o progreso no estaria
mal". The loading half is this issue: between `enter` and the first samples
of an uncached channel's song the screen said nothing. The status line now
carries `⟳ fetching <name>…` from the moment the queue reaches a remote
entry that is not cached — the producer stores which entry it is resolving
in a cell the interface reads on its tick, the way the volume and mono cells
already work — until the track starts sounding, and a failed fetch keeps its
skip report. Whether an entry waits is the resolver's own answer: a
`cached(path)` method with a default on the [`PathResolver`] trait,
implemented where the cache actually knows. The byte count the author
might have wanted is left for later: the download is one whole-body `get`
inside the transport, so there is no copy loop to hang a counter on without
rewriting it — and the line never invents a percentage.

**L9** — the issue (#60)'s other half, same words: the line was set but the
screen stayed frozen behind it, because the first entry's resolve ran
inside `apply()` — the frame only got painted when the wait ended, forty
seconds later for a channel's song. The event loop never waits for a start
any more. When the entry that would be opened first is remote and not
cached — the resolver's own `cached` seam answers, the one question both
halves of this issue already ask — `apply` hands the resolve to a worker,
says the fetching line and **returns**, so the loop paints at once; the
tick polls the worker's answer (one `mpsc` slot, `try_recv`, never a wait),
and when the file is on disk the very same start runs again — the queue and
the rebuild exactly as the request named them, the resolver now finding a
cache hit, so the stream starts as it always has. The decisions beyond the
brief, each pinned:

- **One resolve at a time, and the rule is supersede.** A newer play, skip
  or seek replaces the waiting one in the single slot; the displaced
  worker's answer is dropped unread — a download it already started lands
  in the cache and is simply reused later, never wasted. Skip and seek
  while a wait runs find no player (their own fetch closed it) and keep the
  queue standing; they cannot start a second resolve.
- **A failed resolve is one line, no stream, no advance.** The line is
  `velo: <reason>` — the selection-time error shape — and the state goes
  stopped the way a start failing on this thread always has. The standing
  queue is *not* cleared the way the device-error arm of a fresh start
  clears it: the user's queue is not moved by a network failure (the brief
  says so). A worker that died before answering — a panicked resolver — is
  the same failure with its own sentence, never an eternal wait.
- **The synchronous starts are untouched.** A local file, an already-cached
  entry and every station open on this thread in the same turn, byte for
  byte as before; the blocking build path survives only behind the
  uncached-remote gate, so the two shapes are never live for the same case.
- **The skip/seek shape is pinned through a test hook.** No test can hold a
  `Player` — building one opens an audio device — so the rebuild a skip or
  a seek performs reaches its tests through `start_track_at_for_tests`, the
  same doc-hidden hook family the transport fixture lives in.
- **What a successful fetch shows while the preroll runs** is what the
  blocking start always showed — the fetching line staying up until the
  first samples — and the headless drains and the producer are untouched.

#60 closes with this: the line appears at once, the wait runs off the event
loop, and nothing behind the start moved.
