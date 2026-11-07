# velo

A fast, single-binary music player for your terminal. Rust, no external daemon,
no server to keep running.

> **Status: early.** The audio foundation and a minimal terminal interface
> work and are tested; the library database is not built yet. See
> [Roadmap](#roadmap).

## What works today

- Decoding and playback of FLAC, MP3, M4A/AAC, OGG Vorbis and WAV through
  [symphonia](https://github.com/pdeljanov/symphonia).
- Gapless sequencing: the next track is pre-decoded and joined with no inserted
  silence and no padding.
- Encoder delay and padding trimmed where the container declares them, so an MP3
  does not carry its LAME priming frames into the next track.
- Lock-free output through a ring buffer: the device callback only copies memory,
  never decodes or allocates.
- A minimal terminal interface (`T3`): a three-band layout — header with the
  wordmark and playback state, a track list with a full-width accent selection
  and a `▸` play marker, a footer with state and key hints — plus a help
  overlay on `?`. The keymap is a pure, tested function of the key.
- A clear error instead of a gap when two tracks cannot be joined without
  resampling (resampling landed in T2).

Not yet: the library database, search, cover art, playlists, ReplayGain,
Subsonic/Navidrome, Opus. Transport polish (seek, audible volume, fades) is T6.

## Build and run

Rust 1.99 is pinned in `.mise.toml`:

```bash
mise install          # or any Rust 1.88+ toolchain
cargo build --release

velo --info song.flac song.mp3     # format, frame count and length
velo song.flac song.mp3            # open the terminal interface over them
velo --play song.flac song.mp3     # play them as one continuous stream and exit
```

Interface keys: `j`/`k` move, `g`/`G` first/last, `enter` play from the
selection, `space` pause/resume, `+`/`-` volume, `?` help, `q` quit.

## Design

The audio path is split so nothing slow runs where the clock matters:

```
TrackSource (symphonia)  →  producer thread  →  ring buffer  →  device callback
   decode + trim              fill ahead          lock-free        copy only
```

The first track decides the device format. A later track that disagrees is
rejected rather than silently resampled, because joining two rates without
resampling shifts pitch and inserts the very gap this player exists to avoid.

## Roadmap

Tracked in [`odd/tasks/velo-player.md`](odd/tasks/velo-player.md): audio
foundation (T1, done), resampling (T2), interface (T3, done), library (T4),
queue (T5), transport (T6), search (T7), cover art (T8), CLI and config (T9),
release (T10), ReplayGain (T11, T13), Subsonic/Navidrome (T12), Opus (T14),
symphonia 0.6 (T15), AAC priming trim (T16).

## License

MIT.
