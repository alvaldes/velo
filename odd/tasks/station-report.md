# Feature: a station that cannot play says why, and can be reported (issue #19)
Branch `feat/station-report`; deliverable: one PR against `main`.

## Specs

Quoted verbatim from issue #19, which was filed from the human's own words after a real
station refused to play.

- **S1** "On a `✗` row, say why: `HLS manifest` versus `the codec X is not in this build`.
  In the row's note, or in the status line, in the same way the library's cover art says why
  it hides instead of hiding silently."
- **S2** "The README says what to do with one. Beside the `✗` explanation: a station marked
  `✗` is worth reporting as an issue, with the name, the codec, the URL, the reason velo
  reported, the velo version and the platform."
- **S3** "On a `✗` row, one keypress builds a prefilled GitHub issue (title and body carrying
  those facts) and opens it in the browser with the system opener. velo holds no token and
  posts nothing: the human reads it and submits. That is the 'something simple' — no
  configuration, no account, no server."

The human's own words, from the message that produced the issue:

> si prepara el issue para que ademas se documente en el readme que si alguien encuentra una
> estacion asi que lo informe para agregarlo como issue. Y ya un pro de esta issue es que la
> app permita desde su interfaz reportar como issue que esa no abre pero algo sencillo
> presiono una tecla sobre estos X y listo reporte hecho.

## Decisions taken (not extra requirements)

- **The reason is computed once, in one place.** `is_playable_station` already decides
  *whether*; the same module grows the *why*, so the marker, the status line and the report
  cannot disagree.
- **The report never posts.** The key builds a `github.com` issue URL and hands it to the
  system opener (`open` on macOS, `xdg-open` on Linux). No token, no account, no daemon. If
  the opener fails, the URL lands in the status line so nothing is lost.
- **The key is `i`** (info / issue), unbound today, and it only acts on a `✗` row. It is
  written in the README next to the other radio keys.
- **`f` stays favourite.** Reporting is not marking, and the two must not share a key.

## Tasks

| ID | Specs | Route | Surface | Commit |
| --- | --- | --- | --- | --- |
| T1 | S1 S2 | worker | `src/radio/directory.rs`, `src/ui/radio.rs`, `src/ui/mod.rs`, `tests/radio_directory.rs`, `tests/ui_radio.rs`, `README.md` | `feat(radio): a station that cannot play says why` |
| T2 | S3 | worker | `src/ui/radio.rs`, `src/ui/mod.rs`, `tests/ui_radio.rs`, `README.md` | `feat(radio): i reports a station that cannot play` |
| T3 | S1–S3 | parent | — | verification: fmt, clippy, the suite, and the URL opened by hand |

## Log

**L1 — the human's request, verbatim.**

> si prepara el issue para que ademas se documente en el readme que si alguien encuentra una
> estacion asi que lo informe para agregarlo como issue. Y ya un pro de esta issue es que la
> app permita desde su interfaz reportar como issue que esa no abre pero algo sencillo
> presiono una tecla sobre estos X y listo reporte hecho.

The station that produced it: `الجزيرة صوت` `[UNKNOWN]` · QA, whose `url_resolved` ends in
`.m3u8` — so the marker fired on the URL rule, not the codec, and favouriting it and pressing
`enter` produced `velo: unsupported format: the station body is an HLS manifest: its .ts
segments cannot be decoded` (`src/radio/stream.rs:161`).
