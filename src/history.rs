//! What was played recently (issue #5): a small TOML file beside the config,
//! newest first, one entry per path, capped at [`CAP`] entries.
//!
//! The file is a courtesy, never a dependency. `velo` plays whatever it says
//! or does not say, a file that does not parse is reported by `velo history`
//! and ignored by the interface, and a write that fails never interrupts
//! playback: the history is the first thing allowed to lose.
//!
//! ```toml
//! [[entry]]
//! path = "/music/Tide/01 - Nightfall.flac"
//! played_at = 1762668000
//! artist = "Tide"
//! album = "Harbor"
//! title = "Nightfall"
//! ```
//!
//! The artist, album and title are optional on purpose: a path the library
//! index cannot describe — a file opened from the command line — is still
//! worth remembering, and [`Entry::label`] falls back to the file's name.

use std::fmt;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// How many entries the file keeps. A few hundred is enough to answer "what
/// was I listening to?" without growing without bound, and cheap enough to
/// rewrite on every track change.
pub const CAP: usize = 500;

/// What a history entry is: a track the queue carried, or a live radio
/// station (issue #33).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A track: [`Entry::path`] is the file path the queue carried.
    #[default]
    File,
    /// A live station: [`Entry::path`] is the stream URL.
    Station,
}

/// The file's name beside `config.toml`.
const FILE_NAME: &str = "history.toml";

/// Where the history lives for a given config file: the same directory, so a
/// `XDG_CONFIG_HOME` the user already chose decides both.
#[must_use]
pub fn path_beside(config_file: &Path) -> PathBuf {
    config_file.with_file_name(FILE_NAME)
}

/// The current time as the file records it.
#[must_use]
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// One beginning: the path the queue carried, when it began, and what the
/// index could say about it.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Entry {
    /// The path verbatim, and the entry's identity: for a track, the file
    /// path the queue held — a `subsonic://…` row stays what the queue held,
    /// never the cache file it resolves to; for a station, the stream URL.
    /// Never empty.
    pub path: String,
    /// What kind of beginning this is. An entry written before the key
    /// existed loads as a file, the way it always was.
    #[serde(default)]
    pub kind: Kind,
    /// Unix seconds.
    pub played_at: u64,
    /// The artist, when the index knew it.
    pub artist: Option<String>,
    /// The album, when the index knew it.
    pub album: Option<String>,
    /// The title, when the index knew it.
    pub title: Option<String>,
}

impl Entry {
    /// A station's beginning (issue #33): the stream URL stands in the
    /// identity field, and the station's own name is the title. A station
    /// has no artist and no album — nothing to look up.
    #[must_use]
    pub fn station(url: &str, name: &str, played_at: u64) -> Self {
        Self {
            path: url.to_owned(),
            kind: Kind::Station,
            played_at,
            artist: None,
            album: None,
            title: Some(name.to_owned()),
        }
    }

    /// Whether this beginning is a live station, not a file.
    #[must_use]
    pub fn is_station(&self) -> bool {
        self.kind == Kind::Station
    }

    /// The identity a beginning dedupes on: the radio store's station key
    /// of the stream URL for a station — two writings of one station are
    /// one entry — and the path verbatim for a file, so a station never
    /// collapses into a file or the other way round.
    #[must_use]
    pub fn identity(&self) -> String {
        if self.kind == Kind::Station {
            format!("station:{}", crate::radio::store::station_key(&self.path))
        } else {
            format!("file:{}", self.path)
        }
    }

    /// How one entry is named, on screen and in `velo history`: `artist —
    /// title` when both are known, the title alone when that is all there is,
    /// and the file's own name when the index could not describe the path.
    #[must_use]
    pub fn label(&self) -> String {
        match (&self.artist, &self.title) {
            (Some(artist), Some(title)) if !artist.is_empty() && !title.is_empty() => {
                format!("{artist} \u{2014} {title}")
            }
            (_, Some(title)) if !title.is_empty() => title.clone(),
            _ => Path::new(&self.path).file_stem().map_or_else(
                || self.path.clone(),
                |stem| stem.to_string_lossy().into_owned(),
            ),
        }
    }

    /// The entry as one JSON object, the optional fields left out when the
    /// index did not know them.
    fn to_json(&self) -> String {
        let mut fields = vec![
            format!("\"path\":\"{}\"", escape(&self.path)),
            format!(
                "\"kind\":\"{}\"",
                if self.kind == Kind::Station {
                    "station"
                } else {
                    "file"
                }
            ),
            format!("\"played_at\":{}", self.played_at),
        ];
        for (key, value) in [
            ("artist", &self.artist),
            ("album", &self.album),
            ("title", &self.title),
        ] {
            if let Some(text) = value {
                fields.push(format!("\"{key}\":\"{}\"", escape(text)));
            }
        }
        format!("{{{}}}", fields.join(","))
    }
}

/// Why a history file could not be read or written.
#[derive(Debug)]
pub enum Error {
    /// The file exists but cannot be read.
    Read(io::Error),
    /// The file is not the TOML this module writes.
    Parse(toml::de::Error),
    /// The file cannot be written.
    Write(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(error) | Self::Write(error) => write!(formatter, "{error}"),
            Self::Parse(error) => write!(formatter, "not a history file: {error}"),
        }
    }
}

/// The recently played entries, newest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct History {
    /// One table per beginning: `[[entry]]`, newest first.
    pub entry: Vec<Entry>,
}

impl History {
    /// Reads the history at `path`. A missing file is an empty history, not an
    /// error: nobody who never played anything is missing something.
    ///
    /// # Errors
    /// [`Error::Read`] when the file cannot be read, [`Error::Parse`] when it
    /// does not parse.
    pub fn load(path: &Path) -> Result<Self, Error> {
        match fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(Error::Parse),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(Error::Read(error)),
        }
    }

    /// Writes the history to `path` atomically: a temporary file in the same
    /// directory, flushed to disk, then renamed over the target, so the file
    /// on disk is never half a history. The directory is created when missing
    /// and the temporary file is removed when anything fails.
    ///
    /// # Errors
    /// [`Error::Write`] when the directory cannot be created, the temporary
    /// file cannot be written or flushed, or the rename fails.
    pub fn save(&self, path: &Path) -> Result<(), Error> {
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(dir).map_err(Error::Write)?;
        let body =
            toml::to_string_pretty(self).map_err(|error| Error::Write(io::Error::other(error)))?;
        let document = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(FILE_NAME);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let temp = dir.join(format!(".{document}.tmp.{}-{nanos}", std::process::id()));

        let written = (|| -> io::Result<()> {
            let mut file = fs::File::create_new(&temp)?;
            file.write_all(body.as_bytes())?;
            file.flush()?;
            file.sync_all()
        })();
        if let Err(error) = written {
            let _ = fs::remove_file(&temp);
            return Err(Error::Write(error));
        }
        match fs::rename(&temp, path) {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = fs::remove_file(&temp);
                Err(Error::Write(error))
            }
        }
    }

    /// Records a beginning: the entry goes to the top, any earlier entry
    /// with the same identity is dropped (playing it again moves it up
    /// instead of duplicating it — the same station twice is one entry,
    /// however its URL was written), and the list is trimmed to [`CAP`],
    /// oldest first.
    pub fn record(&mut self, begun: Entry) {
        let identity = begun.identity();
        self.entry.retain(|entry| entry.identity() != identity);
        self.entry.insert(0, begun);
        self.entry.truncate(CAP);
    }

    /// Empties the file at `path`, writing the empty document rather than
    /// removing it: the user asked for no entries, not for a missing file.
    ///
    /// # Errors
    /// [`Error::Write`] when the empty document cannot be written.
    pub fn clear(path: &Path) -> Result<(), Error> {
        Self::default().save(path)
    }

    /// The entries as a JSON array, newest first, for `velo history --json`.
    #[must_use]
    pub fn to_json(&self) -> String {
        let rows: Vec<String> = self.entry.iter().map(Entry::to_json).collect();
        format!("[{}]", rows.join(","))
    }
}

/// One JSON string literal's worth of `text`: the quotes, the backslash and
/// the control characters escaped, everything else as written.
fn escape(text: &str) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if control < ' ' => {
                let _ = write!(out, "\\u{:04x}", u32::from(control));
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        reason = "a test that cannot unwrap has already failed"
    )]

    use super::*;

    fn entry(path: &str, at: u64) -> Entry {
        Entry {
            path: path.to_owned(),
            played_at: at,
            ..Entry::default()
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("velo-history-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn a_missing_file_is_an_empty_history() {
        let dir = temp_dir("missing");
        let history = History::load(&dir.join("history.toml")).unwrap();
        assert_eq!(history, History::default());
    }

    #[test]
    fn recording_puts_the_newest_first() {
        let mut history = History::default();
        history.record(entry("/music/one.flac", 10));
        history.record(entry("/music/two.flac", 20));
        assert_eq!(
            history
                .entry
                .iter()
                .map(|e| e.path.as_str())
                .collect::<Vec<_>>(),
            ["/music/two.flac", "/music/one.flac"]
        );
    }

    #[test]
    fn the_same_path_moves_up_instead_of_duplicating() {
        let mut history = History::default();
        history.record(entry("/music/one.flac", 10));
        history.record(entry("/music/two.flac", 20));
        history.record(entry("/music/one.flac", 30));
        assert_eq!(history.entry.len(), 2, "no duplicate row");
        assert_eq!(history.entry[0].path, "/music/one.flac");
        assert_eq!(history.entry[0].played_at, 30, "the newest moment wins");
        assert_eq!(history.entry[1].path, "/music/two.flac");
    }

    #[test]
    fn the_list_is_capped_oldest_first() {
        let mut history = History::default();
        for index in 0..CAP + 20 {
            history.record(entry(&format!("/music/{index}.flac"), index as u64));
        }
        assert_eq!(history.entry.len(), CAP);
        assert_eq!(history.entry[0].path, format!("/music/{}.flac", CAP + 19));
        assert_eq!(
            history.entry[CAP - 1].path,
            "/music/20.flac",
            "the oldest went"
        );
    }

    #[test]
    fn a_save_and_load_round_trip_keeps_the_order_and_the_metadata() {
        let dir = temp_dir("round-trip");
        let file = path_beside(&dir.join("config.toml"));
        let mut history = History::default();
        history.record(Entry {
            path: String::from("subsonic://music.example/rest/stream?id=42"),
            kind: Kind::File,
            played_at: 1_762_668_000,
            artist: Some(String::from("Tide")),
            album: Some(String::from("Harbor")),
            title: Some(String::from("Nightfall")),
        });
        history.save(&file).unwrap();
        assert_eq!(History::load(&file).unwrap(), history);
        assert_eq!(
            file.file_name().unwrap(),
            "history.toml",
            "it lives beside the config file"
        );
    }

    #[test]
    fn a_corrupt_file_is_an_error_and_never_a_panic() {
        let dir = temp_dir("corrupt");
        let file = dir.join("history.toml");
        fs::write(&file, "[[entry]\npath = ").unwrap();
        let error = History::load(&file).unwrap_err();
        assert!(
            matches!(error, Error::Parse(_)),
            "a broken file is reported: {error}"
        );
    }

    #[test]
    fn unknown_keys_and_missing_fields_are_tolerated() {
        let dir = temp_dir("forward");
        let file = dir.join("history.toml");
        fs::write(
            &file,
            "future = 1\n[[entry]]\npath = \"/music/one.flac\"\nplayed_at = 5\nmood = \"calm\"\n",
        )
        .unwrap();
        let history = History::load(&file).unwrap();
        assert_eq!(history.entry, [entry("/music/one.flac", 5)]);
    }

    #[test]
    fn clear_writes_an_empty_document() {
        let dir = temp_dir("clear");
        let file = dir.join("history.toml");
        let mut history = History::default();
        history.record(entry("/music/one.flac", 10));
        history.save(&file).unwrap();

        History::clear(&file).unwrap();
        assert_eq!(History::load(&file).unwrap(), History::default());
        assert!(file.exists(), "the file stays, empty");
    }

    #[test]
    fn a_label_prefers_the_artist_then_the_title_then_the_file_name() {
        let full = Entry {
            path: String::from("/music/Tide/01 - Nightfall.flac"),
            artist: Some(String::from("Tide")),
            title: Some(String::from("Nightfall")),
            ..Entry::default()
        };
        assert_eq!(full.label(), "Tide \u{2014} Nightfall");

        let title_only = Entry {
            artist: None,
            title: Some(String::from("Nightfall")),
            ..full.clone()
        };
        assert_eq!(title_only.label(), "Nightfall");

        let bare = entry("/music/Tide/01 - Nightfall.flac", 10);
        assert_eq!(bare.label(), "01 - Nightfall");
    }

    #[test]
    fn json_is_an_array_newest_first_with_the_optional_fields_when_known() {
        let mut history = History::default();
        history.record(entry("/music/one.flac", 10));
        history.record(Entry {
            path: String::from("/music/two.flac"),
            played_at: 20,
            artist: Some(String::from("Tide")),
            title: Some(String::from("Nightfall")),
            ..Entry::default()
        });
        assert_eq!(
            history.to_json(),
            concat!(
                r#"[{"path":"/music/two.flac","kind":"file","played_at":20,"artist":"Tide","title":"Nightfall"},"#,
                r#"{"path":"/music/one.flac","kind":"file","played_at":10}]"#
            )
        );
    }

    #[test]
    fn json_says_the_kind_of_every_entry() {
        let mut history = History::default();
        history.record(Entry::station("https://stream.example/x", "Nova", 10));
        history.record(entry("/music/one.flac", 20));
        assert_eq!(
            history.to_json(),
            concat!(
                r#"[{"path":"/music/one.flac","kind":"file","played_at":20},"#,
                r#"{"path":"https://stream.example/x","kind":"station","played_at":10,"title":"Nova"}]"#
            )
        );
    }

    #[test]
    fn a_station_round_trips_through_save_and_load_with_its_kind() {
        let dir = temp_dir("station-round-trip");
        let file = path_beside(&dir.join("config.toml"));
        let mut history = History::default();
        history.record(Entry::station(
            "https://stream.example/nova",
            "Nova",
            1_762_668_000,
        ));
        history.save(&file).unwrap();

        let loaded = History::load(&file).unwrap();
        assert_eq!(loaded, history);
        assert!(loaded.entry[0].is_station());
        assert_eq!(loaded.entry[0].path, "https://stream.example/nova");
        assert_eq!(loaded.entry[0].title.as_deref(), Some("Nova"));
        assert_eq!(loaded.entry[0].artist, None, "a station has no artist");
        assert_eq!(loaded.entry[0].album, None, "a station has no album");
        assert_eq!(loaded.entry[0].label(), "Nova", "the name is the label");
    }

    #[test]
    fn two_writings_of_one_station_are_one_entry_but_a_track_with_the_same_text_is_not() {
        let mut history = History::default();
        history.record(Entry::station("https://Stream.example:443/x", "Nova", 10));
        history.record(Entry::station("https://stream.example/x", "Nova", 20));
        assert_eq!(history.entry.len(), 1, "one station however it is written");
        assert_eq!(history.entry[0].played_at, 20, "the newest moment wins");

        history.record(entry("https://stream.example/x", 30));
        assert_eq!(history.entry.len(), 2, "a file path is never a station");
        assert!(!history.entry[0].is_station());
        assert_eq!(history.entry[0].identity(), "file:https://stream.example/x");
        assert_eq!(
            history.entry[1].identity(),
            "station:https://stream.example/x"
        );
    }

    #[test]
    fn an_entry_written_before_kind_existed_loads_as_a_file() {
        let dir = temp_dir("old-kind");
        let file = dir.join("history.toml");
        fs::write(
            &file,
            "[[entry]]\npath = \"/music/one.flac\"\nplayed_at = 5\n",
        )
        .unwrap();

        let history = History::load(&file).unwrap();
        assert_eq!(history.entry, [entry("/music/one.flac", 5)]);
        assert!(!history.entry[0].is_station());
    }

    #[test]
    fn json_escapes_what_would_otherwise_break_the_document() {
        let entry = Entry {
            path: String::from("/music/quo\"te\\back\nnew\tline.flac"),
            played_at: 10,
            ..Entry::default()
        };
        assert_eq!(
            entry.to_json(),
            r#"{"path":"/music/quo\"te\\back\nnew\tline.flac","kind":"file","played_at":10}"#
        );
    }
}
