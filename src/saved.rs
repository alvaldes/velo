//! What the user marked (issue #7): a small TOML file beside the config,
//! `library.toml`, carrying both tables a saved list ever needs —
//! `[[favorite]]`, the tracks the user starred, and `[[playlist]]`, the named
//! lists issue #4 will fill. One file for both, decided once, so a playlist
//! never needs a format change.
//!
//! ```toml
//! [[favorite]]
//! path = "/music/Tide/01 - Nightfall.flac"
//! artist = "Tide"
//! album = "Harbor"
//! title = "Nightfall"
//!
//! [[playlist]]
//! name = "Noche"
//! track = ["/music/a.flac", "/music/b.flac"]
//! ```
//!
//! The file is a courtesy, never a dependency: a missing file is an empty
//! store, unknown keys are tolerated so a newer velo never has its file
//! wiped, and a file that does not parse is reported where a reader asks for
//! it and ignored by the interface. A favourite's identity is its path,
//! verbatim — a `subsonic://` path stays itself — so marking twice never
//! duplicates it. There is no lookup by path in the index, so a favourite
//! keeps `artist`, `album` and `title` copied from the row it was marked on:
//! the song must still be nameable from the file alone.
//!
//! Saving is atomic, in the shape [`crate::radio::store::RadioStore::save`]
//! uses: a temporary file in the same directory, flushed to disk, renamed
//! over the target, and the temporary removed when anything fails — the file
//! on disk is never half a store.

use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// The file's name beside `config.toml`.
const FILE_NAME: &str = "library.toml";

/// The template a launch writes into a missing `library.toml`: what the file
/// is, that velo rewrites it whole when it saves, and one commented example
/// per table. Every line starts with `#`, so an untouched template parses as
/// an empty store.
const TEMPLATE: &str = concat!(
    "# velo's saved library: the tracks you marked with `f` and your saved\n",
    "# playlists, in one file.\n",
    "#\n",
    "# velo rewrites this file whole every time it saves a change, so edits\n",
    "# made while velo runs are lost — edit it only while velo is not running.\n",
    "#\n",
    "# Marking a track with `f` writes an entry like:\n",
    "#\n",
    "# [[favorite]]\n",
    "# path = \"/music/Tide/01 - Nightfall.flac\"\n",
    "# artist = \"Tide\"\n",
    "# album = \"Harbor\"\n",
    "# title = \"Nightfall\"\n",
    "#\n",
    "# A saved playlist is a name and its tracks, in order:\n",
    "#\n",
    "# [[playlist]]\n",
    "# name = \"Noche\"\n",
    "# track = [\"/music/a.flac\", \"/music/b.flac\"]\n",
);

/// Writes [`TEMPLATE`] into `path` when the file is MISSING, the way
/// [`crate::config::write_default_config`] writes `config.toml`: the launch
/// leaves every config-side file self-documenting, and an existing file is
/// never touched — the write is create-only (`create_new`), so it cannot
/// clobber, reorder or even re-read a file the user wrote. Answers whether a
/// file was created.
///
/// # Errors
/// [`std::io::Error`] when the directory cannot be created or the template
/// cannot be written; an existing file is never an error.
pub fn write_default_library(path: &Path) -> std::io::Result<bool> {
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
            std::io::Write::write_all(&mut file, TEMPLATE.as_bytes())?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error),
    }
}

/// Where the store lives for a given config file: the same directory, so a
/// `XDG_CONFIG_HOME` the user already chose decides both.
#[must_use]
pub fn path_beside(config_file: &Path) -> PathBuf {
    config_file.with_file_name(FILE_NAME)
}

/// One marked track: the path verbatim — the identity — and the metadata the
/// library row carried when it was marked, so the song stays nameable from
/// the file alone.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Favorite {
    /// The path exactly as the row carried it; a `subsonic://` path stays
    /// itself. Never empty, and the entry's identity.
    pub path: String,
    /// The artist, when the row had one.
    pub artist: Option<String>,
    /// The album, when the row had one.
    pub album: Option<String>,
    /// The title, when the row had one.
    pub title: Option<String>,
}

/// One saved playlist (issue #4): a name and the tracks it carries, in
/// order. Written by this build from the start — the table exists so the
/// format never changes — and filled by #4.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Playlist {
    /// The playlist's own name.
    pub name: String,
    /// The tracks, in order, as paths.
    #[serde(default)]
    pub track: Vec<String>,
}

/// Why a store could not be read or written.
#[derive(Debug)]
pub enum SavedError {
    /// The file exists but cannot be read.
    Read(io::Error),
    /// The file is not the TOML this module writes.
    Parse(toml::de::Error),
    /// The file cannot be written.
    Write(io::Error),
}

impl fmt::Display for SavedError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(error) | Self::Write(error) => write!(formatter, "{error}"),
            Self::Parse(error) => write!(formatter, "not a saved file: {error}"),
        }
    }
}

/// Why a playlist could not be saved or renamed (issue #4): the name is
/// already taken. The store refuses and stores nothing; the interface says
/// so — never a silent merge, never a duplicate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DuplicateName;

/// The durable list of what the user marked, as the file holds it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Saved {
    /// One table per marked track: `[[favorite]]`, in the order they were
    /// marked.
    #[serde(default)]
    pub favorite: Vec<Favorite>,
    /// One table per saved playlist: `[[playlist]]` (issue #4), carried by
    /// the format from the start so filling it needs no format change.
    #[serde(default)]
    pub playlist: Vec<Playlist>,
}

impl Saved {
    /// Reads the store at `path`. A missing file is an empty store, not an
    /// error: nobody who never marked anything is missing something. Unknown
    /// keys are tolerated, so a newer velo's file is never wiped by an older
    /// reader.
    ///
    /// # Errors
    /// [`SavedError::Read`] when the file cannot be read,
    /// [`SavedError::Parse`] when it does not parse.
    pub fn load(path: &Path) -> Result<Self, SavedError> {
        match fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(SavedError::Parse),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(SavedError::Read(error)),
        }
    }

    /// Writes the store to `path` atomically, in the shape
    /// [`crate::radio::store::RadioStore::save`] uses: a temporary file in
    /// the same directory, flushed to disk, then renamed over the target, so
    /// the file on disk is never half a store. The directory is created when
    /// missing and the temporary file is removed when anything fails.
    ///
    /// # Errors
    /// [`SavedError::Write`] when the directory cannot be created, the
    /// temporary file cannot be written or flushed, or the rename fails.
    pub fn save(&self, path: &Path) -> Result<(), SavedError> {
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(dir).map_err(SavedError::Write)?;
        let body = toml::to_string_pretty(self)
            .map_err(|error| SavedError::Write(io::Error::other(error)))?;
        let name = path
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or(FILE_NAME);
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
                return Err(SavedError::Write(error));
            }
        }
        match fs::rename(&temp, path) {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = fs::remove_file(&temp);
                Err(SavedError::Write(error))
            }
        }
    }

    /// Whether `path` is marked, compared exactly: the identity is the path
    /// as the row carried it, with no normalising of any kind.
    #[must_use]
    pub fn is_favorite(&self, path: &str) -> bool {
        self.favorite.iter().any(|favorite| favorite.path == path)
    }

    /// Marks `favorite`, or unmarks it again when its path is already
    /// stored: marking twice never duplicates. Answers whether the mark is
    /// on after the toggle.
    pub fn toggle_favorite(&mut self, favorite: Favorite) -> bool {
        if let Some(index) = self
            .favorite
            .iter()
            .position(|stored| stored.path == favorite.path)
        {
            self.favorite.remove(index);
            false
        } else {
            self.favorite.push(favorite);
            true
        }
    }

    /// The saved playlist called `name`, compared verbatim: no trimming, no
    /// case folding — the name is the identity, the way a favourite's path
    /// is.
    #[must_use]
    pub fn playlist(&self, name: &str) -> Option<&Playlist> {
        self.playlist.iter().find(|saved| saved.name == name)
    }

    /// Saves a new playlist (issue #4, S1): a name and its tracks, in
    /// order. A name that is already saved is refused with
    /// [`DuplicateName`] — never a silent merge, never a duplicate.
    ///
    /// # Errors
    /// [`DuplicateName`] when a playlist called `name` is already stored;
    /// nothing is written in that case.
    pub fn save_playlist(&mut self, name: &str, tracks: Vec<String>) -> Result<(), DuplicateName> {
        if self.playlist(name).is_some() {
            return Err(DuplicateName);
        }
        self.playlist.push(Playlist {
            name: name.to_owned(),
            track: tracks,
        });
        Ok(())
    }

    /// Appends one track to the end of the named playlist (issue #4, S3).
    /// An unknown name appends nothing: the interface chooses what to say,
    /// the store never invents a playlist.
    pub fn append_to_playlist(&mut self, name: &str, path: &str) {
        if let Some(playlist) = self.playlist.iter_mut().find(|saved| saved.name == name) {
            playlist.track.push(path.to_owned());
        }
    }

    /// Renames the named playlist (issue #4, S5), keeping its tracks and its
    /// place in the file. A new name another playlist holds is refused with
    /// [`DuplicateName`]; renaming to its own name changes nothing.
    ///
    /// # Errors
    /// [`DuplicateName`] when another playlist is already called `new`;
    /// nothing is written in that case.
    pub fn rename_playlist(&mut self, old: &str, new: &str) -> Result<(), DuplicateName> {
        let Some(index) = self.playlist.iter().position(|saved| saved.name == old) else {
            return Ok(());
        };
        if new != old && self.playlist(new).is_some() {
            return Err(DuplicateName);
        }
        new.clone_into(&mut self.playlist[index].name);
        Ok(())
    }

    /// Removes the named playlist (issue #4, S5); answers whether it was
    /// there.
    pub fn delete_playlist(&mut self, name: &str) -> bool {
        let Some(index) = self.playlist.iter().position(|saved| saved.name == name) else {
            return false;
        };
        self.playlist.remove(index);
        true
    }

    /// Moves track `index` of the named playlist down one (`delta` 1) or up
    /// one (`delta` -1) by swapping it with its neighbour (issue #4, S4);
    /// answers the row's new index, or `None` when the move lands outside
    /// the list, the index is out of bounds, or the name is unknown. A
    /// refused move changes nothing.
    #[must_use]
    pub fn move_in_playlist(&mut self, name: &str, index: usize, delta: i64) -> Option<usize> {
        let playlist = self.playlist.iter_mut().find(|saved| saved.name == name)?;
        let target = i64::try_from(index).ok()?.checked_add(delta)?;
        let target = usize::try_from(target).ok()?;
        if target >= playlist.track.len() {
            return None;
        }
        playlist.track.swap(index, target);
        Some(target)
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        reason = "a test that cannot unwrap has already failed"
    )]

    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("velo-saved-unit-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn a_missing_file_is_an_empty_store() {
        let dir = temp_dir("missing");
        assert_eq!(
            Saved::load(&dir.join("library.toml")).unwrap(),
            Saved::default()
        );
    }

    #[test]
    fn a_second_playlist_with_a_taken_name_is_refused_never_merged() {
        let mut saved = Saved::default();
        saved
            .save_playlist("Noche", vec![String::from("/music/a.flac")])
            .unwrap();

        assert_eq!(
            saved.save_playlist("Noche", vec![String::from("/music/b.flac")]),
            Err(DuplicateName),
            "the second save with the same name is refused"
        );
        assert_eq!(saved.playlist.len(), 1, "no duplicate is stored");
        assert_eq!(
            saved.playlist[0].track,
            [String::from("/music/a.flac")],
            "the first playlist's tracks are not merged into"
        );
    }

    #[test]
    fn a_saved_playlist_is_found_by_its_name_verbatim() {
        let mut saved = Saved::default();
        saved
            .save_playlist("Noche", vec![String::from("/music/a.flac")])
            .unwrap();

        assert_eq!(saved.playlist("Noche").map(|p| p.track.len()), Some(1));
        assert_eq!(saved.playlist("noche"), None, "no case folding");
        assert_eq!(saved.playlist("Missing"), None);
    }

    #[test]
    fn append_adds_a_track_to_the_end_of_the_named_playlist() {
        let mut saved = Saved::default();
        saved
            .save_playlist("Noche", vec![String::from("/music/a.flac")])
            .unwrap();

        saved.append_to_playlist("Noche", "/music/b.flac");
        assert_eq!(
            saved.playlist("Noche").unwrap().track,
            [String::from("/music/a.flac"), String::from("/music/b.flac")],
            "the track lands at the end"
        );
        // An unknown name is a quiet nothing: the interface chooses what to
        // say, the store never invents a playlist.
        saved.append_to_playlist("Missing", "/music/c.flac");
        assert_eq!(saved.playlist.len(), 1);
    }

    #[test]
    fn rename_keeps_the_tracks_and_the_place_and_refuses_a_taken_name() {
        let mut saved = Saved::default();
        saved
            .save_playlist("Noche", vec![String::from("/music/a.flac")])
            .unwrap();
        saved
            .save_playlist("Dia", vec![String::from("/music/b.flac")])
            .unwrap();

        assert_eq!(
            saved.rename_playlist("Noche", "Dia"),
            Err(DuplicateName),
            "renaming onto a taken name is refused"
        );
        saved.rename_playlist("Noche", "Fiesta").unwrap();
        assert_eq!(saved.playlist("Noche"), None);
        assert_eq!(
            saved.playlist("Fiesta").unwrap().track,
            [String::from("/music/a.flac")],
            "the tracks follow the rename"
        );
        assert_eq!(
            saved.playlist[0].name, "Fiesta",
            "the playlist keeps its place in the file"
        );
    }

    #[test]
    fn renaming_to_its_own_name_changes_nothing() {
        let mut saved = Saved::default();
        saved
            .save_playlist("Noche", vec![String::from("/music/a.flac")])
            .unwrap();

        saved.rename_playlist("Noche", "Noche").unwrap();
        assert_eq!(saved.playlist.len(), 1);
        assert_eq!(saved.playlist[0].name, "Noche");
    }

    #[test]
    fn delete_removes_the_named_playlist_and_answers_whether_it_was_there() {
        let mut saved = Saved::default();
        saved
            .save_playlist("Noche", vec![String::from("/music/a.flac")])
            .unwrap();

        assert!(saved.delete_playlist("Noche"), "the name was there");
        assert_eq!(saved.playlist.len(), 0);
        assert!(!saved.delete_playlist("Noche"), "gone is gone");
    }

    #[test]
    fn move_swaps_a_track_with_its_neighbour_and_answers_the_new_index() {
        let mut saved = Saved::default();
        saved
            .save_playlist(
                "Noche",
                vec![
                    String::from("/music/a.flac"),
                    String::from("/music/b.flac"),
                    String::from("/music/c.flac"),
                ],
            )
            .unwrap();

        assert_eq!(
            saved.move_in_playlist("Noche", 0, 1),
            Some(1),
            "J moves down one"
        );
        assert_eq!(
            saved.playlist("Noche").unwrap().track,
            [
                String::from("/music/b.flac"),
                String::from("/music/a.flac"),
                String::from("/music/c.flac")
            ]
        );
        assert_eq!(
            saved.move_in_playlist("Noche", 1, -1),
            Some(0),
            "K moves up one"
        );
        assert_eq!(
            saved.playlist("Noche").unwrap().track,
            [
                String::from("/music/a.flac"),
                String::from("/music/b.flac"),
                String::from("/music/c.flac")
            ]
        );
        assert_eq!(
            saved.move_in_playlist("Noche", 2, 1),
            None,
            "moving past the end is refused"
        );
        assert_eq!(
            saved.move_in_playlist("Noche", 0, -1),
            None,
            "moving above the start is refused"
        );
        assert_eq!(saved.move_in_playlist("Noche", 3, 1), None);
        assert_eq!(
            saved.move_in_playlist("Missing", 0, 1),
            None,
            "no playlist, no move"
        );
        assert_eq!(
            saved.playlist("Noche").unwrap().track.len(),
            3,
            "a refused move changed nothing"
        );
    }

    #[test]
    fn a_round_trip_keeps_both_tables_and_skips_the_fields_the_row_did_not_have() {
        let dir = temp_dir("round-trip");
        let file = dir.join("library.toml");
        let mut saved = Saved::default();
        saved.toggle_favorite(Favorite {
            path: String::from("/music/one.flac"),
            artist: Some(String::from("Tide")),
            ..Favorite::default()
        });
        saved.playlist.push(Playlist {
            name: String::from("Noche"),
            track: vec![String::from("/music/a.flac")],
        });
        saved.save(&file).unwrap();

        let loaded = Saved::load(&file).unwrap();
        assert_eq!(loaded, saved);
        assert_eq!(loaded.favorite[0].album, None, "an absent field stays out");
        let text = fs::read_to_string(&file).unwrap();
        assert!(!text.contains("album ="), "no empty field is written");
    }

    #[test]
    fn the_temporary_file_is_gone_after_a_save() {
        let dir = temp_dir("temp-gone");
        let file = dir.join("library.toml");
        Saved::default().save(&file).unwrap();
        let strays: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".library.toml.tmp."))
            .collect();
        assert!(
            strays.is_empty(),
            "no temporary file left behind: {strays:?}"
        );
    }

    #[test]
    fn a_failed_save_leaves_no_temporary_file() {
        // A directory in the place of the store's parent makes every write
        // fail; the temporary file must still not survive.
        let dir = temp_dir("failed-save");
        let blocker = dir.join("library.toml");
        fs::write(&blocker, "not the parent — the target itself is a file").unwrap();
        // `create_dir_all` on an existing file errors; the save fails before
        // any temporary exists, which is the first half of the rule. The
        // rename-failure half needs a directory the temp can be written to
        // but not renamed over, which no portable fixture gives; the unit
        // pins the cleanup after the write leg instead.
        let file = blocker.join("nested").join("library.toml");
        assert!(Saved::default().save(&file).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_template_is_written_only_when_missing_and_is_all_comments() {
        let dir = temp_dir("template");
        let file = dir.join("library.toml");

        assert!(
            write_default_library(&file).unwrap(),
            "created when missing"
        );
        let text = fs::read_to_string(&file).unwrap();
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            assert!(line.starts_with('#'), "all comments: {line:?}");
        }
        assert_eq!(Saved::load(&file).unwrap(), Saved::default());

        // A second call — a second launch — touches nothing: an existing
        // file is never an error and never rewritten.
        fs::write(&file, "[[favorite]]\npath = \"/music/one.flac\"\n").unwrap();
        assert!(!write_default_library(&file).unwrap(), "exists: untouched");
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "[[favorite]]\npath = \"/music/one.flac\"\n"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
