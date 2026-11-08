# Changelog

Every release of velo, newest first, generated from its Conventional Commits.
## [0.10.2] - 2026-11-08

### Fixes
- **ci:** Install alsa headers and cmake in the cross container

### Maintenance
- Repo hygiene — contributing guide, issue templates, CI build deps (#1)
## [0.10.1] - 2026-11-08

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

### Tests
- **aac:** Pin the content, not the float bits, in the priming baseline

### Maintenance
- **deps:** Migrate to symphonia 0.6.1 T15
- **release:** Make the release buildable by hand, and fix the artifact path

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
