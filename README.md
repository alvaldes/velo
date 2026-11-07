# velo

A single-binary music player for your terminal: point it at your music folder,
and it indexes your library from your files' own tags, browses by artist and
album, plays gaplessly, and renders cover art — all from one Rust binary, with
no daemon and no server. A fuzzy search and an editable queue make it a player
you actually drive, not a playlist viewer.

```
velo                                        artists   16        stopped
▸ Lo-Fi Scientists         2 albums · 7 tracks
  Poldoore                 3 albums · 14 tracks

j/k move  enter play  space pause  ←/→ seek  / search  tab queue  ? help  q quit
```

## Keys

Every key below is what the code does today.

| Key | Action |
|:--|:--|
| `j` / `↓`, `k` / `↑` | Move down / up |
| `g` / `G` | First / last row |
| `enter` | Descend into an artist or album; play from the selected track |
| `esc` / `backspace` | Back one level (browse), close the help overlay |
| `/` | Fuzzy search the whole library (`lofi` finds `Lo-Fi Scientists`) |
| `space` | Pause / resume |
| `←` / `→` (or `,` / `.`) | Seek 5 s back / forward |
| `+` / `-` | Volume up / down, 5% at a time |
| `tab` | Switch between the library browser and the queue |
| `a` | Append the selection to the queue — a track, or every track of an album |
| `d` | Remove the selected entry from the queue (queue view) |
| `J` / `K` | Move the selected entry down / up (queue view) |
| `enter` (queue view) | Play the queue from the selected entry |
| `?` | Help overlay |
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

Then, inside the interface: `enter` into an artist, into an album, `enter`
again to play; `/` to search; `←`/`→` to seek; `tab`, `a` to queue something
else without stopping playback. Files given on the command line play as one
gapless stream instead:

```sh
velo --info song.flac          # format, frame count, length
velo song.flac song.mp3        # open the interface over them
velo --play song.flac song.mp3 # play and exit when finished
```

## Configuration

An optional TOML file, read once at startup:

```
$XDG_CONFIG_HOME/velo/config.toml      # else ~/.config/velo/config.toml
```

| Key | Meaning | Default |
|:--|:--|:--|
| `library` | The root folder to index and browse | `--library` flag, else the config file, else the built-in default — which is the author's own `~/Music/Library`, so set your own |
| `volume` | Start level, `0..=100` | `100` |
| `bit_perfect` | Accepted and stored; nothing reads it yet | `false` |

Precedence: `--library` beats the config file, the config file beats the
default. A missing file is normal and silent. An unknown key is an error
naming the key; a bad value names the file and the line.

## Requirements

- Linux or macOS. Windows is untested.
- To build from source on Linux, the ALSA development headers
  (`libasound2-dev` on Debian/Ubuntu); the audio output goes through cpal.
- A terminal. The interface needs an interactive terminal and refuses to run
  otherwise.
- Cover art is drawn when the terminal speaks the kitty, sixel or iTerm2
  protocol and the window is at least 100×28. Without one of those protocols
  there is no art pane at all — the layout is unchanged, and the status line
  says why a cover is hidden rather than faking one with half-blocks.

## Exit codes

| Code | Meaning |
|:--|:--|
| `0` | Success, including `--help` |
| `1` | A missing file, a missing library root, a non-interactive terminal, or a playback error |
| `2` | A usage error, or a config file that cannot be read or parsed |

## Not yet

Honest list; none of these exist behind a flag:

- ReplayGain.
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
