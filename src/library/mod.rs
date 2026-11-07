//! The library index (S6): one SQLite database that mirrors a music folder.
//!
//! Scanning is parallel ([`jwalk`]), tags come from [`lofty`], and the path
//! layout `Artist/YYYY - Album/NN - Title.ext` is only a fallback for files
//! that carry no tag — a missing tag never drops a file from the index.

mod scan;
mod sync;

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

pub use scan::AUDIO_EXTENSIONS;
use scan::scan;
pub use sync::{SyncReport, source_for};

/// The index: a SQLite connection holding the `tracks` table.
pub struct Library {
    conn: Connection,
    /// Whether `tracks` has the `genre` column. Only a version-0 index in a
    /// file that cannot be written lacks it: it is never migrated, its
    /// genres are unknown and every track reads as [`NO_GENRE`].
    has_genre: bool,
    has_source: bool,
    /// Whether `tracks` has the `cover_id` column (T12d). Only a
    /// version-2-or-older index in a file that cannot be written lacks it:
    /// it is never migrated, and every track reads with no cover id until
    /// the first open that can migrate the file.
    has_cover_id: bool,
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

/// The source value of every row a local scan wrote.
pub const LOCAL_SOURCE: &str = "local";

/// The schema this build writes: 1 added the `genre` column, 2 added the
/// `source`/`remote_id` pair that lets a Subsonic server's library live in
/// the same table as the local files (T12b), and 3 added the `cover_id`
/// column that carries the server's `coverArt` id a remote cover is fetched
/// with (T12d).
const SCHEMA_VERSION: i64 = 3;

/// One track of one album, for the track browser. A row synced from a
/// Subsonic server carries the `source` it came from and the server's own
/// song id; a local row is `source = "local"`, `remote_id = None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackEntry {
    pub artist: String,
    pub album: String,
    pub title: String,
    pub track_no: Option<i64>,
    pub disc_no: Option<i64>,
    pub duration_ms: i64,
    pub path: PathBuf,
    /// Which index the row came from: [`LOCAL_SOURCE`] or `subsonic:<host>`.
    pub source: String,
    /// The server's own song id, when the row is remote.
    pub remote_id: Option<String>,
    /// The server's `coverArt` id the row's cover is fetched with (T12d):
    /// the song's own when it says one, else its album's. A remote cover is
    /// fetched with `getCoverArt?id=<cover_id>` into the covers cache.
    pub cover_id: Option<String>,
}

impl TrackEntry {
    /// Whether this row came from a Subsonic server rather than a file.
    /// Its `path` is a `subsonic://` URL, and the producer resolves it through
    /// the cache before opening anything (T12c).
    #[must_use]
    pub fn is_remote(&self) -> bool {
        self.source != LOCAL_SOURCE
    }
}

/// Every failure the index can produce, as one type.
#[derive(Debug)]
pub enum LibraryError {
    Io(std::io::Error),
    Db(rusqlite::Error),
    DataDir,
    /// The sync could not talk to the server; the message is the Subsonic
    /// error's own sentence. Nothing was written.
    Remote(String),
}

impl fmt::Display for LibraryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Db(error) => write!(f, "{error}"),
            Self::DataDir => write!(f, "cannot determine the system data directory"),
            Self::Remote(message) => write!(f, "{message}"),
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
    /// cannot be opened or its schema written. A file that cannot be written
    /// opens read-only and is not migrated: an index built by an older velo
    /// then browses with its genres unknown, and the first open once the
    /// file is writable migrates it.
    pub fn open_at(path: &Path) -> Result<Self, LibraryError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        if conn.is_readonly(rusqlite::MAIN_DB)?
            && let Some(uri) = read_only_uri(path, !has_wal_rows(path))
        {
            drop(conn);
            return Self::open_read_only(&uri);
        }
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn)?;
        Ok(Self {
            conn,
            has_genre: true,
            has_source: true,
            has_cover_id: true,
        })
    }

    /// Opens a file that cannot be written read-only. With no rows left in
    /// its WAL it is opened `immutable`, so reading it leaves no `-wal`/`-shm`
    /// files behind — such files would keep the file's read-only mode and
    /// block every write after it becomes writable again. With rows still in
    /// the WAL it is opened plainly read-only, so those rows are read.
    fn open_read_only(uri: &str) -> Result<Self, LibraryError> {
        let conn = Connection::open_with_flags(
            uri,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        // A no-op on a complete index; fails as before on one without tables.
        conn.execute_batch(SCHEMA)?;
        let has_genre = conn
            .prepare("SELECT 1 FROM pragma_table_info('tracks') WHERE name = 'genre'")?
            .exists([])?;
        // A read-only index cannot be migrated, so an old one is read as what
        // it is: every row in it was scanned from a folder, hence `local`.
        let has_source = conn
            .prepare("SELECT 1 FROM pragma_table_info('tracks') WHERE name = 'source'")?
            .exists([])?;
        let has_cover_id = conn
            .prepare("SELECT 1 FROM pragma_table_info('tracks') WHERE name = 'cover_id'")?
            .exists([])?;
        Ok(Self {
            conn,
            has_genre,
            has_source,
            has_cover_id,
        })
    }

    /// Runs `read` inside one deferred read transaction on this connection,
    /// so every query it makes sees the same snapshot of the index: a commit
    /// from another connection (the background indexer) lands either before
    /// all of them or after all of them. The transaction ends with `read`;
    /// nothing is written. For reads only: `read` must not call
    /// [`Library::index`], which opens a transaction of its own.
    ///
    /// # Errors
    ///
    /// The transaction cannot begin or end, or `read` fails; its error is
    /// returned as it is.
    pub fn snapshot<T>(
        &self,
        read: impl FnOnce(&Self) -> Result<T, LibraryError>,
    ) -> Result<T, LibraryError> {
        // Deferred: the snapshot is taken by the first read inside it.
        let transaction = self.conn.unchecked_transaction()?;
        let outcome = read(self)?;
        // Ending it this way writes nothing; a failed `read` rolls back on drop.
        transaction.commit()?;
        Ok(outcome)
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
        if !self.has_genre {
            // Only a read-only file lacks the column: migrating reports the
            // same read-only error the writes below would hit.
            migrate(&self.conn)?;
        }
        let mut known: HashMap<String, (i64, i64)> = HashMap::new();
        {
            // Only local rows belong to a scan; the remote rows belong to
            // their sync and are never touched here.
            let mut statement = self
                .conn
                .prepare("SELECT path, mtime, size FROM tracks WHERE source = 'local'")?;
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
            // The same bound as the `known` read: a scan removes local rows
            // only, never a row a sync wrote.
            let mut delete = transaction
                .prepare_cached("DELETE FROM tracks WHERE path = ?1 AND source = 'local'")?;
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

    /// The number of tracks per source, ordered by source name — `local`
    /// first, then each `subsonic:<host>`. This is what `velo --sync` prints.
    ///
    /// # Errors
    ///
    /// Fails when the query cannot run.
    pub fn counts_by_source(&self) -> Result<Vec<(String, u64)>, LibraryError> {
        // `local` is named even when it holds nothing: the printed line is
        // `local: 0 tracks · subsonic:<host>: N tracks`, never silently
        // missing the local index the user indexed yesterday.
        let mut statement = self.conn.prepare(
            "SELECT source, COUNT(*) FROM tracks GROUP BY source
             UNION SELECT 'local', 0
               WHERE NOT EXISTS (SELECT 1 FROM tracks WHERE source = 'local')
             ORDER BY 1",
        )?;
        let rows = statement.query_map([], |row| {
            let count: i64 = row.get(1)?;
            Ok((row.get(0)?, sign_to_count(count)))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
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
        let source = match (self.has_source, self.has_cover_id) {
            (true, true) => "source, remote_id, cover_id",
            (true, false) => "source, remote_id, NULL",
            (false, _) => "'local', NULL, NULL",
        };
        let mut statement = self.conn.prepare(&format!(
            "SELECT title, track_no, disc_no, duration_ms, path, {source}
             FROM tracks WHERE artist = ?1 AND album = ?2
             ORDER BY COALESCE(disc_no, 0), COALESCE(track_no, 1000000), title"
        ))?;
        let rows = statement.query_map([artist, album], |row| {
            Ok(TrackEntry {
                artist: artist.to_owned(),
                album: album.to_owned(),
                title: row.get(0)?,
                track_no: row.get(1)?,
                disc_no: row.get(2)?,
                duration_ms: row.get(3)?,
                path: PathBuf::from(row.get::<_, String>(4)?),
                source: row.get(5)?,
                remote_id: row.get(6)?,
                cover_id: row.get(7)?,
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
        let genre = if self.has_genre { "genre" } else { "NULL" };
        let source = match (self.has_source, self.has_cover_id) {
            (true, true) => "source, remote_id, cover_id",
            (true, false) => "source, remote_id, NULL",
            (false, _) => "'local', NULL, NULL",
        };
        let mut statement = self.conn.prepare(&format!(
            "SELECT title, track_no, disc_no, duration_ms, path, artist, album, {genre}, {source}
             FROM tracks
             ORDER BY artist, album, COALESCE(disc_no, 0), COALESCE(track_no, 1000000), title"
        ))?;
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
                    source: row.get(8)?,
                    remote_id: row.get(9)?,
                    cover_id: row.get(10)?,
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
/// Version 1 had no `source`/`remote_id` pair: the columns are added with
/// every existing row labelled `local`, and the `(source, remote_id)`
/// uniqueness that remote rows live under comes with them (T12b). Version 2
/// had no `cover_id` column: it is added empty — a row's cover id is filled
/// by the next `--sync`, so an existing index loses nothing and changes
/// nothing else (T12d). The whole migration is one transaction on columns
/// that `ALTER TABLE` already refuses to add twice, so running it twice is
/// harmless.
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
        // A genre-migrated index forgets its mtimes so the next scan re-reads
        // each file's tags; a source migration below must not force one.
        transaction.execute("UPDATE tracks SET mtime = -1", [])?;
    }
    // T12b: the `source`/`remote_id` pair. An old file's rows all came from
    // the local scan, so `DEFAULT 'local'` is exactly what they are.
    let has_source = transaction
        .prepare("SELECT 1 FROM pragma_table_info('tracks') WHERE name = 'source'")?
        .exists([])?;
    if !has_source {
        transaction
            .execute_batch("ALTER TABLE tracks ADD COLUMN source TEXT NOT NULL DEFAULT 'local'")?;
        transaction.execute_batch("ALTER TABLE tracks ADD COLUMN remote_id TEXT")?;
    }
    // T12d: the `coverArt` id a remote cover is fetched with. Existing rows
    // read `NULL` until the next sync upserts them with the id.
    let has_cover_id = transaction
        .prepare("SELECT 1 FROM pragma_table_info('tracks') WHERE name = 'cover_id'")?
        .exists([])?;
    if !has_cover_id {
        transaction.execute_batch("ALTER TABLE tracks ADD COLUMN cover_id TEXT")?;
    }
    // One server may not hold two rows with the same song id. A partial
    // unique index: local rows have no remote id and are unconstrained here.
    transaction.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_tracks_source_remote
         ON tracks(source, remote_id) WHERE remote_id IS NOT NULL",
    )?;
    transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    transaction.commit()?;
    Ok(())
}

/// The `file:` URI that opens `path` read-only and `immutable`, or `None`
/// when the path is not UTF-8. Bytes outside the unreserved set and `/` are
/// percent-encoded, so `?`, `#` and `%` in a folder name stay part of it.
fn read_only_uri(path: &Path, immutable: bool) -> Option<String> {
    use std::fmt::Write as _;
    let mut uri = String::from("file:");
    for byte in path.to_str()?.bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
            uri.push(char::from(byte));
        } else {
            let _ = write!(uri, "%{byte:02X}");
        }
    }
    uri.push_str(if immutable {
        "?mode=ro&immutable=1"
    } else {
        "?mode=ro"
    });
    Some(uri)
}

/// Whether committed rows may still sit in the index's `-wal` file — a velo
/// killed before it closed leaves them there. `immutable` would ignore them
/// (fewer tracks, or no tables at all), so such an index is opened plainly
/// read-only instead, at the cost of the `-shm` file SQLite then needs.
fn has_wal_rows(path: &Path) -> bool {
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    std::fs::metadata(wal).is_ok_and(|metadata| metadata.len() > 0)
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
    genre        TEXT,
    source       TEXT NOT NULL DEFAULT 'local',
    remote_id    TEXT,
    cover_id     TEXT
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

/// The sync's upsert (T12b). The conflict target is `path`: a remote row's
/// path is built from its source and remote id, so the same song re-synced
/// updates its row and never duplicates it. `album_artist`, `sample_rate`,
/// `channels` and `has_cover` have nothing to say about a remote song;
/// `mtime`/`size` are meaningless off the file system and stay 0. The
/// `cover_id` column (T12d) carries the server's `coverArt` id, the song's
/// own or its album's (T12d); a local upsert never names it, so a local row
/// keeps `NULL` whatever it was.
const UPSERT_REMOTE: &str = "
INSERT INTO tracks (path, title, artist, album, album_artist, track_no, disc_no,
                    year, duration_ms, sample_rate, channels, format, has_cover,
                    mtime, size, genre, source, remote_id, cover_id)
VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7, ?8, NULL, NULL, 'subsonic', 0, 0, 0, NULL, ?9, ?10, ?11)
ON CONFLICT(path) DO UPDATE SET
    title = excluded.title,
    artist = excluded.artist,
    album = excluded.album,
    track_no = excluded.track_no,
    disc_no = excluded.disc_no,
    year = excluded.year,
    duration_ms = excluded.duration_ms,
    source = excluded.source,
    remote_id = excluded.remote_id,
    cover_id = excluded.cover_id
";

#[expect(
    clippy::cast_sign_loss,
    reason = "COUNT(*) never returns a negative value"
)]
fn sign_to_count(value: i64) -> u64 {
    value as u64
}
