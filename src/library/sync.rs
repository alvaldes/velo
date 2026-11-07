//! Syncing a Subsonic server into the one index (T12b).
//!
//! The remote library becomes rows in the existing `tracks` table — the same
//! rows the browse queries and the fuzzy search already read — so there is
//! no second code path for browsing. What a row gains is a `source` (the
//! server that holds it, [`source_for`]) and the server's own song id as
//! `remote_id`. Playback of such a row goes through the cache: the producer
//! resolves the `subsonic://` path into a downloaded file before opening it
//! (T12c).
//!
//! Syncing is explicit (`velo --sync`), never on startup, and it is bounded:
//! the albums come from `getAlbumList2` in pages (alphabeticalByName), and
//! each album's songs from one `getAlbum` — one request per album, never one
//! per track. Everything fetched is written in one transaction at the end,
//! so a sync the server or the network breaks writes nothing: rows the
//! server dropped are removed only after a fully successful fetch.

use crate::library::{Library, LibraryError};
use crate::subsonic::client::{Client, RemoteAlbumDetail, Transport};

/// Albums per `getAlbumList2` page. The Subsonic default of 10 would make a
/// sync crawl; 500 albums per page keeps a home library at a handful of
/// list requests while staying well inside every server's limits.
pub const PAGE_SIZE: u32 = 500;

/// The listing order the sync pages through: alphabetical by album name,
/// stable and complete — every album appears on exactly one page.
const LIST_TYPE: &str = "alphabeticalByName";

/// The `source` value that identifies one server: `subsonic:<host>`.
///
/// The host is the identity because velo speaks to one server per config:
/// the host is what the user typed, it is stable across changes to the URL's
/// path (a server moved under `/navidrome/` keeps its rows) and across a
/// port that a proxy hides. One host is enough for one server; a second
/// server gets its own `source` and its own rows.
#[must_use]
pub fn source_for(base_url: &str) -> String {
    let rest = base_url.split("://").nth(1).unwrap_or(base_url);
    let authority = rest.split(['/', '?']).next().unwrap_or(rest);
    // An IPv6 authority keeps its host in brackets; the brackets are not
    // part of the name.
    let host = match authority.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(authority),
        None => authority.split(':').next().unwrap_or(authority),
    };
    format!("subsonic:{host}")
}

/// What one sync did, what `velo --sync` reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncReport {
    /// The `source` the rows were written under.
    pub source: String,
    /// Albums the server listed.
    pub albums: usize,
    /// Rows written for those albums' songs (new and refreshed together).
    pub tracks: usize,
    /// Rows of this source the server no longer holds, removed because the
    /// whole fetch succeeded. A failed sync removes nothing.
    pub removed: usize,
    /// Requests the sync cost: one per page plus one per album.
    pub requests: usize,
}

/// One song of the server's library, shaped for the `tracks` table.
struct RemoteRow {
    path: String,
    title: String,
    artist: String,
    album: String,
    track_no: Option<i64>,
    disc_no: Option<i64>,
    year: Option<i64>,
    duration_ms: i64,
    remote_id: String,
}

/// Maps one `getAlbum` answer into index rows for `source`.
///
/// A row's `path` is not a file: it is `subsonic://<host>/<song id>`, built
/// from the source so two servers that reuse a song id never collide, and
/// shaped like a URL so it can never be mistaken for a file in a folder.
/// The producer resolves such a path through the cache before opening
/// anything (T12c), so a remote row plays exactly like a local one.
fn rows_from_album(detail: &RemoteAlbumDetail, source: &str) -> Vec<RemoteRow> {
    let host = source.strip_prefix("subsonic:").unwrap_or(source);
    let album = detail.name.clone();
    let album_artist = detail.artist.clone();
    detail
        .songs
        .iter()
        .map(|song| RemoteRow {
            path: format!("subsonic://{host}/{}", song.id),
            title: song.title.clone().unwrap_or_else(|| "Untitled".to_owned()),
            artist: song
                .artist
                .clone()
                .or_else(|| album_artist.clone())
                .unwrap_or_default(),
            album: album.clone(),
            track_no: song.track,
            disc_no: song.disc_number,
            year: song.year.or(detail.year),
            duration_ms: song.duration.map_or(0, duration_ms),
            remote_id: song.id.clone(),
        })
        .collect()
}

/// The row's duration in milliseconds, rounded from the server's seconds.
fn duration_ms(seconds: f64) -> i64 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a non-negative duration rounds to a whole number of milliseconds"
    )]
    let millis = (seconds.max(0.0) * 1_000.0).round() as i64;
    millis
}

impl Library {
    /// Syncs one Subsonic server into the index under `source`
    /// ([`source_for`]).
    ///
    /// The server's albums are paged through `getAlbumList2`
    /// (alphabeticalByName, [`PAGE_SIZE`] at a time) and each album's songs
    /// fetched with one `getAlbum`. Every request happens before any write;
    /// then one transaction upserts the rows and removes this source's rows
    /// the sync did not see. A request that fails anywhere aborts before the
    /// transaction, so a failed or partial sync changes nothing.
    ///
    /// # Errors
    ///
    /// Fails with [`LibraryError::Remote`] when any request fails, or
    /// [`LibraryError::Db`] when the index cannot be written; in both cases
    /// the index is left exactly as it was.
    pub fn sync_subsonic<T: Transport>(
        &self,
        client: &Client<T>,
        source: &str,
    ) -> Result<SyncReport, LibraryError> {
        let remote = |error: crate::subsonic::Error| LibraryError::Remote(error.to_string());

        let mut rows: Vec<RemoteRow> = Vec::new();
        let mut requests = 0_usize;
        let mut albums = 0_usize;
        let mut offset = 0_u64;
        loop {
            let page = client
                .album_list2(LIST_TYPE, PAGE_SIZE, offset)
                .map_err(remote)?;
            requests += 1;
            for summary in &page {
                let detail = client.album(&summary.id).map_err(remote)?;
                requests += 1;
                rows.extend(rows_from_album(&detail, source));
            }
            albums += page.len();
            if page.len() < usize::try_from(PAGE_SIZE).unwrap_or(usize::MAX) {
                break;
            }
            offset += u64::try_from(page.len()).unwrap_or(u64::MAX);
        }

        let transaction = self.conn.unchecked_transaction()?;
        {
            let mut upsert = transaction.prepare_cached(super::UPSERT_REMOTE)?;
            for row in &rows {
                upsert.execute(rusqlite::params![
                    row.path,
                    row.title,
                    row.artist,
                    row.album,
                    row.track_no,
                    row.disc_no,
                    row.year,
                    row.duration_ms,
                    source,
                    row.remote_id,
                ])?;
            }
        }
        // The ids this sync saw, in a temp table: the removal below must
        // scale to a whole library, not to SQLite's parameter limit.
        transaction.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS sync_seen (remote_id TEXT PRIMARY KEY);
             DELETE FROM sync_seen;",
        )?;
        {
            let mut seen = transaction.prepare("INSERT INTO sync_seen (remote_id) VALUES (?1)")?;
            for row in &rows {
                seen.execute([&row.remote_id])?;
            }
        }
        let removed = transaction.execute(
            "DELETE FROM tracks
             WHERE source = ?1 AND remote_id IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM sync_seen WHERE sync_seen.remote_id = tracks.remote_id)",
            [source],
        )?;
        transaction.execute_batch("DROP TABLE IF EXISTS temp.sync_seen")?;
        transaction.commit()?;

        Ok(SyncReport {
            source: source.to_owned(),
            albums,
            tracks: rows.len(),
            removed,
            requests,
        })
    }
}
