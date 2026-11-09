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

## Open at lane start (the human's decision)

- **How a song is played**: cache it first through the existing Subsonic-style
  `PathResolver` (reuses what exists, seek works on the local copy, `--cache-clean`
  already covers it) or stream the finite remote file with byte ranges (no copy on
  disk, but the audio path needs a remote finite source). Not decided yet.
- **Whether the songs list is one level or a queue**: settled by the note — "playlist de
  archivos finitos", so a level whose `enter` plays from that row onwards, like an album.

## Log

**L1** — the issue: "A second radio source beside the Radio Browser directory, so `Radio`
lists both places and its own sections carry what belongs to each". Amended by the note,
which the issue itself makes authoritative.

**L2** — user, when asked whether the vault note could be read and the live service
re-verified: "Sí a las dos: la nota y la red", and "si la nota contradice el issue, gana la
nota". Hence S1: the channels arrive as configuration, never as a shipped source.
