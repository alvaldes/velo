# remote-streaming — a remote track starts sounding before its body arrives

Feature: a remote track (a cliamp channel's song row, a `subsonic://` row) is
read from its URL through HTTP Range requests instead of waiting for the whole
body to land in the cache. Revises L36 of `velo-player.md` ("Playback is
cache-on-play") on the author's report that the wait is now felt.

## Specs

**S1 — the first sound does not wait for the whole body.** Selecting a remote
track whose body the cache does not hold opens it as a stream read from the URL
as bytes arrive; the status line stops saying the track is being fetched the
moment it sounds.

> "el fetching de los tracks demora mucho porque espera a que se descargue todo
> el track antes de empezar a reproducir. se puede mejorar si no espera hasta el
> final que crees?"

**S2 — the design is B, the range source.** The remote URL is read directly,
not downloaded first: the audio path gets a media source that asks for ranges
and can answer `is_seekable()` and `byte_len()`, so a seek is a new range
request and the duration is what the decoder reports.

> "para el fetch vamos por B"

**S3 — seek survives.** A seek inside a remote track that is streaming resolves
to a new range request at the seek point, never to the end of the track and
never to a re-download of the prefix.

**S4 — a server that cannot answer ranges is not a regression.** When the URL
refuses `Range` (no `206`, no `Accept-Ranges`), the start falls back to today's
cache-on-play: full download, `.part` + rename, then open. The user still hears
the track; the status line says which way it went.

**S5 — a failed or interrupted stream is the same failure it is today.** A
dropped connection mid-track is one retry the way a station's is, then the
track fails with the sentence a failed fetch already uses, and the queue is
never advanced behind the user's back.

## Open decision

**D1 — what the cache is for, now that playback no longer needs it.** Today a
play *is* the download, so `~/.cache` accrues every track played. Streaming
makes playback independent of the cache, and three policies are possible:
(a) no cache at all — every play needs the network; (b) stream and fill the
cache in the background as the bytes pass, so a later play is local; (c) stream,
and cache only what the user marks. This decides T1's shape and the disk story;
it is asked of the author, not chosen here.

## Tasks

| ID | Task | Spec | Route | Commit |
|:--|:--|:--|:--|:--|
| T1 | A range-backed `MediaSource` in the audio path (Range requests, `is_seekable`, `byte_len`), tested against the transport fixture | S1, S2 | delegated writer + independent verifier | `17de680` |
| T2 | The start path opens remote entries through it; seek and duration pinned | S1, S2, S3 | delegated writer | `8596b33` |
| T3 | No-range fallback onto cache-on-play, with the status line naming which way | S4 | delegated writer | `8596b33` |
| T4 | Mid-track failure: one retry, the existing sentence, no queue advance | S5 | delegated writer | `5b3601b` |

## Log

**L1** — (2026-11-11, author): "el fetching de los tracks demora mucho porque
espera a que se descargue todo el track antes de empezar a reproducir. se puede
mejorar si no espera hasta el final que crees?" and, choosing among the three
designs put to them: "para el fetch vamos por B".

**L2** — the recorded decision this feature revises, quoted from
`velo-player.md` L36: "Playback is cache-on-play. … This keeps seek, gapless,
bit-perfect and ReplayGain-from-tags working with no change to the audio path.
Streaming with HTTP ranges is a later optimisation, not a correctness question:
a 1–3 s first play on a LAN is a fair price for not touching the engine." The
premise (LAN, short tracks) no longer holds for a radio channel's songs, which
is what reopens it.

**L3** — (2026-11-11, author, on D1): "Streaming + llenar el cache en
background" — the cache still fills behind the sequential reads, so the first
play streams and the second is local. Slice boundaries: `17de680` (T1, the
source and the seam), `8596b33` (T2+T3, the wiring and the fallback),
`5b3601b` (T4, the retry). What the delivered slice does **not** do, named so
nobody has to rediscover it:

- **ReplayGain reads tags from a file**, and a source that has not landed has
  no tags: a streamed track plays at its no-tag gain, and at its real gain
  once the cache holds it. The seam is `Gain::read_from(path, mode)`, file-only
  by construction; a later slice can read tags from the fill.
- **Later queue entries and a rebuild after a stream** (device switch, gapless
  reopen, repeat) still resolve through cache-on-play. They are usually a
  cache hit by then; a miss is today's download, not a stream.
- **`subsonic://` rows stay on cache-on-play** this slice.
- **Verification is partial by nature here**: the probe and the retry are
  tested against a fixture transport, and every open under a `Player` is not;
  the audible first-sound still needs ears, like the handover.

`./scripts/check.sh` green on each slice (fmt, clippy `-D warnings`, full
suite: 22 remote_stream tests, 1227+ workspace tests, 0 failures).
