//! The library index (S6): one SQLite database that mirrors a music folder.
//!
//! Scanning is parallel ([`jwalk`]), tags come from [`lofty`], and the path
//! layout `Artist/YYYY - Album/NN - Title.ext` is only a fallback for files
//! that carry no tag — a missing tag never drops a file from the index.

mod scan;

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

pub use scan::AUDIO_EXTENSIONS;
use scan::scan;

/// The index: a SQLite connection holding the `tracks` table.
pub struct Library {
    conn: Connection,
}

/// Everything `--index` needs to report after a scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexReport {
    pub added: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub removed: usize,
}

/// Top-level counts, printed by `velo --index`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub tracks: u64,
    pub albums: u64,
    pub artists: u64,
}

/// One row of the artist browser: how much an artist holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtistEntry {
    pub name: String,
    pub tracks: u64,
    pub albums: u64,
}

/// One album of one artist, for the album browsers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumEntry {
    pub artist: String,
    pub title: String,
    pub year: Option<i64>,
    pub tracks: u64,
}

/// One genre and how many tracks carry it, for the genre browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenreEntry {
    pub name: String,
    pub tracks: u64,
}

/// The genre that holds every track whose file names none, so no track is
/// missing from the genre browser.
pub const NO_GENRE: &str = "(no genre)";

/// The schema this build writes: 1 added the `genre` column.
const SCHEMA_VERSION: i64 = 1;

/// One track of one album, for the track browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackEntry {
    pub artist: String,
    pub album: String,
    pub title: String,
    pub track_no: Option<i64>,
    pub disc_no: Option<i64>,
    pub duration_ms: i64,
    pub path: PathBuf,
}

/// Every failure the index can produce, as one type.
#[derive(Debug)]
pub enum LibraryError {
    Io(std::io::Error),
    Db(rusqlite::Error),
    DataDir,
}

impl fmt::Display for LibraryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Db(error) => write!(f, "{error}"),
            Self::DataDir => write!(f, "cannot determine the system data directory"),
        }
    }
}

impl std::error::Error for LibraryError {}

impl From<std::io::Error> for LibraryError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<rusqlite::Error> for LibraryError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Db(error)
    }
}

impl Library {
    /// Opens (creating if needed) the index at its default location:
    /// `dirs::data_dir()/velo/library.db`, or the `VELO_DB` path when set.
    ///
    /// # Errors
    ///
    /// Fails when the system data directory cannot be determined or the
    /// database cannot be created or opened.
    pub fn open() -> Result<Self, LibraryError> {
        let path = if let Some(override_path) = std::env::var_os("VELO_DB") {
            PathBuf::from(override_path)
        } else {
            let mut base = dirs::data_dir().ok_or(LibraryError::DataDir)?;
            base.push("velo");
            base.push("library.db");
            base
        };
        Self::open_at(&path)
    }

    /// Opens the index at an explicit path. Tests use this so they never
    /// touch the user's real index.
    ///
    /// # Errors
    ///
    /// Fails when the parent directory cannot be created or the database
    /// cannot be opened or its schema written.
    pub fn open_at(path: &Path) -> Result<Self, LibraryError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn)?;
        Ok(Self { conn })
    }

    /// Builds or refreshes the index for one root folder.
    ///
    /// Files whose mtime and size are unchanged are not re-read; rows whose
    /// file disappeared are deleted. Everything happens in one transaction,
    /// so a re-index stays cheap even on a 40 000-track library.
    ///
    /// # Errors
    ///
    /// Fails when the tree cannot be walked or the database cannot be
    /// written; the index is left as it was before the call.
    pub fn index(&self, root: &Path) -> Result<IndexReport, LibraryError> {
        let mut known: HashMap<String, (i64, i64)> = HashMap::new();
        {
            let mut statement = self.conn.prepare("SELECT path, mtime, size FROM tracks")?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                let path: String = row.get(0)?;
                let mtime: i64 = row.get(1)?;
                let size: i64 = row.get(2)?;
                known.insert(path, (mtime, size));
            }
        }

        let outcome = scan(root, &known)?;

        let transaction = self.conn.unchecked_transaction()?;
        {
            let mut upsert = transaction.prepare_cached(UPSERT)?;
            for record in &outcome.fresh {
                upsert.execute(rusqlite::params![
                    record.path,
                    record.title,
                    record.artist,
                    record.album,
                    record.album_artist,
                    record.track_no,
                    record.disc_no,
                    record.year,
                    record.duration_ms,
                    record.sample_rate,
                    record.channels,
                    record.format,
                    i64::from(record.has_cover),
                    record.mtime,
                    record.size,
                    record.genre,
                ])?;
            }
        }
        let mut removed = 0_usize;
        {
            let mut delete = transaction.prepare_cached("DELETE FROM tracks WHERE path = ?1")?;
            for path in known.keys() {
                if !outcome.seen.contains(path) {
                    removed += delete.execute([path])?;
                }
            }
        }
        transaction.commit()?;

        Ok(IndexReport {
            added: outcome
                .fresh
                .iter()
                .filter(|r| !known.contains_key(&r.path))
                .count(),
            updated: outcome
                .fresh
                .iter()
                .filter(|r| known.contains_key(&r.path))
                .count(),
            unchanged: outcome.unchanged,
            removed,
        })
    }

    /// Tracks, distinct albums and distinct artists in the index.
    ///
    /// # Errors
    ///
    /// Fails when the queries cannot run.
    pub fn counts(&self) -> Result<Counts, LibraryError> {
        let one = |sql: &str| -> Result<u64, LibraryError> {
            let value: i64 = self.conn.query_row(sql, [], |row| row.get(0))?;
            // Counts are non-negative and far below i64::MAX.
            #[expect(
                clippy::cast_sign_loss,
                reason = "COUNT(*) never returns a negative value"
            )]
            Ok(value as u64)
        };
        Ok(Counts {
            tracks: one("SELECT COUNT(*) FROM tracks")?,
            albums: one("SELECT COUNT(*) FROM (SELECT 1 FROM tracks GROUP BY artist, album)")?,
            artists: one("SELECT COUNT(*) FROM (SELECT 1 FROM tracks GROUP BY artist)")?,
        })
    }

    /// Every artist with its track and album counts, name-insensitively ordered.
    ///
    /// # Errors
    ///
    /// Fails when the query cannot run.
    pub fn artists(&self) -> Result<Vec<ArtistEntry>, LibraryError> {
        let mut statement = self.conn.prepare(
            "SELECT artist, COUNT(*) AS tracks, COUNT(DISTINCT album) AS albums
             FROM tracks GROUP BY artist ORDER BY artist",
        )?;
        let rows = statement.query_map([], |row| {
            let tracks: i64 = row.get(1)?;
            let albums: i64 = row.get(2)?;
            Ok(ArtistEntry {
                name: row.get(0)?,
                tracks: sign_to_count(tracks),
                albums: sign_to_count(albums),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// The albums of one artist, ordered by year then name.
    ///
    /// # Errors
    ///
    /// Fails when the query cannot run.
    pub fn albums_of(&self, artist: &str) -> Result<Vec<AlbumEntry>, LibraryError> {
        let mut statement = self.conn.prepare(
            "SELECT album, MAX(year) AS year, COUNT(*) AS tracks
             FROM tracks WHERE artist = ?1
             GROUP BY album ORDER BY year, album",
        )?;
        let rows = statement.query_map([artist], |row| {
            let tracks: i64 = row.get(2)?;
            Ok(AlbumEntry {
                artist: artist.to_owned(),
                title: row.get(0)?,
                year: row.get(1)?,
                tracks: sign_to_count(tracks),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// The tracks of one album, ordered by disc then track number; tracks
    /// without numbers fall to the end of their group, ordered by title.
    ///
    /// # Errors
    ///
    /// Fails when the query cannot run.
    pub fn tracks_of(&self, artist: &str, album: &str) -> Result<Vec<TrackEntry>, LibraryError> {
        let mut statement = self.conn.prepare(
            "SELECT title, track_no, disc_no, duration_ms, path
             FROM tracks WHERE artist = ?1 AND album = ?2
             ORDER BY COALESCE(disc_no, 0), COALESCE(track_no, 1000000), title",
        )?;
        let rows = statement.query_map([artist, album], |row| {
            Ok(TrackEntry {
                artist: artist.to_owned(),
                album: album.to_owned(),
                title: row.get(0)?,
                track_no: row.get(1)?,
                disc_no: row.get(2)?,
                duration_ms: row.get(3)?,
                path: PathBuf::from(row.get::<_, String>(4)?),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Every indexed track, `artist`/`album` first and, inside one album, in
    /// the same order the album browser shows. The search over the whole
    /// library reads through this: one indexed scan over the `(artist, album)`
    /// sorted rows, not one query per row.
    ///
    /// # Errors
    ///
    /// Fails when the query cannot run.
    pub fn all_tracks(&self) -> Result<Vec<TrackEntry>, LibraryError> {
        Ok(self
            .tracks_with_genre()?
            .into_iter()
            .map(|(track, _)| track)
            .collect())
    }

    /// Every album of every artist, one row each, ordered by album title and
    /// then artist (both case-insensitively).
    ///
    /// # Errors
    ///
    /// Fails when the query cannot run.
    pub fn all_albums(&self) -> Result<Vec<AlbumEntry>, LibraryError> {
        let mut statement = self.conn.prepare(
            "SELECT artist, album, MAX(year) AS year, COUNT(*) AS tracks
             FROM tracks GROUP BY artist, album ORDER BY album, artist",
        )?;
        let rows = statement.query_map([], |row| {
            let tracks: i64 = row.get(3)?;
            Ok(AlbumEntry {
                artist: row.get(0)?,
                title: row.get(1)?,
                year: row.get(2)?,
                tracks: sign_to_count(tracks),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Every genre with how many tracks carry it. A tag holding several
    /// (`Dub / Trip-Hop`, `Lo-Fi; Jazz`) counts the track in each; spellings
    /// that differ only in case are one genre, named as first met in
    /// [`Library::all_tracks`] order. Genres are ordered case-insensitively and
    /// [`NO_GENRE`], holding the tracks without one, comes last.
    ///
    /// # Errors
    ///
    /// Fails when the query cannot run.
    pub fn genres(&self) -> Result<Vec<GenreEntry>, LibraryError> {
        let mut genres: Vec<(String, GenreEntry)> = Vec::new();
        let mut untagged = 0_u64;
        for (_, genre) in self.tracks_with_genre()? {
            let pieces = split_genres(genre.as_deref().unwrap_or_default());
            if pieces.is_empty() {
                untagged += 1;
            }
            for (key, name) in pieces {
                match genres.iter_mut().find(|(known, _)| *known == key) {
                    Some((_, entry)) => entry.tracks += 1,
                    None => genres.push((key, GenreEntry { name, tracks: 1 })),
                }
            }
        }
        genres.sort_by(|(left, _), (right, _)| left.cmp(right));
        let mut genres: Vec<GenreEntry> = genres.into_iter().map(|(_, entry)| entry).collect();
        if untagged > 0 {
            genres.push(GenreEntry {
                name: NO_GENRE.to_owned(),
                tracks: untagged,
            });
        }
        Ok(genres)
    }

    /// The tracks of one genre (matched case-insensitively), in
    /// [`Library::all_tracks`] order; [`NO_GENRE`] lists the tracks without
    /// one.
    ///
    /// # Errors
    ///
    /// Fails when the query cannot run.
    pub fn tracks_of_genre(&self, genre: &str) -> Result<Vec<TrackEntry>, LibraryError> {
        let wanted = genre.trim().to_lowercase();
        let untagged = genre == NO_GENRE;
        Ok(self
            .tracks_with_genre()?
            .into_iter()
            .filter(|(_, tag)| {
                let pieces = split_genres(tag.as_deref().unwrap_or_default());
                if untagged {
                    pieces.is_empty()
                } else {
                    pieces.iter().any(|(key, _)| *key == wanted)
                }
            })
            .map(|(track, _)| track)
            .collect())
    }

    /// Every track with its raw genre tag, in [`Library::all_tracks`] order.
    fn tracks_with_genre(&self) -> Result<Vec<(TrackEntry, Option<String>)>, LibraryError> {
        let mut statement = self.conn.prepare(
            "SELECT title, track_no, disc_no, duration_ms, path, artist, album, genre
             FROM tracks
             ORDER BY artist, album, COALESCE(disc_no, 0), COALESCE(track_no, 1000000), title",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                TrackEntry {
                    title: row.get(0)?,
                    track_no: row.get(1)?,
                    disc_no: row.get(2)?,
                    duration_ms: row.get(3)?,
                    path: PathBuf::from(row.get::<_, String>(4)?),
                    artist: row.get(5)?,
                    album: row.get(6)?,
                },
                row.get(7)?,
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

/// The genres one tag names, as `(lowercase key, name as written)`: split on
/// `/`, `;`, `,` (and the NUL an `ID3v2` multi-value frame uses), trimmed,
/// empty pieces dropped, and a genre repeated in other case kept once.
fn split_genres(tag: &str) -> Vec<(String, String)> {
    let mut genres: Vec<(String, String)> = Vec::new();
    for piece in tag.split(['/', ';', ',', '\0']) {
        let name = piece.trim();
        if name.is_empty() {
            continue;
        }
        let key = name.to_lowercase();
        if !genres.iter().any(|(known, _)| *known == key) {
            genres.push((key, name.to_owned()));
        }
    }
    genres
}

/// Brings an index written by an older velo up to [`SCHEMA_VERSION`] without
/// losing a row. Version 0 had no `genre` column: it is added, and every
/// stored mtime is forgotten so the next [`Library::index`] re-reads each
/// file's tags — the rows themselves stay until that scan replaces them.
fn migrate(conn: &Connection) -> Result<(), LibraryError> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version >= SCHEMA_VERSION {
        return Ok(());
    }
    let transaction = conn.unchecked_transaction()?;
    let has_genre = transaction
        .prepare("SELECT 1 FROM pragma_table_info('tracks') WHERE name = 'genre'")?
        .exists([])?;
    if !has_genre {
        transaction.execute_batch("ALTER TABLE tracks ADD COLUMN genre TEXT")?;
    }
    transaction.execute("UPDATE tracks SET mtime = -1", [])?;
    transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    transaction.commit()?;
    Ok(())
}

/// `artist`/`album`/`title` are declared `COLLATE NOCASE`, so grouping and
/// ordering fold case everywhere; the composite index serves both the artist
/// browser (prefix) and the album browser.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS tracks (
    id           INTEGER PRIMARY KEY,
    path         TEXT NOT NULL UNIQUE,
    title        TEXT NOT NULL COLLATE NOCASE,
    artist       TEXT NOT NULL COLLATE NOCASE,
    album        TEXT NOT NULL COLLATE NOCASE,
    album_artist TEXT COLLATE NOCASE,
    track_no     INTEGER,
    disc_no      INTEGER,
    year         INTEGER,
    duration_ms  INTEGER NOT NULL DEFAULT 0,
    sample_rate  INTEGER,
    channels     INTEGER,
    format       TEXT NOT NULL,
    has_cover    INTEGER NOT NULL DEFAULT 0,
    mtime        INTEGER NOT NULL,
    size         INTEGER NOT NULL,
    genre        TEXT
);
CREATE INDEX IF NOT EXISTS idx_tracks_artist_album ON tracks(artist, album);
CREATE INDEX IF NOT EXISTS idx_tracks_album ON tracks(album);
";

const UPSERT: &str = "
INSERT INTO tracks (path, title, artist, album, album_artist, track_no, disc_no,
                    year, duration_ms, sample_rate, channels, format, has_cover,
                    mtime, size, genre)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
ON CONFLICT(path) DO UPDATE SET
    title = excluded.title,
    artist = excluded.artist,
    album = excluded.album,
    album_artist = excluded.album_artist,
    track_no = excluded.track_no,
    disc_no = excluded.disc_no,
    year = excluded.year,
    duration_ms = excluded.duration_ms,
    sample_rate = excluded.sample_rate,
    channels = excluded.channels,
    format = excluded.format,
    has_cover = excluded.has_cover,
    mtime = excluded.mtime,
    size = excluded.size,
    genre = excluded.genre
";

#[expect(
    clippy::cast_sign_loss,
    reason = "COUNT(*) never returns a negative value"
)]
fn sign_to_count(value: i64) -> u64 {
    value as u64
}
