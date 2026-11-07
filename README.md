# velo

A fast, single-binary music player for your terminal. Rust, no external daemon,
no server to keep running.

> **Status: early.** The audio engine, the library index, the terminal
> interface, search, the queue, cover art and the command line all work and
> are tested; ReplayGain and Subsonic/Navidrome are later versions. See
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
- A library index (SQLite): scan a root folder once, incrementally after that,
  and browse by artist, album and track.
- Fuzzy search over the whole library on `/`, tolerant of misspellings and
  separations (`lofi` finds `Lo-Fi Scientists`).
- A play queue you can edit: `Tab` to the queue view, append, remove, reorder.
- Cover art rendered through the kitty, sixel or iTerm2 protocols.
- A real command line ([clap](https://docs.rs/clap)) and an optional config
  file, so the library root is not hardcoded.
- A clear error instead of a gap when two tracks cannot be joined without
  resampling (resampling landed in T2).

Not yet: ReplayGain, Subsonic/Navidrome, bit-perfect device mode (T17), Opus.

## Build and run

Rust 1.99 is pinned in `.mise.toml`:

```bash
mise install          # or any Rust 1.88+ toolchain
cargo build --release

velo                                 # browse the library in the interface
velo --index                         # (re)scan the library root into the index
velo --info song.flac song.mp3       # format, frame count and length
velo song.flac song.mp3              # open the terminal interface over them
velo --play song.flac song.mp3       # play them as one continuous stream and exit
```

Interface keys: `j`/`k` move, `g`/`G` first/last, `enter` play from the
selection, `Tab` queue view (`a` append, `d` remove, `J`/`K` move), `/` search,
`space` pause/resume, `←`/`→` or `,`/`.` seek ±5 s, `+`/`-` volume, `?` help,
`q` quit.

Exit codes: `0` on success and `--help`; `1` for a missing file, a missing
library root or a non-interactive terminal; `2` for a usage error.

### Configuration

Read once at startup, before the command line is applied:

```
$XDG_CONFIG_HOME/velo/config.toml        # else ~/.config/velo/config.toml
```

```toml
library = "/Users/you/Music/Library"   # library root to index and browse
volume = 85                            # start level, 0..=100 (default 100)
bit_perfect = false                    # opt-in for T17; nothing reads it yet
```

All keys are optional and a missing file is normal and silent. Precedence is
fixed: `--library` beats the config file, the config file beats the default.
An unknown key is an error naming the key; a bad value names the file and
the line.

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
foundation (T1, done), resampling (T2), interface (T3, done), library (T4,
done), queue (T5, done), transport (T6, done), search (T7, done), cover art
(T8, done), CLI and config (T9, done), release (T10), ReplayGain (T11, T13),
Subsonic/Navidrome (T12), Opus (T14), symphonia 0.6 (T15), AAC priming trim
(T16), bit-perfect mode (T17).

## License

MIT.
