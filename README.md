<p align="center">
  <img src="docs/media/logo.svg" width="96" height="96" alt="The velo mark: seven spectrum bars standing in the shape of a V">
</p>

# velo

[![CI](https://github.com/alvaldes/velo/actions/workflows/ci.yml/badge.svg)](https://github.com/alvaldes/velo/actions/workflows/ci.yml)

A single-binary music player for your terminal. Point it at your music folder
and it indexes the library from your files' own tags, browses it by artist,
album, track or genre, plays gaplessly and draws cover art — one Rust binary,
no daemon, no server.

Site, with a player you can press play on: <https://alvaldes.github.io/velo/>

![The library's tracks view while a track plays: the now-playing block with its clock and spectrum, the transport, and seven tracks with the playing row marked](docs/media/shot-library.png)

| The visualizer over the whole window (`V`) | A radio channel: the live row, then its songs |
|:--|:--|
| ![The spectrum drawn as bars across the full height of the terminal](docs/media/shot-visualizer.png) | ![Radio › Lofi: a live row with its listener count above the channel's numbered songs](docs/media/shot-radio.png) |

## Features

- **Local library** — indexed from tags into SQLite, refreshed in the
  background on every launch, with fuzzy search across all of it.
- **Gapless playback** of MP3, FLAC, M4A/AAC, OGG Vorbis, Opus and WAV, with
  encoder delay and padding trimmed at the seams.
- **An editable queue** — append, reorder, remove, shuffle and repeat without
  stopping playback.
- **Favourites, playlists and history** — kept in plain TOML files, with
  `.m3u`/`.pls` import and export.
- **Internet radio** — the [Radio Browser](https://www.radio-browser.info/)
  directory, [cliamp](https://radio.cliamp.stream) channels, and any stream URL.
- **Subsonic servers** (early access) — Navidrome, Airsonic, Gonic: synced
  into the same index, played through a local cache, scrobbled.
- **ReplayGain** — applied at playback with clipping protection, and an
  EBU R128 scanner that writes the tags.
- **Bit-perfect mode**, mono, output device selection and OS media keys
  (macOS, MPRIS).
- **Nine visualizers and thirteen themes**, or your terminal's own colours.
- **No telemetry** — see [Privacy](#privacy).

## Install

**Homebrew** (macOS, Linux):

```sh
brew install alvaldes/velo/velo
```

**Install script** (macOS, Linux) — verifies the release checksum and installs
to `~/.local/bin`, no sudo:

```sh
curl -fsSL https://alvaldes.github.io/velo/install.sh | sh
```

`VELO_INSTALL_DIR` names another directory and `VELO_VERSION` another tag.

**Prebuilt binary** — download `velo-<tag>-<target>.tar.gz` from
[Releases](https://github.com/alvaldes/velo/releases) and put `velo` on your
`PATH`. Built for Linux and macOS, x86_64 and aarch64.

**From source** — Rust 1.88 or newer and `cmake`; on Linux also the ALSA
headers (`libasound2-dev`). See [CONTRIBUTING.md](CONTRIBUTING.md).

```sh
git clone https://github.com/alvaldes/velo
cd velo
cargo install --path .
```

## Quick start

```sh
velo
```

Press `o` and type your music folder (`tab` completes). velo saves it to
`config.toml` and indexes it right away; every later launch refreshes the
index in the background. `velo --library <path>` uses another folder for one
session without saving it.

The first screen is `Sources`: `Local library`, `Radio`, `Recently played`,
`Favourites` and `Playlists`. `enter` opens and plays, `esc` goes back one
level, `/` searches, `?` lists every key.

Files given on the command line skip the library:

```sh
velo song.flac song.mp3          # open the interface over them
velo --play song.flac song.mp3   # play as one gapless stream, then exit
velo --info song.flac            # format, frame count and length
```

## Keys

The essentials. The footer names the keys of the screen you are on — the
radio's rows, the queue and a level of the library each answer to a different
handful — and `?` opens the full, filterable list inside velo.

| Key | Action |
|:--|:--|
| `j` / `k`, `g` / `G` | Move down / up, first / last row |
| `enter`, `esc` | Open or play from the selection; back one level |
| `1` `2` `3` `4` | Library view: artists, albums, tracks, genres |
| `/` | Fuzzy search the library; in radio, search stations by name |
| `space` | Pause / resume |
| `←` / `→` | Seek 5 s; with `shift`, 30 s |
| `:` | Jump to a position: `70%`, `1:20` or `90` |
| `n` / `,` | Next / previous track |
| `+` / `-` | Volume, 5% at a time; `[` / `]` a point at a time |
| `z`, `r`, `m` | Shuffle; repeat off → all → one; mono |
| `tab` | To the queue and back |
| `a` / `A` | Append the selection to the queue / append and play it |
| `x`, `J` / `K` | In the queue: remove the entry, move it down / up |
| `f` | Favourite a track or a station |
| `S` | Save the queue as a playlist; on a track, add it to one |
| `R`, `L` | Jump to the radio, to the saved playlists |
| `u` | Play a stream URL |
| `v`, `V` | Next visualizer; visualizer over the whole window |
| `d` | Choose the audio output device |
| `ctrl+i` | Describe the track: path, codec, bitrate, ReplayGain tags |
| `ctrl+p` | Settings: everything in `config.toml`, applied live |
| `ctrl+z` | Suspend to the shell; `fg` resumes |
| `q` | Quit |

## Radio

`R` opens internet radio: cliamp's channels, then the Radio Browser directory
by country, region, genre or name, then your favourites.

- A station is a live stream: no position, no seek, and the title comes from
  the station's own ICY metadata.
- A station velo cannot play (an HLS manifest, an unsupported codec) is marked
  `✗`; `i` on it opens a prefilled bug report in your browser.
- `f` favourites a station or pins a place. Favourites and hand-added stations
  live in `radio.toml`.
- `velo --radio-test <URL>` probes a stream without the interface: it exits `0`
  when the URL plays end to end, `1` with the reason when it does not.

## Subsonic servers (early access)

velo talks to any server exposing the Subsonic API. Add the table to
`config.toml`:

```toml
[subsonic]
url = "https://music.example.com"
user = "you"
# api_key = "the-key-from-the-server"
# insecure = false   # true accepts a self-signed certificate
```

The secret is read from `api_key`, then the `VELO_SUBSONIC_SECRET` environment
variable, then the first line of `~/.config/velo/subsonic.key`.

```sh
velo --subsonic-test   # can I use this server?
velo --sync            # copy the server's library into the index
velo --cache-clean     # empty the track and cover caches
```

Syncing is explicit and never runs on startup. Remote tracks then browse,
search and queue like local ones. A track is downloaded once, untranscoded,
into `~/.cache/velo/subsonic` (capped at 2 GiB) and played from there, so
seeking, gapless and ReplayGain all work. `ctrl+s` saves a cached track out to
a folder you name.

## Command line

Everything below runs without a terminal, so it works over ssh and in scripts.
`velo --help` has the full reference.

```sh
velo --index                                  # refresh the library index, print counts
velo history [--json | clear]                 # what played, newest first
velo update                                   # is a newer velo out?
velo playlist import mix.m3u                  # .m3u, .m3u8 and .pls
velo playlist export Mix ~/backup/mix.m3u
velo --replaygain-scan album/*.flac           # measure loudness — a dry run
velo --replaygain-scan --apply album/*.flac   # write the ReplayGain tags
```

`--replaygain-scan` treats the files of one invocation as one album and is the
only command that ever writes to a music file, and only with `--apply`.

## Configuration

Everything lives in `$XDG_CONFIG_HOME/velo`, else `~/.config/velo`:

| File | Holds |
|:--|:--|
| `config.toml` | Every setting, written on first launch with a comment per key |
| `radio.toml` | Favourite stations, pinned places, stations added by hand |
| `library.toml` | Favourite tracks and saved playlists |
| `history.toml` | What played, newest first, capped at 500 entries |
| `themes/*.toml` | One file per custom theme |

`ctrl+p` changes most settings while velo runs and writes them back key by
key, leaving your comments and ordering alone. The ones worth knowing:

| Key | Meaning | Default |
|:--|:--|:--|
| `library` | The folder to index: absolute, `~/…` or relative. `--library` overrides it | none |
| `replay_gain` | `track`, `album` or `off` | `track` |
| `bit_perfect` | Samples reach the device unmodified: no resampling, no ReplayGain, volume pinned to 100% | `false` |
| `[theme] name` | A built-in theme or a file in `themes/` | `velo` |
| `[theme] transparent_background` | Let the terminal's own background show through | `false` |
| `[ui] visualizer` | `bars`, `bars-outline`, `wave`, `scatter`, `flame`, `matrix`, `scope`, `mirror` or `none` | `bars` |
| `[ui] cover_art` | Draw the cover art pane | `true` |
| `[ui] low_power` | A quarter of the redraw rate and no spectrum; `P` toggles it | `false` |
| `[audio] device` | The output device by name; `d` picks it | system default |
| `[update] check` | `daily`, `weekly` or `never` | `daily` |

An unknown key is an error that names the key; a bad value names the file and
the line.

### `[update]` — knowing a newer velo exists

| Key | Meaning | Default |
|:--|:--|:--|
| `check` | How often velo may ask GitHub whether a newer velo is out: `daily`, `weekly` or `never` — `never` means no request at all | `daily` |
| `last_check` | When the check last asked, as unix seconds — written by velo itself, never edited by hand; delete the line to make the next launch ask at once | absent |

A due check is one request to `https://api.github.com/repos/alvaldes/velo/releases/latest`
on its own thread, off the event loop; it says nothing while it runs, says one
line in the interface when a newer version exists, and is silent on any
failure — no retry storm, no error line. The settings card's `version` row
(`ctrl+p`, under `about`) always names the running version, and the newer one
when a check found it. velo never downloads, verifies or replaces itself.

To ask for a check yourself: `velo update` in the shell asks now, whatever
`check` and `last_check` say, and answers on stdout — the running version,
the newest release, and one of two verdicts: up to date, or a newer version
plus how to get it (`brew upgrade velo` for a Homebrew install, the release
archive otherwise). Exit 0 for either verdict; a request or an answer that
failed is one line on stderr and exit 1. `enter` on the `version` row of the
settings card asks the same way in the interface, and its answer lands like
the automatic one's: the status line, the row, and the refreshed
`last_check`. Neither route ever downloads or replaces the binary.

## Privacy

velo sends no usage data: no analytics, no install id. It contacts only what
you point it at — the Radio Browser directory, the cliamp service, your own
Subsonic server — plus one update check a day against GitHub's releases API,
with no identifier attached. `[update] check = "never"` turns that off. velo
never downloads or replaces itself.

## Requirements

- Linux or macOS. Windows is untested.
- An interactive terminal.
- For cover art, a terminal that speaks the kitty, sixel or iTerm2 protocol
  and a window of at least 100×28. The cover is the picture embedded in the
  track, else a `cover`, `folder` or `front` `.jpg`/`.png` in its folder.

## Exit codes

| Code | Meaning |
|:--|:--|
| `0` | Success |
| `1` | A missing file or library, a non-interactive terminal, a playback error |
| `2` | A usage error, or a config file that cannot be read or parsed |

## Contributing

```sh
cargo build --release
./target/release/velo --library ~/Music
scripts/check.sh    # fmt, clippy -D warnings and tests: exactly what CI runs
```

[CONTRIBUTING.md](CONTRIBUTING.md) has the platform dependencies, the commit
convention and what a reviewable change looks like. Changes are listed in
[CHANGELOG.md](CHANGELOG.md).

## License

MIT — see [LICENSE](LICENSE).
