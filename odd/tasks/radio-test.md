# Feature: `velo --radio-test <url>` — one-shot station probe (issue #13)

Branch `feat/radio-test`; deliverable: one PR against `main`.

## Specs

**S1 — Headless, in the shape and tone of `--subsonic-test`.**
`velo --radio-test <url>` needs no terminal, no library and no server. It prints
a short report on stdout and exits 0 when the URL plays end to end, 1 with one
`velo: ` line on stderr when it does not, and 2 on a usage error (no URL).

**S2 — The report answers, in this order:** the URL as given; the wrapper
resolution (`none`, or the URL it really points at); the redirect chain (each
hop); `content-type`; `icy-metaint` when present; the declared codec and whether
this build's symphonia features can play it; and the decode probe — how many
seconds were decoded, the sample rate and channel count, or the exact error.

**S3 — The playback path does not change behaviour.**
`src/radio/stream.rs` today reads only the metaint header; the report gets its
facts additively (a struct carrying status/headers/final URL) without altering
what the player gets. Redirects are recorded with `save_redirect_history(true)`
— recording URLs only; the retry rule the radio module documents is untouched.

**S4 — The smallest codec accessor.**
`TrackSource` exposes none today; it gains the smallest accessor that names the
decoded format so the report can print it — nothing more.

**S5 — No network in tests.**
The tests spawn the built binary against a local `TcpListener` that speaks a
canned ICY/MP3 response, the way `tests/subsonic.rs` does. Pinned: a playable
stream (exit 0 and the report lines), a 404 (exit 1 and the reason), a wrapper
`.m3u` that points at a stream (the resolved URL appears), and no URL (exit 2).

## Tasks

| ID | Spec | Route | Task | Commit |
|:--|:--|:--|:--|:--|
| T1 | S1–S5 | worker | `radio_test: Option<String>` on the CLI; `stream::probe` + `Probe` in `src/radio/stream.rs`; `TrackSource::codec_name` (S4); `radio_test` report in `src/bin/velo.rs`; `tests/radio_test.rs`; README `## Radio` | this commit |

## Log

**L1** — issue #13, the issue's own words: "A `--radio-test <url>` in the style
of `--subsonic-test`: resolve one wrapper level and print the URL it really
points at, the response headers that matter (content type, `icy-metaint`,
redirects), the codec, whether the build can play it, and the result of decoding
the first seconds of audio."

**L2** — T1 closed. Five binary-level tests in `tests/radio_test.rs` written
first and observed failing (the flag was an unknown argument), then implemented:
a playable stream pins the report's lines and their order, a wrapper `.m3u`
pins the resolved URL, a 302 pins the redirect hop, a 404 pins exit 1 with the
`velo: ` reason, and a missing URL pins clap's usage error at exit 2.

- **The probe reuses the player's open, it does not imitate it.**
  `stream::open` became a thin wrapper over a new `stream::probe`, which
  returns the opened `StationStream` plus a `Probe` record (`resolved_url`,
  `redirects`, `content_type`, `metaint`). The player's `open` discards the
  record; every fetch, sniff and wrapper rule runs once, for both callers.
- **Redirect history is recording only.** The agent adds
  `save_redirect_history(true)` — ureq collects the URLs it followed and the
  playback path reads none of them. The three per-operation timeout knobs and
  the documented retry rule are untouched.
- **The decode probe reads the same blocks the player does.** The report
  decodes up to 2 s through `StationStream::read_block`, then prints the
  seconds, rate and channels. Anything less than one decoded frame is a
  failure: headers that say audio but a body that decodes nothing is exactly
  the case this probe exists for.
- **`TrackSource::codec_name`** names only the formats a report can spell in
  words (`mp3`, `aac`, `vorbis`, `flac`, `opus`, `alac`) and answers `None`
  otherwise — symphonia's own spelling of the rest is a hex id nobody reads.
  The playback path never reads it.
- **On failure the report is not half-printed**: like `--subsonic-test`, a
  failing probe prints nothing on stdout and says the reason once on stderr.
