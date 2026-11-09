# velo

[![CI](https://github.com/alvaldes/velo/actions/workflows/ci.yml/badge.svg)](https://github.com/alvaldes/velo/actions/workflows/ci.yml)

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
│ ▸─ 1 artists  2 albums  3 tracks  4 genres   [3 artists]                     │
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
| `enter` (sources) | Open the source under the cursor: `Local library` or `Radio` |
| `enter` (library) | Open an artist, album or genre; play from the selected track onwards through the list it is in |
| `esc` / `backspace` | Back one level; from the top of the library, or when the radio closes, back to `Sources`. Closes the help overlay |
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
| `tab` | Switch between the library browser and the queue — inert on `Sources` |
| `a` | Append the selection to the queue — a track, or every track of an album or genre |
| `d` | Remove the selected entry from the queue (queue view) |
| `J` / `K` | Move the selected entry down / up (queue view) |
| `enter` (queue view) | Play the queue from the selected entry |
| `?` | Help card: `j`/`k` (or `↓`/`↑`) scroll it and `g`/`G` jump to its ends; `/` filters it by key or description (`enter` keeps the filter, `esc` clears it, a second `esc` or `?` closes the help). The card keeps its full size however few rows the filter leaves, and every other key waits until it closes — `q` and `space` included |
| `o` | Settings card: everything in `config.toml` — replay gain, repeat, shuffle, the seven `[theme]` colours, the six `[ui]` switches and the library folder (the volume is not a row: it stays on `+`/`-`). `j`/`k` (or `↓`/`↑`) move between the settings (the `sound`/`theme`/`screen`/`library` headers are never focused), `g`/`G` jump to the ends, `enter` or `←`/`→` step the focused row (colours cycle a short preset list, the `folder` row opens the library prompt), `esc` closes — and every other key waits until it does, `q` and `space` included |
| `R` | Radio view, straight from either screen: the shortcut for `Sources` → `Radio` |
| `f` (radio view) | Favourite the selected station, or pin the selected country or region |
| `N` (radio view) | The country list, with your own country first when it knows it |
| `ctrl+r` (radio view) | Refetch the directory for the place you are in: the country list, a region's stations or a tag's |
| `q` | Quit |

While a search is open, every printable key types into the query,
`backspace` deletes, `enter` plays from the selected result, and `esc`
cancels — leaving the browser exactly where it was. The queue only lets you
edit entries after the one playing: the playing entry cannot be removed and
nothing moves across it, and each refusal explains itself in the status line.

The first screen is `Sources`: `Local library` and `Radio`, with the cursor
already on the library. `esc` walks back one level at a time; the top of the
library and the radio both land there, and a radio opened with `R` from the
library comes back to the library — the way out is always the way in. A
source's own keys stay its own once it is open. A launch that names files on
the command line skips the first screen: those files are what plays, and
there is no source to choose between.

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

## Radio

From `Sources`, choose `Radio` — or press `R` anywhere — for internet radio: the
[Radio Browser](https://www.radio-browser.info/)
directory — about 58,000 stations across 241 countries — plus the stations you add
yourself. It is a source beside your library and your Subsonic servers, not a
replacement for either.

Your favourites come first, at the top of the view: the stations you actually
listen to are one keypress away instead of buried under a country. Under them
come the country browser, your home country with its regions, your pinned
places, and a section of **stations without a country** — the ones you added by
hand and the favourites saved with no country code — so that radio is not only
radio inside a country.

Browse by country (your own first, and its regions when the directory records
them), by the community's genres and tags, or search a name. Every list can be
ordered by votes, listeners, trend, name or nothing at all, and a place brings at
most 200 stations — the directory's catalogue is not something you scroll.

A station is a **live stream**, so it differs from a file in ways the interface
says out loud: there is no position and no total, the clock row reads `● live`,
`←` and `→` refuse instead of pretending, and the title comes from the station's
own ICY metadata (`StreamTitle`) rather than from tags. A stream that fails — a
cut, a timeout, a station that answers with a playlist instead of audio — gets
exactly one retry before the queue moves on, and a station never joins the next
track as a gapless pair. A playlist wrapper (`.pls`, `.m3u`) is resolved one level
deep; an HLS manifest is refused with its own message before anything tries to
decode it, because its `.ts` segments are not audio this player can play.

Stations are **not** in the library index: they are not scanned, not searched with
`/` from the library, and they never appear in the artist, album or genre views.
Your favourites, your pinned places and your own stations live in
`~/.config/velo/radio.toml`, deduplicated by URL and written atomically, so a bad
edit in `config.toml` cannot take them with it. A URL that is already a favourite
is unmarked by pressing `f` again, however it is spelled (`https://A.example/stream`
and `https://a.example:443/stream/` are the same station).

The country used for "near you" is **opt-in**: velo offers the question once, reads
it from your locale (`LC_ALL`, `LC_MESSAGES`, `LANG`) and nothing else — no geo-IP,
no location service, and a `n` leaves the feature off for good. It is a
convenience for finding your own country in a list of 241, not a gate: the country
browser, the pins and the catalogue work the same either way.

What it does not do: no radius search around your location (the directory records
coordinates for about 12% of stations, so such a search would silently hide most of
the catalogue), no scrobbling to a remote account, and no station art — the
directory has none per station.

## ReplayGain

velo can measure a file's loudness the EBU R128 way and write the ReplayGain
2.0 tags the player then applies (see `replay_gain` in the configuration):

```sh
velo --replaygain-scan album/*.flac          # measure and print — a dry run
velo --replaygain-scan --apply album/*.flac  # write the tags
```

Every file passed in one invocation is measured as one album: the track gain
come from each file's own measurement, the album gain from all the files'
blocks pooled and gated together (EBU R128 / ITU-R BS.1770-4, −18 LUFS target,
absolute gate −70 LUFS, relative gate −10 LU). The dry run is the default —
nothing is ever written without `--apply`. A file that already carries artist,
title, comments or cover art keeps every one of them; only the four
`REPLAYGAIN_*` keys are added. The written peak is a **true peak**: the signal is
oversampled 4× with the interpolating filter ITU-R BS.1770-4 Annex 2 specifies,
and the largest absolute value of that oversampled signal is what gets written —
which catches the inter-sample overshoot a sample peak misses, and which is what
the clipping cap further up respects.

Each file is reported as `ok`, `skipped (no audio)` or `failed (reason)`; one
bad file never stops the run, and a read-only file is reported, not forced.

## Configuration

One TOML file, read once at startup, and the only place velo keeps anything:

```
$XDG_CONFIG_HOME/velo/config.toml      # else ~/.config/velo/config.toml
```

When the file does not exist, velo writes it with every key at its default
value and a short comment per key, so everything below can be changed from
it. Everything below can also be changed while velo runs: `o` opens the
settings card (a smaller card shaped like the help), where the
ReplayGain mode, the repeat and the shuffle, every `[theme]` colour, the
six `[ui]` switches and the library folder can be stepped through live —
each change applies on the next frame and is written to this file key by
key, just as if the matching key had been pressed. An existing file is
never overwritten or reformatted: every value velo
saves itself is written key by key, leaving your comments and your ordering
alone.

| Key | Meaning | Default |
|:--|:--|:--|
| `library` | The root folder to index and browse: an absolute path, `~/...` (your home) or a path relative to the directory velo starts in | none — written commented out (`# library = "~/Music"`) |
| `bit_perfect` | Decoder samples reach the device unmodified: no resampling, no ReplayGain, and the volume is pinned to 100%. A track at another rate reopens the device at its own rate (a small gap at that boundary); channel layouts still map; when the device has no such output, velo falls back to converting and says so once | `false` |
| `replay_gain` | ReplayGain mode: `track` (use the track's own gain), `album` (album gain, falling back to the track's), or `off` | `track` |

### `[state]` — where the next launch begins

| Key | Meaning | Default |
|:--|:--|:--|
| `volume` | Start level, `0..=100` | `100` |
| `shuffle` | Whether the queue is shuffled | `false` |
| `repeat` | What follows the end of a track: `off`, `all` or `one` | `off` |
| `view` | The tab to come back to inside `Local library`: `library` or `queue` | `library` |
| `browse_by` | The library view: `artists`, `albums`, `tracks` or `genres` | `artists` |
| `artist`, `album`, `genre` | The node that was open inside that view — `album` always belongs to `artist` | absent |

velo keeps this table up to date as you listen: the volume when it changes,
the tab and the node when you move, the shuffle and the repeat when you toggle
them. The next launch opens on `Sources` and comes back to this when you enter
`Local library`. A hand-written `sources` in `view` is accepted and ignored:
every launch opens there. The queue and the track that was
playing are not kept. `volume` used to live at the top level of the file; it
is still read there and moves under `[state]` the first time velo saves one.

### `[theme]` — the colours

Every value is `"#rrggbb"`. `"reset"` means the terminal's own colour and is
allowed for `background` and `text` only.

| Key | Meaning | Default |
|:--|:--|:--|
| `background` | The screen behind everything | `reset` |
| `text` | Body text and the values in a list | `reset` |
| `dim` | Hints and everything that should recede | `#767676` |
| `accent` | Borders, titles, the focused row and the selection | `#af00af` |
| `secondary` | The keys and labels next to the values | `#5f87d7` |
| `border` | The panes that are not in focus | `#767676` |
| `ink` | The text drawn on top of `accent` | `#000000` |

The brighter shade behind the accent is derived from `accent`, not written.

### `[ui]` — what is drawn

| Key | Meaning | Default |
|:--|:--|:--|
| `cover_art` | Draw the cover art pane at all | `true` |
| `spectrum` | Draw the spectrum under the now-playing block | `true` |
| `hints` | Draw the key hints on the last row | `true` |
| `top_bar` | Draw the tabs and the `V E L O` name | `true` |
| `advisories` | Let the status line explain why something is not drawn — today, a cover the window is too small for. Errors and ordinary messages are never silenced | `true` |
| `view_numbers` | Number the library views in the header (`1 artists  2 albums  3 tracks  4 genres`) | `true` |

Precedence: `--library` (expanded the same way) beats the config file. With
no library set, the interface no longer demands a folder: when a `[subsonic]`
server is configured, the synced index *is* the library and `/` searches it;
when neither a library nor a server is configured, one sentence in the
interface says what to set up. `L` still opens the folder prompt at any time
— it starts at `~/`, which cannot be deleted, and `tab` completes folder
names the way a shell does (several matches are listed under the field). A
path that is not a directory is refused in the prompt; an accepted one is
saved to the file as `library = "~/<what you typed>"` (every other line
is kept) and indexed right away; `esc` skips it. `velo --index` without a
library exits 2 and names the key and the flag. An unknown key is an error
naming the key; a bad value names the file and the line.

### Subsonic servers (early access)

velo can talk to a Subsonic-compatible server (Navidrome and friends).
This is early access, and playback works by caching: what exists today is
the connection and its authentication, one command that answers
"can I use this server?", one command that syncs the server's library into
the index so browsing and fuzzy search reach it, and playback of a remote
track through that cache:

```sh
velo --subsonic-test
velo --sync
```

`--subsonic-test` prints the server's type, version, API version and the
OpenSubsonic extensions it advertises, says which authentication it used and
whether it worked, and exits `0` on success and `1` with a one-line reason on
failure. It needs no interactive terminal, so it works over ssh.

`velo --sync` fetches the server's albums (`getAlbumList2`, paged, and one
`getAlbum` per album — never one request per track) and writes them as rows
of the same index the local scan fills, marked with their source
(`subsonic:<host>`). It then prints the counts per source:

```
synced subsonic:music.example.com: 3212 tracks, 310 albums, 4 removed, 311 requests
local: 104 tracks · subsonic:music.example.com: 3212 tracks
```

Syncing is explicit — it never runs on startup. A failed sync writes
nothing; a successful one also removes this server's rows the server no
longer holds (the removed count says how many). The remote rows appear in
the artist, album, track and genre views and in `/` search like local ones,
and pressing `enter` (or `a`, to queue one) plays a remote track like a
local one.

### How remote playback works

Playing a remote track is cache-on-play: the track's original bytes are
fetched once with `stream?id=<id>&format=raw` (the server is asked for no
transcoding, so what lands on disk is what it holds) and then played from
that file. Seek, gapless, bit-perfect and ReplayGain-from-tags all keep
working, because the audio path opens a file either way. A cache hit needs
no network at all; a download that fails skips that track with the reason in
the status line and the queue keeps going — a network dying mid-album does
not end the session.

The cache lives at `$XDG_CACHE_HOME/velo/subsonic`, else
`~/.cache/velo/subsonic`; each file is named after a hash of the track's
stream URL. It is capped at 2 GiB: after each download, when the directory
holds more than that, the oldest files (by modification time) are removed,
whole files only, until it is under the cap again — the file that was just
downloaded is never the one removed, so playing one enormous track cannot
evict the very thing it is playing. The covers cache is capped the same way
at 256 MiB. To empty both caches on demand, run `velo --cache-clean`: it
prints, per cache, how many files and how much space it freed and touches
nothing outside the two directories.

### Remote covers and scrobbling

A remote track's cover comes from the server too. The sync stores each
song's `coverArt` id (the song's own, else its album's) in the index; when a
remote track plays, the interface fetches it once with
`getCoverArt?id=<cover_id>&size=600` into the covers cache —
`$XDG_CACHE_HOME/velo/covers`, else `~/.cache/velo/covers`, one image file
per cover id — and draws it exactly like a local track's cover. The fetch
runs off the drawing path, so a slow or missing cover means no cover, never
a stall or an error. A local track's cover keeps coming from its tags or
folder.

While a remote track plays, velo tells the server: `scrobble` with
`submission=false` when the track starts (now playing), and one
`submission=true` once it has played long enough to count — half its
duration or four minutes, whichever comes first, the same rule the servers
themselves apply. Scrobbles are sent from a background thread and a failing
one is never shown; local tracks are never scrobbled.

Add the table to the same `config.toml` (it must come after the other keys;
`url` and `user` are required when the table exists):

```toml
[subsonic]
url = "https://music.example.com"
user = "you"
# api_key = "the-key-from-the-server"   # see below
# insecure = false                      # true accepts a self-signed certificate
```

The secret is resolved in this order: `api_key` in the table, then the
`VELO_SUBSONIC_SECRET` environment variable, then the first line of the file
`$XDG_CONFIG_HOME/velo/subsonic.key` (else `~/.config/velo/subsonic.key`),
which you create yourself. When that file is readable by group or others,
velo warns and names the path and the mode — restrict it with `chmod 600`.
The warning is said where you are looking: once, in the interface's status
line, when velo starts with a Subsonic server configured (and again on
stderr for `velo --subsonic-test` and `velo --sync`).

The wire format is JSON; a server too old to answer JSON is reported as too
old, never parsed as XML. When the server advertises the `apiKeyAuthentication`
extension the secret is sent as an API key, otherwise as token authentication
(`t = md5(password + salt)` with a fresh salt per request). An unreachable
server is reported as unreachable — never as a credential problem.

## Requirements

- Linux or macOS. Windows is untested.
- To build from source on Linux, the ALSA development headers
  (`libasound2-dev` on Debian/Ubuntu); the audio output goes through cpal.
- **Opus needs one of two things at build time**: `cmake` (used to build the
  bundled libopus from source), or a system libopus that `pkg-config` can find
  (`brew install opus`, `apt install libopus-dev`) — in which case that one is
  linked statically and cmake is never called. Either way nothing is needed at
  runtime. Without both, the build stops in `audiopus_sys` with a message about
  cmake, which is the honest place for it to stop. **With CMake 4** (the current
  release) the bundled libopus needs one more word, because its build files
  predate CMake 3.5 and CMake 4 refuses those by default:
  `CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo install velo`. A clone of this
  repository carries that setting in `.cargo/config.toml` already; a published
  crate cannot, which is why it is written here.
- A terminal. The interface needs an interactive terminal and refuses to run
  otherwise.
- Cover art is drawn when the terminal speaks the kitty, sixel or iTerm2
  protocol and the window is at least 100×28. Without one of those protocols
  there is no art pane at all — the layout is unchanged, and the status line
  says why a cover is hidden rather than faking one with half-blocks (a line
  `[ui] advisories = false` silences). The
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
than misapplied. To fill those tags in from a measurement, see
`--replaygain-scan` above.

## Not yet

Honest list; none of these exist behind a flag:

- Subsonic/Navidrome, still early access: the connection, authentication,
  `velo --subsonic-test`, `velo --sync`, `velo --cache-clean`, cache-on-play
  playback with capped, self-pruning caches, remote covers and scrobbling
  all exist (see above). Streaming over HTTP ranges (playing before the
  whole track is on disk) is a later optimisation.
- Playlists.

MP3, FLAC, M4A/AAC, OGG Vorbis, OGG Opus and WAV are what plays today, decoded
with symphonia 0.6 (Opus through a statically linked libopus behind `audiopus`,
pinned exactly at `0.3.0-rc.0` because that is the version whose `audiopus_sys`
builds libopus with cmake instead of autotools — an RC that does not drift is
worth more than a stable version that needs `autoconf`, `automake` and
`libtool` on every machine that installs this). No dynamic dependency. Sample
rates are
converted to your output device; channel layouts are not converted, except
mono, which plays through every output channel — a layout that would need a
real downmix (5.1 into stereo, say) is skipped with an announcement instead of
being silently averaged away.

Gapless joins trim what the encoder inserted, so a seam carries music only: for
MP3 the LAME tag's delay and padding are honoured, and for M4A/AAC — where
symphonia reports neither, measured on 0.5.5 and 0.6.1 alike — the priming is
read from the container itself
(the `elst` edit list, or an `iTunSMPB` tag when one is present) and trimmed.
Durations and seeks count content frames, not the raw encoded frames. A seek
also discards the codec's own declared warm-up after the jump — for Opus, the
normative pre-skip (RFC 7845) that the decoder re-runs after a reset — so the
stream resumes with the music, not the decoder's re-priming.

With `bit_perfect = true` in the config, sample rates are not converted
either: the device is reopened at each track's own rate, so the decoder's
samples reach it unmodified. ReplayGain is skipped and the volume is pinned
to 100%, because both would scale the samples; `+`/`-` cannot move it, and
one status line explains that. The reopen costs a small gap between tracks of
different rates; consecutive tracks that share a rate stay one continuous
stream, and channel layouts still map the way they do without the setting.
When the device has no output at a track's rate, velo falls back to
converting and says so once — a track is never lost to bit-perfect mode.

## Contributing

Two commands get you from a clone to a running binary, and the third checks your
change the way CI will:

```sh
cargo build --release
./target/release/velo ~/Music
cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test
```

`CONTRIBUTING.md` has the platform dependencies, the commit convention, and what
a reviewable slice looks like. Bug reports and feature requests have templates;
a report that includes your platform, your terminal and `velo --info` is worth
ten without them.

## License

MIT — see [LICENSE](LICENSE).
