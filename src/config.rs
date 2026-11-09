//! The command line and the config file (T9, S12).
//!
//! Everything the user can set outside the interface lives here: the [`Cli`]
//! parsed by [`clap`], and the optional `config.toml` loaded into a
//! [`Config`] and merged with the parsed command line into a [`Settings`].
//!
//! Precedence is one rule, stated once in [`Config::merged`]: the command
//! line wins over the config file, the config file wins over the built-in
//! default.
//!
//! The merged [`Settings`] resolves every key into what the binary runs with:
//! `bit_perfect` is consumed by the audio path (T17) — it reopens the device
//! at each track's own rate instead of resampling, pins the volume to 100%
//! and skips `ReplayGain` — and `replay_gain` is what the audio path reads
//!
//! feature that consumes them arrives. `replay_gain` arrived the other way
//! round (T11, S15): its [`ReplayGainMode`] is what the audio path reads.
//!
//! Since S1 of the config-and-settings feature this one file also carries what
//! the interface needs: the `[state]` table (what the next launch comes back
//! to), the `[theme]` table (every colour, as `"#rrggbb"`) and the `[ui]`
//! table (which parts of the screen are drawn). Nothing else is stored
//! anywhere: no state file, no settings table in the library index.

use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use serde::Deserialize as _;
use serde::de::{self, Deserializer, Visitor};

use crate::audio::output::Repeat;
use crate::ui::{BrowseBy, View};

/// The volume the player has always started at (T3 kept it at full).
pub const DEFAULT_VOLUME: u8 = 100;

/// The Radio Browser host the directory asks through when the config names
/// none (issue #14): the community's published round-robin across its
/// mirrors, not one mirror's box.
pub const DEFAULT_RADIO_HOST: &str = "all.api.radio-browser.info";

/// The way `ReplayGain` is applied (T11, S15): `track` is the default, `album`
/// prefers the album tags and falls back to the track tags, `off` applies
/// nothing. serde's own unknown-variant error is the strictness the config
/// wants: it names the value it refused and every allowed one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReplayGainMode {
    /// Apply the track's own gain and peak (the default).
    #[default]
    Track,
    /// Apply the album gain and peak when the track carries them, otherwise
    /// the track's.
    Album,
    /// Apply no `ReplayGain` at all.
    Off,
}

/// The `[subsonic]` table as the config carries it, with the optionals
/// resolved: the file's `insecure` defaults to false (T12a).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubsonicConfig {
    /// The server base URL, e.g. `https://music.example.com`.
    pub url: String,
    /// The Subsonic user.
    pub user: String,
    /// The API key, when the user wrote one; the credential resolver falls
    /// back to the environment and the key file without it.
    pub api_key: Option<String>,
    /// Accept self-signed certificates, for a server on the home network.
    pub insecure: bool,
}

/// The parsed command line.
///
/// The observable contract is the one the hand-rolled loop had: positional
/// files, `--info`, `--play`, `--index`, `--library <PATH>`, `-h`/`--help`;
/// exit code 2 for a usage error, 1 for a missing file or library root, 0
/// for `--help` and for success.
#[derive(Debug, Parser)]
#[command(
    name = "velo",
    version,
    args_conflicts_with_subcommands = true,
    about = "A fast, single-binary music player for your terminal",
    long_about = "A fast, single-binary music player for your terminal.

With files it plays them as one continuous, gapless stream. Without files it \
browses the library index in a terminal interface with a queue you can edit, \
fuzzy search and cover art.",
    after_help = "File modes:
    velo <file>...               Open the terminal interface over the files
    velo --info <file>...        Report format, frame count and length, then exit
    velo --play <file>...        Play as one continuous stream and exit
Library mode:
    velo                         Browse and play the library index
    velo --index                 Build or refresh the library index, print
                                     track, album and artist counts, then exit
Subsonic:
    velo --subsonic-test         Probe the configured Subsonic server, report
                                     what it supports, then exit
    velo --sync                  Sync the server's library into the index
                                     (browse and search reach it; playback
                                     comes with the cache), print counts
                                     per source, then exit
    velo --cache-clean           Empty both Subsonic caches (tracks and
                                     covers), print per cache how many
                                     files and how much space were freed,
                                     then exit
History:
    velo history                 What was played recently, newest first, one
                                     line per entry, then exit
    velo history --json          The same entries as a JSON array, for a
                                     script, then exit
    velo history clear           Empty the history file, then exit — all
                                     three run without a terminal
ReplayGain:
    velo --replaygain-scan <file>...   Measure each file (all files in one
                                     invocation are one album) and print the
                                     ReplayGain tags velo would write — a dry
                                     run; nothing is ever written
    velo --replaygain-scan --apply <file>...
                                 Write those tags into the files, keeping
                                     every tag they already carry

Interface keys: j/k move, enter open/play, tab switches to the queue view
    (a append, d remove, J/K move, enter play from there), / search, ←/→ or
    ,/. seek ±5 s, space pause, +/- volume, ? help, q quit. Cover art is
    drawn when the terminal supports the kitty, sixel or iTerm2 protocols.

Audio must share one channel layout: sample rates are converted to the
    output device, channel layouts are not. With bit_perfect in the config,
    rates are not converted either — the device is reopened at each track's
    own rate, the volume is pinned to 100% and ReplayGain is skipped. With
    --mono, both channels carry the average of the mix; the combination with
    bit_perfect is refused.

Configuration: $XDG_CONFIG_HOME/velo/config.toml, or
    ~/.config/velo/config.toml — keys: library, bit_perfect, replay_gain, and
    the tables [state] (volume, shuffle, repeat and the open node),
    [theme] (every colour, as \"#rrggbb\"), [ui] (cover_art, spectrum, hints,
    top_bar, advisories, view_numbers) and the optional [subsonic] table (url,
    user, api_key, insecure) for a Subsonic-compatible server; `velo
    --subsonic-test` answers \"can I use this server?\" and works over ssh;
    `velo --radio-test <URL>` probes one live station URL the same way.
    When the file is missing, velo writes it with every key at its default and
    a comment per key; an unknown key is an error. library (and --library)
    takes an absolute path, ~/... or a path relative to the current directory.
    With no library set, the interface no longer demands a folder: with a
    [subsonic] server configured the synced index is the library, and with
    neither, one sentence says what to configure; press L to enter a folder
    in a floating prompt that starts at ~/ (tab completes folders) and saves
    the answer to the file; velo --index needs one and exits 2 without."
)]
// The four flags are four distinct modes the user asks for; folding them
// into an enum would not simplify the parse or the match in main.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent mode the user asks for"
)]
pub struct Cli {
    /// Tracks to play as one continuous, gapless stream. Without files, the
    /// terminal interface browses the library index.
    #[arg(value_name = "FILE")]
    pub files: Vec<PathBuf>,

    /// Report format, frame count and length of each file, then exit.
    #[arg(long)]
    pub info: bool,

    /// Play the files and exit, without the terminal interface.
    #[arg(long)]
    pub play: bool,

    /// Start this session in mono: both output channels carry the average of
    /// the mix, after `ReplayGain`, before the ring. `M` toggles it while
    /// playing. A session setting only — there is no config key, and nothing
    /// is persisted — and refused when the config turns `bit_perfect` on
    /// (issue #8).
    #[arg(long)]
    pub mono: bool,

    /// Build or refresh the library index, print its counts, then exit.
    #[arg(long)]
    pub index: bool,

    /// Probe the configured Subsonic server: report what it is, what it
    /// advertises and which authentication worked, then exit.
    #[arg(long)]
    pub subsonic_test: bool,

    /// Probe one live radio stream URL (issue #13): resolve one wrapper
    /// level, report the URL it really points at, the response headers that
    /// matter (`content-type`, `icy-metaint`, redirects), the codec, whether
    /// this build can play it and the result of decoding the first seconds
    /// of audio, then exit. Exit 0 when the URL plays end to end, 1 with a
    /// one-line reason when it does not.
    #[arg(long, value_name = "URL")]
    pub radio_test: Option<String>,

    /// Sync the configured Subsonic server into the library index: fetch
    /// every album, write the rows, print the counts per source, then exit.
    /// Never runs automatically.
    #[arg(long)]
    pub sync: bool,

    /// Remove every cached remote track and cover from both Subsonic caches
    /// (the track cache and the covers cache), print per cache how many
    /// files and how much space were freed, then exit. Nothing outside the
    /// two cache directories is ever touched.
    #[arg(long)]
    pub cache_clean: bool,

    /// Measure EBU R128 loudness for each file (every file in one invocation
    /// is one album) and print the `ReplayGain` tags velo would write — a dry
    /// run: nothing is ever written without `--apply` (T13).
    #[arg(long)]
    pub replaygain_scan: bool,

    /// With `--replaygain-scan`: write the measured `ReplayGain` tags into the
    /// files, preserving every tag they already carry. Without the scan this
    /// is a usage error; no other velo command ever writes to a music file.
    #[arg(long, requires = "replaygain_scan")]
    pub apply: bool,

    /// Library root to index or browse; overrides `library` in the config file.
    #[arg(long, value_name = "PATH")]
    pub library: Option<PathBuf>,

    /// A headless answer, `velo history …`. Every other mode stays a flag.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// The one subcommand the command line carries: `velo history`, in its three
/// shapes — plain, `--json`, `clear`.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// What was played recently, newest first.
    #[command(args_conflicts_with_subcommands = true)]
    History {
        /// Print the entries as a JSON array instead of one line each.
        #[arg(long)]
        json: bool,
        /// Empty the history.
        #[command(subcommand)]
        action: Option<HistoryAction>,
    },
}

/// A history action: today only emptying the file.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum HistoryAction {
    /// Empty the history file.
    Clear,
}

/// The config file as the user wrote it: every key optional, nothing filled in.
///
/// The three tables below are read as written — [`StateKeys`], [`ThemeKeys`],
/// [`UiKeys`] — and [`Config::merged`] resolves each of them with its defaults.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// `library` — the library root to index and browse.
    pub library: Option<PathBuf>,
    /// `volume` at the top level: where the key lived before `[state]`
    /// existed. Still read, so an older file keeps working; `[state] volume`
    /// wins over it, and every write goes to `[state]` from then on (S2).
    #[serde(default, deserialize_with = "deserialize_volume")]
    pub volume: Option<u8>,
    /// `bit_perfect` — the decoder's samples reach the device unmodified:
    /// no resampling, no `ReplayGain`, and the volume pinned to 100% (T17).
    pub bit_perfect: Option<bool>,
    /// `replay_gain` — which `ReplayGain` tags the audio path applies (T11,
    /// S15); the audio path reads it through [`ReplayGainMode`].
    #[serde(default)]
    pub replay_gain: Option<ReplayGainMode>,
    /// `[state]` — what the next launch comes back to (S2, S3).
    #[serde(default)]
    pub state: StateKeys,
    /// `[theme]` — every colour the interface draws with (S4).
    #[serde(default)]
    pub theme: ThemeKeys,
    /// `[ui]` — which parts of the screen are drawn (S5).
    #[serde(default)]
    pub ui: UiKeys,
    /// `[subsonic]` — the Subsonic-compatible server (T12a), when the
    /// table exists. `url` and `user` are required inside it.
    #[serde(default)]
    pub subsonic: Option<SubsonicFile>,
    /// `[radio]` — the Radio Browser directory host (issue #14), when the
    /// table exists. `host` is optional inside it.
    #[serde(default)]
    pub radio: Option<RadioFile>,
}

impl Config {
    /// Loads the config file at `path`.
    ///
    /// A missing file is normal and silent: it yields the defaults, because a
    /// user who never wrote a config has not made a mistake. Any other read
    /// failure, a value of the wrong type or range, or an unknown key is an
    /// error naming the file and the line.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be read or does not parse.
    pub fn load_from(path: &Path) -> Result<Self, ConfigError> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(ConfigError {
                    message: format!("{}: {error}", path.display()),
                });
            }
        };

        toml::from_str(&text).map_err(|error| ConfigError {
            message: parse_error_message(path, &error, &text),
        })
    }

    /// Merges the config file with the command line: the command line wins
    /// over the config file, the config file wins over the built-in default.
    #[must_use]
    pub fn merged(self, cli: &Cli) -> Settings {
        let node = self.state.open_node();
        Settings {
            // An empty `library` in the file is no library: the prompt asks.
            library: cli.library.clone().or(self
                .library
                .filter(|library| !library.as_os_str().is_empty())),
            volume: self.state.volume.or(self.volume).unwrap_or(DEFAULT_VOLUME),
            bit_perfect: self.bit_perfect.unwrap_or(false),
            // `--mono` is a session request alone (issue #8, S2): the config
            // file carries no such key, so there is nothing to merge it with.
            mono: cli.mono,
            replay_gain: self.replay_gain.unwrap_or(ReplayGainMode::Track),
            session: Session {
                shuffle: self.state.shuffle.unwrap_or(false),
                repeat: self.state.repeat.unwrap_or_default(),
                view: self.state.view.unwrap_or_default(),
                browse_by: self.state.browse_by.unwrap_or_default(),
                node,
            },
            theme: self.theme.resolved(),
            ui: self.ui.resolved(),
            subsonic: self.subsonic.map(|server| SubsonicConfig {
                url: server.url,
                user: server.user,
                api_key: server.api_key,
                insecure: server.insecure.unwrap_or(false),
            }),
        }
    }
}

/// What the binary actually runs with, after the command line, the config
/// file and the defaults have been resolved.
///
/// `bit_perfect` is consumed by the audio path (T17): the device stream is
/// reopened at each track's own rate instead of resampling, the volume is
/// pinned at 100% and `ReplayGain` is skipped.
/// `replay_gain` is consumed by the audio path (T11): the gain is applied in
/// the producer, once per track, before the ring. `session`, `theme` and `ui`
/// are what the interface needs on its very first frame (S2–S5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// The library root as written (`--library`, else the file), unexpanded;
    /// `None` when neither names one. [`expand_path`] resolves it.
    pub library: Option<PathBuf>,
    pub volume: u8,
    pub bit_perfect: bool,
    /// Whether the session starts in mono (issue #8, S2): `--mono` on the
    /// command line, never a config key, never persisted. The interface and
    /// the `--play` path hand it to the player; the interface's `M` key
    /// toggles it through the shared cell.
    pub mono: bool,
    pub replay_gain: ReplayGainMode,
    /// What the next launch comes back to (S2, S3).
    pub session: Session,
    /// The colours the interface draws with (S4).
    pub theme: ThemeColors,
    /// Which parts of the interface are drawn (S5).
    pub ui: UiParts,
    /// The Subsonic server (T12a), when the config has the table.
    pub subsonic: Option<SubsonicConfig>,
}

/// Where the next launch picks up (S3): the library view and the node that
/// was open when velo last ran. The recorded tab (`view`) is retired (S14):
/// it is kept here — and read from the file — for compatibility only, and
/// is no longer written or applied.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Session {
    /// Whether the queue is shuffled (S2).
    pub shuffle: bool,
    /// What follows the end of a track (S2).
    pub repeat: Repeat,
    /// The tab the file recorded, read for compatibility (S14): it still
    /// parses, but it is no longer written and no longer applied — a launch
    /// opens `Sources`.
    pub view: View,
    /// The library view it was showing (S3).
    pub browse_by: BrowseBy,
    /// The node that was open inside that view (S3).
    pub node: Option<OpenNode>,
}

/// The library node that was open (S3): an artist, one of that artist's
/// albums, or a genre. Which names matter is decided by the view: `albums`
/// with an artist and an album is that album's tracks, `albums` with neither
/// is every album, `genres` with a genre is that genre's tracks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenNode {
    /// The open artist, when the view is inside one.
    pub artist: Option<String>,
    /// The open album, which always belongs to [`OpenNode::artist`].
    pub album: Option<String>,
    /// The open genre, in the `genres` view.
    pub genre: Option<String>,
}

/// The `[state]` table as the file writes it (S2, S3): every key optional.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateKeys {
    /// `volume` — an integer between 0 and 100, inclusive.
    #[serde(default, deserialize_with = "deserialize_volume")]
    pub volume: Option<u8>,
    /// `shuffle` — whether the queue is shuffled.
    pub shuffle: Option<bool>,
    /// `repeat` — `off`, `all` or `one`.
    #[serde(default)]
    pub repeat: Option<Repeat>,
    /// `view` — `library` or `queue`. Read for compatibility (S14): it
    /// still parses, but it is no longer written or applied.
    #[serde(default)]
    pub view: Option<View>,
    /// `browse_by` — `artists`, `albums`, `tracks` or `genres`.
    #[serde(default)]
    pub browse_by: Option<BrowseBy>,
    /// `artist` — the open artist, when the view is inside one.
    pub artist: Option<String>,
    /// `album` — the open album, when the view is inside one.
    pub album: Option<String>,
    /// `genre` — the open genre, in the genres view.
    pub genre: Option<String>,
}

impl StateKeys {
    /// The open node the three names describe: a name that is missing or
    /// blank is no node, so a table without names is no node either.
    fn open_node(&self) -> Option<OpenNode> {
        fn name(value: Option<&str>) -> Option<String> {
            value
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
        }

        let node = OpenNode {
            artist: name(self.artist.as_deref()),
            album: name(self.album.as_deref()),
            genre: name(self.genre.as_deref()),
        };
        if node.artist.is_none() && node.album.is_none() && node.genre.is_none() {
            None
        } else {
            Some(node)
        }
    }
}

/// A colour as the `[theme]` table writes it (S4): `"#rrggbb"`, or `"reset"`
/// for whatever the terminal draws with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeColor {
    /// The terminal's own colour: the user's palette, not velo's.
    Terminal,
    /// A 24-bit colour.
    Rgb { red: u8, green: u8, blue: u8 },
}

impl ThemeColor {
    /// The colour as the file writes it: `"reset"` or `"#rrggbb"`.
    #[must_use]
    pub fn to_key(self) -> String {
        match self {
            Self::Terminal => "reset".to_owned(),
            Self::Rgb { red, green, blue } => format!("#{red:02x}{green:02x}{blue:02x}"),
        }
    }
}

/// One colour of the default palette.
const fn rgb(red: u8, green: u8, blue: u8) -> ThemeColor {
    ThemeColor::Rgb { red, green, blue }
}

/// The `[theme]` table resolved with its defaults (S4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeColors {
    /// The screen behind everything.
    pub background: ThemeColor,
    /// The body text and the values in a list.
    pub text: ThemeColor,
    /// Hints, section headers and everything that should recede.
    pub dim: ThemeColor,
    /// Card borders, titles, the focused row and the selection.
    pub accent: ThemeColor,
    /// The keys and labels next to the values.
    pub secondary: ThemeColor,
    /// The panes that are not the one in focus.
    pub border: ThemeColor,
    /// The text drawn on top of `accent`.
    pub ink: ThemeColor,
}

impl Default for ThemeColors {
    /// The palette velo drew with before colours were configurable: the
    /// terminal's own background and text, the ANSI magenta accent and the
    /// ANSI blue secondary.
    fn default() -> Self {
        Self {
            background: ThemeColor::Terminal,
            text: ThemeColor::Terminal,
            dim: rgb(0x76, 0x76, 0x76),
            accent: rgb(0xaf, 0x00, 0xaf),
            secondary: rgb(0x5f, 0x87, 0xd7),
            border: rgb(0x76, 0x76, 0x76),
            ink: rgb(0x00, 0x00, 0x00),
        }
    }
}

/// The `[theme]` table as the file writes it (S4): every colour optional.
#[derive(Debug, Clone, Copy, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeKeys {
    /// `background` — `"#rrggbb"`, or `"reset"` for the terminal's own.
    #[serde(default, deserialize_with = "deserialize_terminal_colour")]
    pub background: Option<ThemeColor>,
    /// `text` — `"#rrggbb"`, or `"reset"` for the terminal's own.
    #[serde(default, deserialize_with = "deserialize_terminal_colour")]
    pub text: Option<ThemeColor>,
    /// `dim` — hints and everything that recedes.
    #[serde(default, deserialize_with = "deserialize_hex_colour")]
    pub dim: Option<ThemeColor>,
    /// `accent` — borders, titles and the selection.
    #[serde(default, deserialize_with = "deserialize_hex_colour")]
    pub accent: Option<ThemeColor>,
    /// `secondary` — keys and labels.
    #[serde(default, deserialize_with = "deserialize_hex_colour")]
    pub secondary: Option<ThemeColor>,
    /// `border` — the panes that are not in focus.
    #[serde(default, deserialize_with = "deserialize_hex_colour")]
    pub border: Option<ThemeColor>,
    /// `ink` — the text drawn on top of `accent`.
    #[serde(default, deserialize_with = "deserialize_hex_colour")]
    pub ink: Option<ThemeColor>,
}

impl ThemeKeys {
    /// The table with its defaults filled in.
    #[must_use]
    pub fn resolved(self) -> ThemeColors {
        let default = ThemeColors::default();
        ThemeColors {
            background: self.background.unwrap_or(default.background),
            text: self.text.unwrap_or(default.text),
            dim: self.dim.unwrap_or(default.dim),
            accent: self.accent.unwrap_or(default.accent),
            secondary: self.secondary.unwrap_or(default.secondary),
            border: self.border.unwrap_or(default.border),
            ink: self.ink.unwrap_or(default.ink),
        }
    }
}

/// Which parts of the interface are drawn (S5).
#[expect(
    clippy::struct_excessive_bools,
    reason = "six independent switches, one per key the file writes"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiParts {
    /// The cover art pane, when the terminal can show one.
    pub cover_art: bool,
    /// The spectrum bars in the now-playing block.
    pub spectrum: bool,
    /// The key hints on the last row.
    pub hints: bool,
    /// The tabs and the `V E L O` name on the first row.
    pub top_bar: bool,
    /// The status line that explains why something is not drawn (S5.4):
    /// today, a cover the window is too small for. Errors and ordinary
    /// messages are never silenced by it.
    pub advisories: bool,
    /// The numbers of the four library views in the header (S5.5):
    /// `1 artists  2 albums  3 tracks  4 genres`.
    pub view_numbers: bool,
}

impl Default for UiParts {
    /// Everything is drawn until the user says otherwise.
    fn default() -> Self {
        Self {
            cover_art: true,
            spectrum: true,
            hints: true,
            top_bar: true,
            advisories: true,
            view_numbers: true,
        }
    }
}

/// The `[ui]` table as the file writes it (S5): every key optional.
#[derive(Debug, Clone, Copy, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiKeys {
    /// `cover_art` — whether the cover art pane is drawn at all.
    pub cover_art: Option<bool>,
    /// `spectrum` — whether the spectrum bars are drawn.
    pub spectrum: Option<bool>,
    /// `hints` — whether the key hints are drawn.
    pub hints: Option<bool>,
    /// `top_bar` — whether the tabs and the player name are drawn.
    pub top_bar: Option<bool>,
    /// `advisories` — whether the status line explains why something is not
    /// drawn (S5.4); never silences errors or ordinary messages.
    pub advisories: Option<bool>,
    /// `view_numbers` — whether the library header numbers its views 1–4
    /// (S5.5).
    pub view_numbers: Option<bool>,
}

impl UiKeys {
    /// The table with its defaults filled in.
    #[must_use]
    pub fn resolved(self) -> UiParts {
        let default = UiParts::default();
        UiParts {
            cover_art: self.cover_art.unwrap_or(default.cover_art),
            spectrum: self.spectrum.unwrap_or(default.spectrum),
            hints: self.hints.unwrap_or(default.hints),
            top_bar: self.top_bar.unwrap_or(default.top_bar),
            advisories: self.advisories.unwrap_or(default.advisories),
            view_numbers: self.view_numbers.unwrap_or(default.view_numbers),
        }
    }
}

/// Resolves the default config file location from the two environment values
/// the binary owns. Tests pass them as arguments, so the resolution is
/// testable and no test ever reads or writes the user's real config.
#[must_use]
pub fn config_path(xdg_config_home: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    let from_xdg = xdg_config_home
        .filter(|value| !value.is_empty())
        .map(|value| Path::new(value).join("velo").join("config.toml"));
    from_xdg.or_else(|| {
        home.filter(|value| !value.is_empty()).map(|value| {
            Path::new(value)
                .join(".config")
                .join("velo")
                .join("config.toml")
        })
    })
}

/// The Radio Browser host the directory asks through: `host` in the
/// `[radio]` table of the config file at `path`, else [`DEFAULT_RADIO_HOST`]
/// (issue #14).
///
/// Never fails: the host is read when the radio view opens, long after the
/// launch already rejected broken files, so a missing path, an unreadable
/// file or a broken one yields the default — a file broken since then must
/// not empty the directory.
#[must_use]
pub fn radio_host_from(config_path: Option<&Path>) -> String {
    let Some(path) = config_path else {
        return DEFAULT_RADIO_HOST.to_owned();
    };
    let Ok(text) = fs::read_to_string(path) else {
        return DEFAULT_RADIO_HOST.to_owned();
    };
    toml::from_str::<Config>(&text)
        .ok()
        .and_then(|config| config.radio)
        .and_then(|radio| radio.host)
        .unwrap_or_else(|| DEFAULT_RADIO_HOST.to_owned())
}

/// The config file velo writes when there is none: every key at its default,
/// each under a short comment. `library` has no default that suits everyone,
/// so it is written commented out — "not in the file" — and the interface
/// asks for it.
#[must_use]
pub fn default_config_text() -> String {
    let theme = ThemeColors::default();
    format!(
        "# velo configuration. velo wrote this file with every key at its default;
# change any value to customize velo, or delete a line to restore its default.

# The folder velo indexes and browses: an absolute path, ~/... (your home) or
# a path relative to the directory velo starts in. --library overrides it.
# Unset, the interface says how to configure one (a folder here, or a
# [subsonic] server); press L in the interface to enter it here.
# library = \"~/Music\"

# Bit-perfect playback: the samples the decoder produces reach the device
# unmodified — no resampling, no ReplayGain, and the volume is pinned to
# 100%. A track at a different rate reopens the device at its own rate (a
# small gap at that boundary); channel layouts still map (mono plays on every
# channel). When the device has no output at a track's rate, velo falls back
# to converting and says so once.
bit_perfect = false

# ReplayGain: \"track\" (the track's gain), \"album\" (the album's, else the
# track's) or \"off\".
replay_gain = \"track\"

# What comes back the next time velo starts. velo keeps this table up to date
# as you listen; edit it to choose where the next launch begins. `artist`,
# `album` and `genre` name the node that was open — `album` always belongs to
# `artist` — and a line velo leaves out is a level velo does not restore.
[state]
volume = {DEFAULT_VOLUME}
shuffle = false
repeat = \"off\"
browse_by = \"artists\"

# Every colour, as \"#rrggbb\". \"reset\" is the terminal's own colour and is
# the default for background and text only.
[theme]
background = \"{}\"
text = \"{}\"
dim = \"{}\"
accent = \"{}\"
secondary = \"{}\"
border = \"{}\"
ink = \"{}\"

# The parts of the interface to draw.
[ui]
cover_art = true
spectrum = true
hints = true
top_bar = true

# The status line that explains why something is not drawn (a cover the
# window is too small for). Errors and messages are never silenced.
advisories = true
# The numbers 1–4 before the library view names in the header.
view_numbers = true

# The Radio Browser directory host: a bare name, no scheme, no path. Unset,
# velo asks the community's round-robin across the directory's mirrors.
# [radio]
# host = \"all.api.radio-browser.info\"

# A Subsonic-compatible server (Navidrome and friends), optional. Uncomment
# the table to browse a remote server; `velo --subsonic-test` checks it and
# `velo --sync` copies the server's library into the index, so browsing and
# search reach it (playback from the server comes later).
# url is required when the table exists, as is user. api_key is where a
# server-issued API key goes; without it velo asks the environment
# ($VELO_SUBSONIC_SECRET) and then ~/.config/velo/subsonic.key (one line,
# readable only by you). insecure = true accepts a self-signed certificate.
# [subsonic]
# url = \"https://music.example.com\"
# user = \"you\"
# api_key = \"the-key-from-the-server\"
# insecure = false
",
        theme.background.to_key(),
        theme.text.to_key(),
        theme.dim.to_key(),
        theme.accent.to_key(),
        theme.secondary.to_key(),
        theme.border.to_key(),
        theme.ink.to_key(),
    )
}

/// Writes [`default_config_text`] to `path`, creating its directory, unless a
/// file is already there: an existing file is never overwritten or
/// reformatted. Answers whether it wrote one.
///
/// # Errors
///
/// Fails when the directory or the file cannot be created.
pub fn write_default_config(path: &Path) -> std::io::Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path);
    match file {
        Ok(mut file) => {
            std::io::Write::write_all(&mut file, default_config_text().as_bytes())?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error),
    }
}

/// Writes `library = "<value>"` into the config file at `path`, keeping every
/// other line and comment as it is: an active `library` line is replaced,
/// else a commented-out one is uncommented, else the key is added on top
/// (the file has no tables, so the top is still the root table). A missing
/// file starts from [`default_config_text`].
///
/// # Errors
///
/// Fails when the file cannot be read or written.
pub fn set_library(path: &Path, value: &str) -> std::io::Result<()> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => default_config_text(),
        Err(error) => return Err(error),
    };
    let entry = format!("library = {}", toml::Value::String(value.to_owned()));
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let target = lines
        .iter()
        .position(|line| is_key_line(line, "library", false))
        .or_else(|| {
            lines
                .iter()
                .position(|line| is_key_line(line, "library", true))
        });
    match target {
        Some(index) => lines[index] = entry,
        None => lines.insert(0, entry),
    }
    let mut out = lines.join("\n");
    if text.ends_with('\n') || text.is_empty() {
        out.push('\n');
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, out)
}

/// Whether `line` assigns `key`, active or (with `commented`) behind `#`.
fn is_key_line(line: &str, key: &str, commented: bool) -> bool {
    let line = line.trim_start();
    let line = if commented {
        match line.strip_prefix('#') {
            Some(rest) => rest.trim_start(),
            None => return false,
        }
    } else {
        line
    };
    line.strip_prefix(key)
        .is_some_and(|rest| rest.trim_start().starts_with('='))
}

/// Whether `line` opens a table: any unindented `[` — a commented
/// `# [theme]` is just a comment, not a header.
fn is_table_header(line: &str) -> bool {
    line.trim_start().starts_with('[')
}

/// The half-open range of lines a table's keys live in: for `Some(name)`, the
/// lines under its `[name]` header and above the next header; for `None`, the
/// lines above the first header — the top-level table. `None` when the named
/// table is not in the file; the top-level table always has a span.
fn table_span(lines: &[String], section: Option<&str>) -> Option<(usize, usize)> {
    if let Some(name) = section {
        let header = format!("[{name}]");
        let header_index = lines.iter().position(|line| line.trim() == header)?;
        let end = lines[header_index + 1..]
            .iter()
            .position(|line| is_table_header(line))
            .map_or(lines.len(), |offset| header_index + 1 + offset);
        Some((header_index + 1, end))
    } else {
        let end = lines
            .iter()
            .position(|line| is_table_header(line))
            .unwrap_or(lines.len());
        Some((0, end))
    }
}

/// Writes `key = value` into the config file at `path`, one key at a time,
/// keeping every other line — comments, blank lines, keys and their order,
/// table headers — byte for byte. `value` is already the TOML literal to
/// write after the `=` (`72`, `"all"`, `false`), so the caller decides the
/// quoting; the function writes exactly `key = value`.
///
/// `section` names the table without its brackets: `Some("state")` writes
/// into `[state]`, `None` into the top-level table, the lines above the
/// first header. Inside that table an active `key = …` line is replaced in
/// place, else a commented `# key = …` line there is uncommented, else the
/// line is appended at the end of the table. A table that does not exist is
/// opened at the end of the file with the key under it. A missing file
/// starts from [`default_config_text`]; otherwise the file is rewritten only
/// when the key actually lands somewhere.
///
/// One key, one table: `set_value` never touches the same key in another
/// table. In particular, the legacy top-level `volume` (S2.5) stays behind
/// when the first `[state] volume` is written — the parser reads `[state]`
/// and ignores the stale line, so the file never holds two `volume` keys in
/// one table. A caller that wants the stale line gone pairs the write with
/// `remove_value(path, None, "volume")`.
///
/// # Errors
///
/// Fails when the file cannot be read or written.
pub fn set_value(
    path: &Path,
    section: Option<&str>,
    key: &str,
    value: &str,
) -> std::io::Result<()> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => default_config_text(),
        Err(error) => return Err(error),
    };
    let entry = format!("{key} = {value}");
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();

    let (start, end) = if let Some(span) = table_span(&lines, section) {
        span
    } else if let Some(name) = section {
        // The table does not exist: open it at the end of the file, with the
        // key to go right under the header written here.
        let header = format!("[{name}]");
        lines.push(header);
        (lines.len(), lines.len())
    } else {
        // `table_span` always finds the top-level table; refusing the write
        // beats panicking in the middle of saving if that ever stops holding.
        return Err(std::io::Error::other("the top-level table has no span"));
    };

    let found = (start..end)
        .find(|index| is_key_line(&lines[*index], key, false))
        .or_else(|| (start..end).find(|index| is_key_line(&lines[*index], key, true)));
    let appended_at_end = if let Some(index) = found {
        lines[index] = entry;
        false
    } else {
        // Append right after the table's last content line, before any blank
        // lines that separate it from the next table.
        let mut at = end;
        while at > start && lines[at - 1].trim().is_empty() {
            at -= 1;
        }
        lines.insert(at, entry);
        at + 1 == lines.len()
    };

    let mut out = lines.join("\n");
    if text.ends_with('\n') || text.is_empty() || appended_at_end {
        out.push('\n');
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, out)
}

/// Removes `key` from `section`, using the same section convention as
/// [`set_value`]: the key's active line and its commented twin are dropped,
/// everything else — a table left empty included — is left as it is. A
/// missing file, a missing table or a missing key is a silent no-op that
/// writes nothing.
///
/// # Errors
///
/// Fails when the file cannot be read or written.
pub fn remove_value(path: &Path, section: Option<&str>, key: &str) -> std::io::Result<()> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let lines: Vec<String> = text.lines().map(str::to_owned).collect();

    let Some((start, end)) = table_span(&lines, section) else {
        // The named table is not in the file, so the key cannot be either.
        return Ok(());
    };

    let mut kept: Vec<String> = Vec::with_capacity(lines.len());
    for (index, line) in lines.iter().enumerate() {
        if index >= start
            && index < end
            && (is_key_line(line, key, false) || is_key_line(line, key, true))
        {
            continue;
        }
        kept.push(line.clone());
    }
    if kept.len() == lines.len() {
        return Ok(());
    }
    let mut out = kept.join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }
    fs::write(path, out)
}

/// Resolves a library path as the user wrote it: `~` and `~/...` against
/// `home`, a relative path against `cwd`, an absolute path as it is. `~user`
/// is not special. Without a home, `~` stays literal (and is then relative).
#[must_use]
pub fn expand_path(raw: &Path, home: Option<&Path>, cwd: &Path) -> PathBuf {
    if let (Ok(rest), Some(home)) = (raw.strip_prefix("~"), home) {
        return if rest.as_os_str().is_empty() {
            home.to_path_buf()
        } else {
            home.join(rest)
        };
    }
    // An empty path stays empty, so it is refused as a root that does not
    // exist instead of quietly becoming the working directory.
    if raw.is_absolute() || raw.as_os_str().is_empty() {
        raw.to_path_buf()
    } else {
        cwd.join(raw)
    }
}

/// The `library` value the prompt writes for a path typed as `typed` that
/// expanded to `expanded`: an absolute or `~` path as typed, so the file
/// reads the way the user wrote it, and a path relative to where velo ran as
/// its expansion — saved verbatim it would only load from that directory.
#[must_use]
pub fn library_to_save(typed: &str, expanded: &Path) -> String {
    let path = Path::new(typed);
    if path.is_absolute() || path.starts_with("~") {
        typed.to_owned()
    } else {
        expanded.display().to_string()
    }
}

/// Everything the config file can fail with, already phrased for the user.
#[derive(Debug)]
pub struct ConfigError {
    message: String,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ConfigError {}

/// One error line naming the file and the line: `path: line N: what`.
fn parse_error_message(path: &Path, error: &toml::de::Error, text: &str) -> String {
    let detail = error.message().to_string();
    // The span is a byte offset into the document; the error line is the
    // number of newlines before it, plus one.
    let line = error
        .span()
        .filter(|span| span.start <= text.len())
        .map(|span| text[..span.start].matches('\n').count() + 1);

    match line {
        Some(line) => format!("{}: line {line}: {detail}", path.display()),
        None => format!("{}: {detail}", path.display()),
    }
}

/// Reads a `[theme]` colour that may be `"reset"`, for `background` and
/// `text`: there it means the terminal's own colour, which is what velo has
/// always drawn with.
fn deserialize_terminal_colour<'de, D>(deserializer: D) -> Result<Option<ThemeColor>, D::Error>
where
    D: Deserializer<'de>,
{
    ThemeColor::deserialize(deserializer).map(Some)
}

/// Reads a `[theme]` colour that has to be `"#rrggbb"`: `"reset"` there would
/// erase an accent, a border or the dim text instead of colouring it.
fn deserialize_hex_colour<'de, D>(deserializer: D) -> Result<Option<ThemeColor>, D::Error>
where
    D: Deserializer<'de>,
{
    let colour = ThemeColor::deserialize(deserializer)?;
    if matches!(colour, ThemeColor::Terminal) {
        return Err(de::Error::custom(
            "\"reset\" is a colour for background and text only: write \"#rrggbb\"",
        ));
    }
    Ok(Some(colour))
}

impl<'de> serde::Deserialize<'de> for ThemeColor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ColourVisitor;

        impl Visitor<'_> for ColourVisitor {
            type Value = ThemeColor;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("\"#rrggbb\" or \"reset\"")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<ThemeColor, E> {
                if value == "reset" {
                    return Ok(ThemeColor::Terminal);
                }
                parse_colour(value).ok_or_else(|| {
                    E::custom(format!(
                        "{value} is not a colour: write \"#rrggbb\" or \"reset\""
                    ))
                })
            }
        }

        deserializer.deserialize_str(ColourVisitor)
    }
}

/// Parses `"#rrggbb"`: a hash and exactly six hexadecimal digits.
fn parse_colour(text: &str) -> Option<ThemeColor> {
    let digits = text.strip_prefix('#')?;
    if digits.len() != 6 || !digits.is_ascii() {
        return None;
    }
    let byte = |at: usize| u8::from_str_radix(digits.get(at..at + 2)?, 16).ok();
    Some(ThemeColor::Rgb {
        red: byte(0)?,
        green: byte(2)?,
        blue: byte(4)?,
    })
}

fn deserialize_volume<'de, D>(deserializer: D) -> Result<Option<u8>, D::Error>
where
    D: Deserializer<'de>,
{
    struct VolumeVisitor;

    impl Visitor<'_> for VolumeVisitor {
        type Value = u8;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("an integer between 0 and 100")
        }

        fn visit_i64<E: de::Error>(self, value: i64) -> Result<u8, E> {
            u8::try_from(value)
                .ok()
                .filter(|volume| *volume <= 100)
                .ok_or_else(|| de::Error::custom("volume must be between 0 and 100"))
        }

        fn visit_u64<E: de::Error>(self, value: u64) -> Result<u8, E> {
            self.visit_i64(
                value
                    .try_into()
                    .map_err(|_| de::Error::custom("volume must be between 0 and 100"))?,
            )
        }
    }

    deserializer.deserialize_i64(VolumeVisitor).map(Some)
}

/// The serde shape of the `[radio]` table: the same keys and nothing else.
/// `host` is a bare host name — no scheme, no path — and an absent table,
/// or a table without `host`, means [`DEFAULT_RADIO_HOST`] (issue #14).
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RadioFile {
    #[serde(default)]
    host: Option<String>,
}

/// The serde shape of the `[subsonic]` table: the same keys and nothing
/// else. `url` and `user` are required when the table exists.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubsonicFile {
    #[serde(deserialize_with = "deserialize_server_url")]
    url: String,
    user: String,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    insecure: Option<bool>,
}

/// Accepts only an `http://` or `https://` server address: the scheme must
/// be there and something must follow it. The toml error keeps the value's
/// span, so the message names the file and the line.
fn deserialize_server_url<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let url: String = serde::Deserialize::deserialize(deserializer)?;
    let trimmed = url.trim();
    let rest = trimmed
        .strip_prefix("http://")
        .or_else(|| trimmed.strip_prefix("https://"))
        .filter(|rest| !rest.is_empty());
    match rest {
        Some(_) => Ok(url),
        None => Err(de::Error::custom(
            "subsonic.url must be an http:// or https:// server address",
        )),
    }
}
