# Changelog

Every release of velo, newest first, generated from its Conventional Commits.
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
