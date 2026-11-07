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
//! otherwise (T11, S15) through [`ReplayGainMode`].

use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use clap::Parser;
use serde::de::{self, Deserializer, Visitor};

/// The volume the player has always started at (T3 kept it at full).
pub const DEFAULT_VOLUME: u8 = 100;

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

Interface keys: j/k move, enter open/play, tab switches to the queue view
    (a append, d remove, J/K move, enter play from there), / search, ←/→ or
    ,/. seek ±5 s, space pause, +/- volume, ? help, q quit. Cover art is
    drawn when the terminal supports the kitty, sixel or iTerm2 protocols.

Audio must share one channel layout: sample rates are converted to the
    output device, channel layouts are not. With bit_perfect in the config,
    rates are not converted either — the device is reopened at each track's
    own rate, the volume is pinned to 100% and ReplayGain is skipped.

Configuration: $XDG_CONFIG_HOME/velo/config.toml, or
    ~/.config/velo/config.toml — keys: library, volume, bit_perfect, replay_gain,
    and the optional [subsonic] table (url, user, api_key, insecure) for a
    Subsonic-compatible server; `velo --subsonic-test` answers \"can I use this
    server?\" and works over ssh. When the file is missing, velo writes it with
    every key at its default and a comment per key; an unknown key is an error. library (and --library)
    takes an absolute path, ~/... or a path relative to the current directory.
    With no library set, the interface asks for it in a floating prompt that
    starts at ~/ (tab completes folders) and saves the answer to the file; velo --index needs one and exits 2 without."
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

    /// Build or refresh the library index, print its counts, then exit.
    #[arg(long)]
    pub index: bool,

    /// Probe the configured Subsonic server: report what it is, what it
    /// advertises and which authentication worked, then exit.
    #[arg(long)]
    pub subsonic_test: bool,

    /// Sync the configured Subsonic server into the library index: fetch
    /// every album, write the rows, print the counts per source, then exit.
    /// Never runs automatically.
    #[arg(long)]
    pub sync: bool,

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
    /// `bit_perfect` — the decoder's samples reach the device unmodified:
    /// no resampling, no `ReplayGain`, and the volume pinned to 100% (T17).
    pub bit_perfect: Option<bool>,
    /// `replay_gain` — which `ReplayGain` tags the audio path applies (T11,
    /// S15); the audio path reads it through [`ReplayGainMode`].
    pub replay_gain: Option<ReplayGainMode>,
    /// `[subsonic]` — the Subsonic-compatible server (T12a), when the
    /// table exists. `url` and `user` are required inside it.
    pub subsonic: Option<SubsonicConfig>,
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
            replay_gain: file.replay_gain,
            subsonic: file.subsonic.map(|subsonic| SubsonicConfig {
                url: subsonic.url,
                user: subsonic.user,
                api_key: subsonic.api_key,
                insecure: subsonic.insecure.unwrap_or(false),
            }),
        })
    }

    /// Merges the config file with the command line: the command line wins
    /// over the config file, the config file wins over the built-in default.
    #[must_use]
    pub fn merged(self, cli: &Cli) -> Settings {
        Settings {
            // An empty `library` in the file is no library: the prompt asks.
            library: cli.library.clone().or(self
                .library
                .filter(|library| !library.as_os_str().is_empty())),
            volume: self.volume.unwrap_or(DEFAULT_VOLUME),
            bit_perfect: self.bit_perfect.unwrap_or(false),
            replay_gain: self.replay_gain.unwrap_or(ReplayGainMode::Track),
            subsonic: self.subsonic,
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
/// the producer, once per track, before the ring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// The library root as written (`--library`, else the file), unexpanded;
    /// `None` when neither names one. [`expand_path`] resolves it.
    pub library: Option<PathBuf>,
    pub volume: u8,
    pub bit_perfect: bool,
    pub replay_gain: ReplayGainMode,
    /// The Subsonic server (T12a), when the config has the table.
    pub subsonic: Option<SubsonicConfig>,
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

/// The config file velo writes when there is none: every key at its default,
/// each under a short comment. `library` has no default that suits everyone,
/// so it is written commented out — "not in the file" — and the interface
/// asks for it.
#[must_use]
pub fn default_config_text() -> String {
    format!(
        "# velo configuration. velo wrote this file with every key at its default;
# change any value to customize velo, or delete a line to restore its default.

# The folder velo indexes and browses: an absolute path, ~/... (your home) or
# a path relative to the directory velo starts in. --library overrides it.
# Unset, the interface asks for it the first time and saves the answer here.
# library = \"~/Music\"

# The volume velo starts at, an integer from 0 to 100.
volume = {DEFAULT_VOLUME}

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
"
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
        .position(|line| is_library_line(line, false))
        .or_else(|| lines.iter().position(|line| is_library_line(line, true)));
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

/// Whether `line` assigns `library`, active or (with `commented`) behind `#`.
fn is_library_line(line: &str, commented: bool) -> bool {
    let line = line.trim_start();
    let line = if commented {
        match line.strip_prefix('#') {
            Some(rest) => rest.trim_start(),
            None => return false,
        }
    } else {
        line
    };
    line.strip_prefix("library")
        .is_some_and(|rest| rest.trim_start().starts_with('='))
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

/// The serde shape of the file: the same keys and nothing else.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    #[serde(default, deserialize_with = "deserialize_volume")]
    volume: Option<u8>,
    library: Option<PathBuf>,
    bit_perfect: Option<bool>,
    #[serde(default)]
    replay_gain: Option<ReplayGainMode>,
    #[serde(default)]
    subsonic: Option<SubsonicFile>,
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

/// The serde shape of the `[subsonic]` table: the same keys and nothing
/// else. `url` and `user` are required when the table exists.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SubsonicFile {
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
