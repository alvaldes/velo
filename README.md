# velo

A single-binary music player for your terminal: point it at your music folder,
and it indexes your library from your files' own tags, browses by artist,
album, track or genre, plays gaplessly, and renders cover art — all from one
Rust binary, with no daemon and no server. A fuzzy search and an editable
queue make it a player you actually drive, not a playlist viewer.

```
╭──────────────────────────────────────────────────────────────────────────────╮
│ library  queue                                                       V E L O │
│ ♫ nothing playing                                                            │
│ enter plays the selection                                                    │
│                                                                              │
│ --:--   ■ stopped                                                            │
│ ──────────────────────────────────────────────────────────────────────────── │
│  ◂◂   ▶ play   ▸▸    shuffle  repeat off     RG track   VOL ━━━━━━━━━━━● 100 │
│                                                                              │
│ ▸─ artists  albums  tracks  genres   [3 artists]                             │
│ ▸ 1. Lo-Fi Scientists                                    2 albums · 3 tracks │
│ ▸ 2. Nujabes                                               1 album · 1 track │
│ ▸ 3. Poldoore                                             1 album · 2 tracks │
│                                                                              │
│ j/k move  enter play  space pause  ←/→ seek  / search  ? help  q quit        │
╰──────────────────────────────────────────────────────────────────────────────╯
```

While a track plays, the block on top names it — by its library tags, or by
its file name when it was given on the command line — counts the
clock against its length, fills the progress bar, and the primary button
turns into `❚❚ pause`. On a tall terminal a spectrum of the music sits
between the clock and the progress bar. On a short terminal the block
shrinks to three rows, then to one, before the list gives up any of its own.

velo wears your terminal's colors: its background (translucency included)
and text, and the ANSI palette for everything else — magenta for the
accent, blue for keys and counts, bright black for what can be dimmed.

## Keys

Every key below is what the code does today.

| Key | Action |
|:--|:--|
| `j` / `↓`, `k` / `↑` | Move down / up |
| `g` / `G` | First / last row |
| `enter` | Open an artist, album or genre; play from the selected track onwards through the list it is in |
| `esc` / `backspace` | Back one level (browse), close the help overlay |
| `1` `2` `3` `4` | Library view: artists (artist → album → track), every album, every track, genres |
| `/` | Fuzzy search the whole library (`lofi` finds `Lo-Fi Scientists`) |
| `space` | Pause / resume |
| `←` / `→` (or `,` / `.`) | Seek 5 s back / forward |
| `n` / `p` | Next track / previous track — `p` restarts a track already 3 s in |
| `s` | Shuffle what is left of the queue; again to put it back in order |
| `r` | Repeat: off → all (the queue again) → one (the track again) |
| `L` | Set the library folder: the same prompt as the first run, at any time |
| `u` | Refresh the library index in the background; the title bar shows `⟳ indexing…` until it ends and the lists reload |
| `+` / `-` | Volume up / down, 5% at a time |
| `tab` | Switch between the library browser and the queue |
| `a` | Append the selection to the queue — a track, or every track of an album or genre |
| `d` | Remove the selected entry from the queue (queue view) |
| `J` / `K` | Move the selected entry down / up (queue view) |
| `enter` (queue view) | Play the queue from the selected entry |
| `?` | Help card: `j`/`k` (or `↓`/`↑`) scroll it and `g`/`G` jump to its ends; `/` filters it by key or description (`enter` keeps the filter, `esc` clears it, a second `esc` or `?` closes the help). The card keeps its full size however few rows the filter leaves, and every other key waits until it closes — `q` and `space` included |
| `q` | Quit |

While a search is open, every printable key types into the query,
`backspace` deletes, `enter` plays from the selected result, and `esc`
cancels — leaving the browser exactly where it was. The queue only lets you
edit entries after the one playing: the playing entry cannot be removed and
nothing moves across it, and each refusal explains itself in the status line.

## Install

**Prebuilt release binary** — download `velo-<tag>-<target>.tar.gz` for your
platform from the repository's
[Releases](https://github.com/alvaldes/velo/releases) page, unpack it, and put
the `velo` binary on your `PATH`.

**From a clone, with cargo:**

```sh
git clone https://github.com/alvaldes/velo
cd velo
cargo install --path .
```

**From crates.io, once published:**

```sh
cargo install velo
```

Building from source needs Rust 1.88 or newer; the release binaries are built
by CI for `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`,
`x86_64-apple-darwin` and `aarch64-apple-darwin`.

## Quick start

```sh
velo --library /path/to/your/music --index   # point velo at your music, index it
velo --library /path/to/your/music           # open the browser
```

Every launch refreshes the index in the background: the interface is usable
at once, the title bar shows `⟳ indexing…` while the scan runs, and the lists
reload with the counts in the status line when it ends. `u` runs the same
refresh at any time.

Then, inside the interface: `enter` into an artist, into an album, `enter`
again to play — or `2` for every album, `3` for every track, `4` for genres
(read from the files' genre tags; `Dub / Trip-Hop` counts in both); `/` to
search; `←`/`→` to seek; `tab`, `a` to queue something
else without stopping playback. Files given on the command line play as one
gapless stream instead:

```sh
velo --info song.flac          # format, frame count, length
velo song.flac song.mp3        # open the interface over them
velo --play song.flac song.mp3 # play and exit when finished
```

## Configuration

One TOML file, read once at startup:

```
$XDG_CONFIG_HOME/velo/config.toml      # else ~/.config/velo/config.toml
```

When the file does not exist, velo writes it with every key at its default
value and a short comment per key, so everything below can be changed from
it. An existing file is never overwritten or reformatted.

| Key | Meaning | Default |
|:--|:--|:--|
| `library` | The root folder to index and browse: an absolute path, `~/...` (your home) or a path relative to the directory velo starts in | none — written commented out (`# library = "~/Music"`) |
| `volume` | Start level, `0..=100` | `100` |
| `bit_perfect` | Accepted and stored; nothing reads it yet | `false` |
| `replay_gain` | ReplayGain mode: `track` (use the track's own gain), `album` (album gain, falling back to the track's), or `off` | `track` |

Precedence: `--library` (expanded the same way) beats the config file. With no
library set, `velo` opens the interface with a floating prompt asking for the
folder. It starts at `~/`, which cannot be deleted, and `tab` completes folder
names the way a shell does (several matches are listed under the field). A
path that is not a directory is refused in the prompt; an accepted one is
saved to the file as `library = "~/<what you typed>"` (every other line
is kept) and indexed right away; `esc` skips it and the empty browser says how
to set it. `velo --index` without a library exits 2 and names the key and the
flag. An unknown key is an error naming the key; a bad value names the file
and the line.

## Requirements

- Linux or macOS. Windows is untested.
- To build from source on Linux, the ALSA development headers
  (`libasound2-dev` on Debian/Ubuntu); the audio output goes through cpal.
- A terminal. The interface needs an interactive terminal and refuses to run
  otherwise.
- Cover art is drawn when the terminal speaks the kitty, sixel or iTerm2
  protocol and the window is at least 100×28. Without one of those protocols
  there is no art pane at all — the layout is unchanged, and the status line
  says why a cover is hidden rather than faking one with half-blocks. The
  cover is the picture embedded in the track or, when it has none, a
  `cover`, `folder` or `front` `.jpg`/`.png` in the track's folder.

## Exit codes

| Code | Meaning |
|:--|:--|
| `0` | Success, including `--help` |
| `1` | A missing file, a missing library root, a non-interactive terminal, or a playback error |
| `2` | A usage error, or a config file that cannot be read or parsed |

When `replay_gain` is `track` or `album`, every track's ReplayGain tags are
read at playback time and the gain is applied to the audio with clipping
protection: a track that declares its own peak is never pushed past full scale.
Tracks without usable tags play unchanged; a wrong tag value is ignored rather
than misapplied.

## Not yet

Honest list; none of these exist behind a flag:

- ReplayGain tag writing (reading and applying them is in: the `replay_gain`
  setting).
- Subsonic/Navidrome (streaming from a server).
- Opus — symphonia has no Opus decoder yet, so `.opus` files are rejected at
  probe time.
- Playlists.
- Bit-perfect playback — the `bit_perfect` config key exists and nothing
  reads it.

MP3, FLAC, M4A/AAC, OGG Vorbis and WAV are what plays today. Sample rates are
converted to your output device; channel layouts are not converted, except
mono, which plays through every output channel — a layout that would need a
real downmix (5.1 into stereo, say) is skipped with an announcement instead of
being silently averaged away.

## License

MIT — see [LICENSE](LICENSE).
