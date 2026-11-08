//! The station preference file: favourites, pinned places, hand-added
//! stations and the opt-in country, in `radio.toml`.
//!
//! Preferences live in their own file and never in `config.toml`: that one is
//! edited line by line by `set_value`/`remove_value` and cannot carry arrays
//! of tables, and favourites and stations are whole sections, not single
//! lines. [`RadioStore::load`] treats a missing file as an empty store — the
//! first launch must not look like data loss — but an unreadable or corrupt
//! file is an error, so the caller can refuse to write over preferences it
//! never saw. [`RadioStore::save`] writes a temporary file in the same
//! directory, flushes it and renames it over the target, so the file on disk
//! is always either the old store or the new one, never a half-written one.
//!
//! A station's identity is its [`station_key`] (spec S6): only scheme case,
//! host case, a default `:80`/`:443` port and a single trailing slash are
//! normalised; path, query and fragment stay as they are. Every toggle and
//! removal deduplicates on that key, so a station reached through two
//! differently written URLs is still one favourite and one entry.
//!
//! Unknown keys are tolerated when loading: a newer velo may write fields
//! this build does not know, and a preferences file that cannot be read is a
//! preferences file that gets wiped.

use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io;
use std::io::Write as _;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// A station's identity (spec S6): the URL the directory listed, normalised
/// only in scheme case, host case, a default `:80`/`:443` port and a single
/// trailing slash. Path, query and fragment are kept as they are; a URL
/// without a scheme is kept entirely as it is.
#[must_use]
pub fn station_key(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_owned();
    };
    let (authority, tail) = match rest.find(['/', '?', '#']) {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, ""),
    };
    // A default port is noise; any other port (or a userinfo `@`, out of
    // scope for directory URLs) is part of the address and stays.
    let authority = match authority.rsplit_once(':') {
        Some((host, "80")) if scheme.eq_ignore_ascii_case("http") => host,
        Some((host, "443")) if scheme.eq_ignore_ascii_case("https") => host,
        _ => authority,
    };
    // One trailing slash on the path is noise; a slash ending a query or
    // fragment belongs to that query or fragment and stays.
    let tail = if tail.ends_with('/') && !tail.contains(['?', '#']) {
        &tail[..tail.len() - 1]
    } else {
        tail
    };
    format!(
        "{}://{}{}",
        scheme.to_ascii_lowercase(),
        authority.to_ascii_lowercase(),
        tail
    )
}

/// What a toggle or add did to the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggled {
    /// The marker was added; the file needs saving.
    On,
    /// The marker was removed; the file needs saving.
    Off,
    /// The station was already stored under a differently written URL, so
    /// nothing changed and there is nothing to save. Only adding produces
    /// this: a toggle on a key match unmarks the station.
    Already,
}

impl Toggled {
    /// Whether the store changed and the caller must save.
    #[must_use]
    pub fn needs_save(self) -> bool {
        self != Self::Already
    }
}

/// Why the store could not be loaded or written.
#[derive(Debug)]
pub enum StoreError {
    /// The file exists but could not be read (permissions, not UTF-8).
    Read(io::Error),
    /// The file exists but is not a store this build understands.
    Parse(toml::de::Error),
    /// The file could not be written or replaced.
    Write(io::Error),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(error) => write!(f, "could not read the radio preferences: {error}"),
            Self::Parse(error) => write!(f, "the radio preferences are corrupt: {error}"),
            Self::Write(error) => write!(f, "could not write the radio preferences: {error}"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read(error) | Self::Write(error) => Some(error),
            Self::Parse(error) => Some(error),
        }
    }
}

/// A favourited station, one `[[favorite]]` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Favorite {
    /// Display name as the directory listed it.
    pub name: String,
    /// The stream URL; the station's identity is [`station_key`] of this.
    pub url: String,
    /// ISO country code the directory listed, when it did.
    #[serde(default)]
    pub countrycode: String,
    /// Codec the directory listed, when it did.
    #[serde(default)]
    pub codec: String,
}

/// A pinned country or region, one `[[pinned]]` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pinned {
    /// ISO 3166-1 country code.
    pub code: String,
    /// Display name as the directory listed it.
    pub name: String,
    /// Region within the country; empty for the country itself.
    #[serde(default)]
    pub state: String,
}

/// A hand-added station, one `[[station]]` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Station {
    /// Display name the user gave it.
    pub name: String,
    /// The stream URL; the station's identity is [`station_key`] of this.
    pub url: String,
}

/// Everything `radio.toml` holds: favourites, pinned places, hand-added
/// stations and the opt-in country. Field names are the file's key names,
/// and unknown keys are ignored when loading.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RadioStore {
    /// The opt-in country for directory searches: an ISO 3166-1 code like
    /// `NO`, or the literal `none` after the user declined. `None` means the
    /// question was never asked, and it is never written to the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    /// Favourited stations, as `[[favorite]]` tables.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub favorite: Vec<Favorite>,
    /// Pinned countries and regions, as `[[pinned]]` tables.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pinned: Vec<Pinned>,
    /// Hand-added stations, as `[[station]]` tables.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub station: Vec<Station>,
}

impl RadioStore {
    /// Loads the store from `path`. A missing file is an empty store; an
    /// unreadable or corrupt file is an error, so the caller can refuse to
    /// write over preferences it never saw.
    ///
    /// # Errors
    /// [`StoreError::Read`] when the file exists but cannot be read;
    /// [`StoreError::Parse`] when it exists but is not a store this build
    /// understands. Unknown keys are tolerated; the fields this build knows
    /// must still parse.
    pub fn load(path: &Path) -> Result<Self, StoreError> {
        match fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(StoreError::Parse),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(StoreError::Read(error)),
        }
    }

    /// Writes the store to `path` atomically: a temporary file in the same
    /// directory, flushed to disk, then renamed over the target, so the file
    /// on disk is never a half-written store. The parent directory is
    /// created when missing, and the temporary file is removed when anything
    /// fails.
    ///
    /// # Errors
    /// [`StoreError::Write`] when the directory cannot be created, the
    /// temporary file cannot be written or flushed, or the rename fails.
    pub fn save(&self, path: &Path) -> Result<(), StoreError> {
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(dir).map_err(StoreError::Write)?;
        let body = toml::to_string_pretty(self)
            .map_err(|error| StoreError::Write(io::Error::other(error)))?;
        let name = path
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or("radio.toml");
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let temp = dir.join(format!(".{name}.tmp.{}-{nanos}", std::process::id()));

        let written = (|| -> io::Result<()> {
            let mut file = fs::File::create_new(&temp)?;
            file.write_all(body.as_bytes())?;
            file.flush()?;
            file.sync_all()
        })();
        match written {
            Ok(()) => (),
            Err(error) => {
                let _ = fs::remove_file(&temp);
                return Err(StoreError::Write(error));
            }
        }
        match fs::rename(&temp, path) {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = fs::remove_file(&temp);
                Err(StoreError::Write(error))
            }
        }
    }

    /// Whether `url` is favourited, under any writing of its station key.
    #[must_use]
    pub fn is_favorite(&self, url: &str) -> bool {
        let key = station_key(url);
        self.favorite
            .iter()
            .any(|favorite| station_key(&favorite.url) == key)
    }

    /// Adds `favorite` as a favourite, or removes it again when the very
    /// same record is already stored. A station already favourited under a
    /// differently written URL (case, default port, trailing slash) is left
    /// as it is: one favourite per station, never two.
    pub fn toggle_favorite(&mut self, favorite: Favorite) -> Toggled {
        let key = station_key(&favorite.url);
        let stored = self
            .favorite
            .iter()
            .position(|stored| station_key(&stored.url) == key);
        // Same station however the URL is written: the marker was on, so
        // pressing it takes the station off.
        if let Some(index) = stored {
            self.favorite.remove(index);
            Toggled::Off
        } else {
            self.favorite.push(favorite);
            Toggled::On
        }
    }

    /// Whether the country `code` with region `state` is pinned.
    #[must_use]
    pub fn is_pinned(&self, code: &str, state: &str) -> bool {
        self.pinned
            .iter()
            .any(|pinned| pinned.code.eq_ignore_ascii_case(code) && pinned.state == state)
    }

    /// Pins `pinned`, or unpins it again when the very same record is
    /// already stored; a country already pinned under a different case is
    /// left as it is.
    pub fn toggle_pin(&mut self, pinned: Pinned) -> Toggled {
        let stored = self.pinned.iter().position(|stored| {
            stored.code.eq_ignore_ascii_case(&pinned.code) && stored.state == pinned.state
        });
        if let Some(index) = stored {
            self.pinned.remove(index);
            Toggled::Off
        } else {
            self.pinned.push(pinned);
            Toggled::On
        }
    }

    /// Adds a hand-added station unless its station key is already present
    /// under another writing, in which case nothing changes.
    pub fn add_station(&mut self, station: Station) -> Toggled {
        let key = station_key(&station.url);
        if self
            .station
            .iter()
            .any(|stored| station_key(&stored.url) == key)
        {
            return Toggled::Already;
        }
        self.station.push(station);
        Toggled::On
    }

    /// Removes the station whose key matches `url` and returns whether the
    /// file needs saving; a station that is not there is a no-op.
    pub fn remove_station(&mut self, url: &str) -> bool {
        let key = station_key(url);
        match self
            .station
            .iter()
            .position(|stored| station_key(&stored.url) == key)
        {
            Some(index) => {
                self.station.remove(index);
                true
            }
            None => false,
        }
    }
}
