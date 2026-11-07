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
//! The merged [`Settings`] carries keys today's code does not read yet —
//! notably `bit_perfect`, stored and passed through for T17 (bit-perfect
//! playback opt-in) — so the config schema does not have to change when the
//! feature that consumes them arrives. Nothing reads them today; that is why
//! they exist.

use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use clap::Parser;
use serde::de::{self, Deserializer, Visitor};

/// The library root indexed and browsed until the config file or `--library`
/// says otherwise.
pub const DEFAULT_LIBRARY_ROOT: &str = "/Users/alvaldes/Music/Library";

/// The volume the player has always started at (T3 kept it at full).
pub const DEFAULT_VOLUME: u8 = 100;

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

Interface keys: j/k move, enter open/play, tab switches to the queue view
    (a append, d remove, J/K move, enter play from there), / search, ←/→ or
    ,/. seek ±5 s, space pause, +/- volume, ? help, q quit. Cover art is
    drawn when the terminal supports the kitty, sixel or iTerm2 protocols.

Audio must share one channel layout: sample rates are converted to the
    output device, channel layouts are not.

Configuration: $XDG_CONFIG_HOME/velo/config.toml, or
    ~/.config/velo/config.toml — keys: library, volume, bit_perfect. A
    missing file is normal and silent; an unknown key is an error."
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

    /// Build or refresh the library index, print its counts, then exit.
    #[arg(long)]
    pub index: bool,

    /// Library root to index or browse; overrides `library` in the config file.
    #[arg(long, value_name = "PATH")]
    pub library: Option<PathBuf>,
}

/// The config file as the user wrote it: every key optional, nothing filled in.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// `library` — the library root to index and browse.
    pub library: Option<PathBuf>,
    /// `volume` — an integer between 0 and 100, inclusive.
    pub volume: Option<u8>,
    /// `bit_perfect` — stored and passed through for T17; nothing reads it
    /// yet, and saying so here is the point.
    pub bit_perfect: Option<bool>,
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

        let file: FileConfig = toml::from_str(&text).map_err(|error| ConfigError {
            message: parse_error_message(path, &error, &text),
        })?;

        Ok(Self {
            library: file.library,
            volume: file.volume,
            bit_perfect: file.bit_perfect,
        })
    }

    /// Merges the config file with the command line: the command line wins
    /// over the config file, the config file wins over the built-in default.
    #[must_use]
    pub fn merged(self, cli: &Cli) -> Settings {
        Settings {
            library: cli
                .library
                .clone()
                .or(self.library)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_LIBRARY_ROOT)),
            volume: self.volume.unwrap_or(DEFAULT_VOLUME),
            bit_perfect: self.bit_perfect.unwrap_or(false),
        }
    }
}

/// What the binary actually runs with, after the command line, the config
/// file and the defaults have been resolved.
///
/// `bit_perfect` is set here and read nowhere; T17 will read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub library: PathBuf,
    pub volume: u8,
    pub bit_perfect: bool,
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

/// The serde shape of the file: the same keys and nothing else.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    #[serde(default, deserialize_with = "deserialize_volume")]
    volume: Option<u8>,
    library: Option<PathBuf>,
    bit_perfect: Option<bool>,
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
