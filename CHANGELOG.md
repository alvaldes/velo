# Changelog

Every release of velo, newest first, generated from its Conventional Commits.
## [0.34.2] - 2026-11-10

### Fixes
- **radio:** Enter answers the country question

### Documentation
- **readme:** Name the servers velo already speaks to
## [0.34.1] - 2026-11-10

### Fixes
- **theme:** The preview is not masked by the config's own defaults
## [0.34.0] - 2026-11-10

### Features
- **keys:** The cliamp keymap, where velo has the same verb
## [0.33.0] - 2026-11-10

### Features
- **ui:** The spectrum becomes a choice of nine visualizers
## [0.32.1] - 2026-11-09

### Fixes
- **audio:** The volume is a perceptual curve, not an amplitude
## [0.32.0] - 2026-11-09

### Features
- **theme:** The card shows the theme, and a picker previews it live
## [0.31.0] - 2026-11-09

### Features
- **ui:** Lists wrap at their ends, except the queue
## [0.30.3] - 2026-11-09

### Fixes
- **ui:** The radio owns the header too, not only the body
## [0.30.2] - 2026-11-09

### Fixes
- **radio:** A channel's songs are named, and a channel is probed once

### Maintenance
- **ci:** Give the old tags' cross image the dbus headers too
## [0.30.1] - 2026-11-09

### Fixes
- **deps:** Souvlaki over its pure-Rust D-Bus backend

### Maintenance
- **ci:** The Linux builds need libdbus for souvlaki
## [0.30.0] - 2026-11-09

### Features
- **radio:** A configured channel with songs opens as a list

### Documentation
- **readme:** Breathe after the media-keys heading
## [0.29.0] - 2026-11-09

### Features
- **playlists:** Import and export .m3u and .pls
## [0.28.0] - 2026-11-09

### Features
- **power:** A low-power mode, and the media keys reach velo
## [0.27.0] - 2026-11-09

### Features
- **playlists:** Save, name and reorder a queue that outlives the session
## [0.26.0] - 2026-11-09

### Features
- **theme:** The themes folder ships velo's own palette
## [0.25.0] - 2026-11-09

### Features
- **saved:** Mark tracks, and a library.toml that outlives the session
## [0.24.0] - 2026-11-09

### Features
- **config:** A fresh install finds every file it can edit by hand

### Documentation
- **config:** The generated file names the theme and its folder
## [0.23.0] - 2026-11-09

### Features
- **theme:** Themes are named files you can hand to someone
## [0.22.0] - 2026-11-09

### Features
- **queue:** A station in the queue reads as live, and next leaves it
## [0.21.0] - 2026-11-09

### Features
- **radio:** `--radio-test` answers what a stream really is

### Refactoring
- **cli:** Gather the one-shot modes in one place
## [0.20.0] - 2026-11-09

### Features
- **audio:** Force a stereo file to play as mono

### Maintenance
- **ci:** Name the tag in the manual release path
## [0.19.0] - 2026-11-09

### Features
- **history:** Record the stations that begin playing
## [0.18.0] - 2026-11-09

### Features
- **history:** Keep what was played recently in history.toml
- **ui:** Record every beginning in the history file
- **cli:** Read the history without opening the interface
- **ui:** A `Recently played` source lists the history

### Documentation
- **readme:** A file named history needs a path now
## [0.17.0] - 2026-11-09

### Features
- **radio:** Search the directory by station name
## [0.16.0] - 2026-11-09

### Features
- **radio:** The directory host comes from the config
## [0.15.0] - 2026-11-09

### Features
- **ui:** A track can be seeked thirty seconds at a time
- **ui:** `:` jumps to a position inside the playing track
## [0.14.0] - 2026-11-09

### Features
- **ui:** The volume has a one-point gear between the five-point steps

### Fixes
- **ui:** The help card covers the radio view it is opened over
## [0.13.0] - 2026-11-09

### Features
- **radio:** A station that cannot play says why and can be reported
- **radio:** The refusal note names the key that reports it

### Fixes
- **radio:** Mark a station that cannot play wherever it appears
- **radio:** The star comes before the cross
- **radio:** Mark the station that is playing, wherever it is listed
- **config:** Retire the recorded tab that no longer opens a screen

### Documentation
- The status line gives the reason in a couple of words, and names i
## [0.12.0] - 2026-11-09

### Features
- **ui:** Open on a sources view and walk back to it
- **radio:** Favourites first and a section for stations with no country
- **ui:** Tab round trip, truthful radio screens and favourites below

### Fixes
- **ui:** Name sources in the title bar, land the library on the library

### Documentation
- Document the sources view and the new radio order
- Document the tab round trip, the radio order and the waits
- The recorded tab no longer chooses a screen
## [0.11.0] - 2026-11-09

### Features
- **radio:** ICY metadata demux keeps the audio stream clean T1
- **radio:** Wrapper playlists resolve to the first playable entry T2
- **radio:** Radio Browser directory client and codec filter T3
- **radio:** Radio.toml store with atomic saves and station identity T4
- **radio:** The country comes from the locale, not a timezone table T7
- **ui:** A radio view with sections, filter and order card T6a
- **ui:** The radio view loads its data without blocking the loop T6b
- **audio:** Decode a live stream from a media source, with one decoder factory T5a
- **radio:** Open a station URL as a live stream with per-operation timeouts T5b1
- **radio:** A wrapper playlist resolves one level, and HLS says so T5b2
- **radio:** The build decides playability, and an HLS station is marked before the queue
- **audio:** A station sits in the queue and plays T5c2a
- **ui:** The home country's regions are reachable and pinnable T6d
- **audio:** One retry for a live station, spent per turn T5c2b
- **ui:** A station chosen in the radio view plays, and reads as live T6c

### Fixes
- **radio:** Read the codec capability from symphonia 0.6's audio registry

### Refactoring
- **audio:** The queue carries entries, not bare paths T5c1

### Documentation
- **audio:** A normative pre-skip is not encoder padding
- The cross container has neither ALSA headers nor cmake, and Cross.toml says so (#2)
- **changelog:** Fold 0.10.1 into 0.10.2 after its tag was removed
- **radio:** The feature, its keys and its refusal to fake a position T6c-ii

### Tests
- **ui:** Stop racing the loader in the wiring tests

### Merge
- Main into feat/radio after the seam landed upstream
- Main into feat/radio for the opus capability
- Main with T14b and the pre-skip fix

### Style
- **radio:** Rustfmt the radio modules and their tests
## [0.10.2] - 2026-11-08

### Features
- **audio:** Bit-perfect playback reopens the stream per track T17
- **subsonic:** A client that speaks the protocol and knows why it failed T12a
- **library:** The remote library becomes rows in the index T12b
- **subsonic:** A remote track plays from the cache T12c
- **subsonic:** Remote covers and scrobbling T12d
- **subsonic:** Prune the caches and say when the key is readable
- **audio:** Measure and write ReplayGain T13
- **audio:** True peak, and tags proved on the formats people have
- **audio:** An Opus decoder, and one mouth for codec playability T14
- **audio:** Decode a live stream from a media source, with one decoder factory T5a
- **audio:** Opus plays, pre-skip trimmed T14b

### Fixes
- **audio:** Trim the encoder priming of an M4A T16
- **audio:** A live Opus stream trims its pre-skip too
- **audio:** A seek does not emit the codec's warm-up
- **build:** Build libopus with cmake, not autotools
- **build:** Tell CMake 4 the policy floor audiopus_sys needs
- **ci:** Install alsa headers and cmake in the cross container

### Tests
- **aac:** Pin the content, not the float bits, in the priming baseline

### Maintenance
- **deps:** Migrate to symphonia 0.6.1 T15
- **release:** Make the release buildable by hand, and fix the artifact path
- Repo hygiene — contributing guide, issue templates, CI build deps (#1)

### Merge
- Bit-perfect (T17) into main
- The Subsonic client (T12a) into main
- The remote library index (T12b) into main
## [0.9.0] - 2026-11-07

### Features
- **config:** The file carries the state, the theme and the ui
- **ui:** The palette comes from the theme table
- **ui:** Cover, spectrum, hints and the top bar can be switched off
- **config:** Velo can write any key of its own file
- **ui:** The session comes back, and every change is written down
- **ui:** O opens the settings card
- **ui:** The card drops the volume and gains two switches
## [0.8.1] - 2026-11-07

### Fixes
- **ui:** The help card keeps its height while the filter narrows it
## [0.8.0] - 2026-11-07

### Features
- **ui:** The help scrolls with j/k and filters with /

### Fixes
- **ui:** An open search follows the index, u says why it cannot refresh, and a reload reads one snapshot
## [0.7.0] - 2026-11-07

### Features
- **ui:** L sets the library folder at any time
- **ui:** Index in the background on launch and on u, with an indicator

### Fixes
- **library:** An old index in a read-only file opens again, with genres unknown
- **library:** A read-only index reads the rows still in its WAL, and a reload keeps the tab
## [0.6.0] - 2026-11-07

### Features
- **library:** Browse by artist, album, track or genre with the keys 1 to 4
- **ui:** The library prompt starts at ~/ and tab completes folders

### Fixes
- **ui:** Enter on a command-line file plays from that file onwards

### Maintenance
- Keep session worktrees out of the tree
## [0.5.0] - 2026-11-07

### Features
- **config:** Write every default to config.toml and ask for the library in a prompt

### Fixes
- **config:** An empty library is no library, and the prompt saves a path that loads anywhere
## [0.4.1] - 2026-11-07

### Fixes
- **ui:** Set the cover inside the now-playing block, beside the title
## [0.4.0] - 2026-11-07

### Features
- **audio:** Repeat the queue or the track at the producer's boundary
- **ui:** S shuffles what is left of the queue and r cycles repeat

### Fixes
- **audio:** A repeated track with no audio no longer spins the producer
## [0.3.0] - 2026-11-07

### Features
- **ui:** N and p skip to the next and the previous track
## [0.2.0] - 2026-11-07

### Features
- **ui:** A framed window with a title bar and the views as tabs T1
- **ui:** A now-playing block with clock, state pill, progress, transport and volume T2
- **ui:** Every list as numbered rows under a header line T3
- **ui:** The help as a rounded card with keys apart from what they do T4
- **audio:** A lock-free tap of what the device plays, for the spectrum T5a
- **audio:** Fold the tapped samples into log-spaced spectrum bands T5b
- **ui:** A spectrum of eighth-block bars in the now-playing block T5c
- **ui:** Wear the terminal's own colors instead of a fixed palette T8
- **ui:** Name the ReplayGain mode beside the volume T10

### Fixes
- **ui:** Show the folder's cover when the file embeds none T6
- **ui:** Keep the volume slider still when its number loses a digit T7
- **ui:** Redraw at thirty frames only while the spectrum is on screen T9

### Documentation
- Show the redesigned interface in the README

### Maintenance
- Cut a release per delivered change from its Conventional Commits
## [0.1.0] - 2026-11-07

### Features
- **audio:** Gapless decode and lock-free output
- **audio:** Resample tracks to the output format
- **ui:** Terminal shell with a testable keymap
- **library:** Index the music root into SQLite
- **ui:** Browse the library and play from it
- **audio:** Transport with volume, pause fade and seek
- **audio:** Skip a broken track instead of playing silence T19
- **audio:** Map the channel layout instead of refusing it T20
- **ui:** Show the track length in command-line mode T21
- **ui:** A queue you can see and edit T18b
- **ui:** Fuzzy search over the library T7
- **ui:** Cover art beside the header T8
- **cli:** Clap, a config file, and honest help T9
- **ui:** A hidden cover says why it is hidden T8b
- **audio:** Apply ReplayGain from the tags T11

### Fixes
- **audio:** Keep the level across a seek and count the clock in output frames
- **audio:** Count the seek offset in samples, not frames

### Refactoring
- **audio:** Share the queue with whoever edits it T18 part 1

### Maintenance
- Scaffold velo and record the ODD feature plan
- Make velo installable by a stranger T10

### Merge
- Audio foundation and terminal shell
