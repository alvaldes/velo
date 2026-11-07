//! The Subsonic sync (T12b): the remote library becomes rows in the one
//! index, so browsing and fuzzy search reach it with no second code path.
//!
//! No test here touches the network: the sync goes through a canned
//! [`Transport`], and the binary-level tests speak to a `TcpListener` on
//! `127.0.0.1`. The demo server (demo.navidrome.org) is for a human's manual
//! check only — the network is not part of the suite.

use std::fs;
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::thread;

use rusqlite::Connection;
use velo::library::{Library, SyncReport, source_for};
use velo::subsonic::Error;
use velo::subsonic::client::{Auth, Client, Transport};
use velo::ui::crossterm::event::{KeyCode, KeyEvent};
use velo::ui::{Action, App, BrowseBy};

// ------------------------------------------------------------- infrastructure

/// A transport that answers from a table of canned bodies: the first entry
/// whose marker is contained in the URL wins.
struct Canned {
    responses: Vec<(String, Result<Vec<u8>, Error>)>,
}

impl Canned {
    fn new(responses: Vec<(&str, Result<Vec<u8>, Error>)>) -> Self {
        Self {
            responses: responses
                .into_iter()
                .map(|(marker, body)| (marker.to_owned(), body))
                .collect(),
        }
    }
}

impl Transport for Canned {
    fn get(&self, url: &str) -> Result<Vec<u8>, Error> {
        for (marker, body) in &self.responses {
            if url.contains(marker.as_str()) {
                return body.clone();
            }
        }
        Err(Error::Unreachable {
            detail: format!("no canned response for {url}"),
        })
    }
}

fn ok_envelope(payload: &str) -> Vec<u8> {
    format!(r#"{{"subsonic-response":{{"status":"ok"{payload}}}}}"#).into_bytes()
}

fn error_envelope(code: u32, message: &str) -> Vec<u8> {
    format!(
        r#"{{"subsonic-response":{{"status":"failed","error":{{"code":{code},"message":"{message}"}}}}}}"#
    )
    .into_bytes()
}

/// A `getAlbumList2` page: albums as `(id, name, artist)`.
fn album_list_body(albums: &[(&str, &str, &str)]) -> Vec<u8> {
    let albums: Vec<String> = albums
        .iter()
        .map(|(id, name, artist)| {
            format!(r#"{{"id":"{id}","name":"{name}","artist":"{artist}","year":2021}}"#)
        })
        .collect();
    ok_envelope(&format!(
        r#","albumList2":{{"album":[{}]}}"#,
        albums.join(",")
    ))
}

/// A `getAlbum` answer: songs as `(id, title, artist, duration, track)`.
fn album_body(id: &str, name: &str, artist: &str, songs: &[(&str, &str, f64, i64)]) -> Vec<u8> {
    let songs: Vec<String> = songs
        .iter()
        .map(|(song_id, title, duration, track)| {
            format!(
                r#"{{"id":"{song_id}","title":"{title}","artist":"{artist}","album":"{name}","duration":{duration},"track":{track},"discNumber":1,"year":2021}}"#
            )
        })
        .collect();
    ok_envelope(&format!(
        r#","album":{{"id":"{id}","name":"{name}","artist":"{artist}","year":2021,"song":[{}]}}"#,
        songs.join(",")
    ))
}

/// The sample library the canned transports serve: two albums, three songs.
/// The `getAlbum` markers are distinct per album id, and `getAlbum?` is no
/// substring of `getAlbumList2`, so every request finds its own body.
fn sample_library_transport() -> Canned {
    Canned::new(vec![
        (
            "id=al-1",
            Ok(album_body(
                "al-1",
                "Harbor",
                "Tide",
                &[("s-1", "Dawn", 213.4, 1), ("s-2", "Tide", 100.0, 2)],
            )),
        ),
        (
            "id=al-2",
            Ok(album_body(
                "al-2",
                "Views",
                "Baltazar",
                &[("s-3", "Rising", 45.5, 1)],
            )),
        ),
        (
            "getAlbumList2",
            Ok(album_list_body(&[
                ("al-1", "Harbor", "Tide"),
                ("al-2", "Views", "Baltazar"),
            ])),
        ),
    ])
}

fn client<T: Transport>(transport: T) -> Client<T> {
    Client::new(
        transport,
        "http://music.example.com",
        "admin",
        Auth::ApiKey {
            key: "secret".to_owned(),
        },
    )
}

fn sync<T: Transport>(library: &Library, transport: T) -> SyncReport {
    library
        .sync_subsonic(&client(transport), &source_for("http://music.example.com"))
        .unwrap_or_else(|error| panic!("sync failed: {error}"))
}

/// A temp directory with an index inside it, removed when dropped.
struct TempIndex {
    dir: PathBuf,
    library: Library,
}

impl TempIndex {
    fn new(name: &str) -> Self {
        let dir = Path::new(&std::env::temp_dir())
            .join(format!("velo-sync-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let library = Library::open_at(&dir.join("index.db")).unwrap();
        Self { dir, library }
    }

    /// Writes a small valid PCM WAV file (0.1 s, 44.1 kHz, stereo).
    fn write_wav(&self, relative: &str) -> PathBuf {
        let path = self.dir.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();

        let sample_rate: u32 = 44_100;
        let channels: u16 = 2;
        let frames: usize = 4_410;
        let data_len: u32 = u32::try_from(frames * usize::from(channels) * 2).unwrap();

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes()); // PCM
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&(sample_rate * u32::from(channels) * 2).to_le_bytes());
        bytes.extend_from_slice(&16_u16.to_le_bytes()); // bits per sample
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        bytes.extend(
            std::iter::repeat_n(0_i16, frames * usize::from(channels)).flat_map(i16::to_le_bytes),
        );

        fs::write(&path, bytes).unwrap();
        path
    }
}

impl Drop for TempIndex {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Counts the rows of the tracks table, any source.
fn row_count(library: &Library) -> usize {
    usize::try_from(library.counts().unwrap().tracks).unwrap()
}

// ------------------------------------------------------------------ migration

/// The `tracks` table as velo 0.6.0 (T12a and before) created it: a `genre`
/// column, but no `source` and no `remote_id`.
const OLD_SCHEMA: &str = "
CREATE TABLE tracks (
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
CREATE INDEX idx_tracks_artist_album ON tracks(artist, album);
CREATE INDEX idx_tracks_album ON tracks(album);
";

#[test]
fn an_old_schema_index_migrates_keeps_every_row_and_opens_twice() {
    let dir = temp_dir("migrate");
    let database = dir.join("index.db");
    {
        let old = Connection::open(&database).unwrap();
        old.execute_batch(OLD_SCHEMA).unwrap();
        old.pragma_update(None, "user_version", 1).unwrap();
        for (path, title) in [
            ("/music/Tide/01 - Dawn.wav", "Dawn"),
            ("/music/Baltazar/01 - Rising.wav", "Rising"),
        ] {
            old.execute(
                "INSERT INTO tracks (path, title, artist, album, format, mtime, size)
                 VALUES (?1, ?2, 'Someone', 'Album', 'wav', 1, 10)",
                rusqlite::params![path, title],
            )
            .unwrap();
        }
    }

    // Opening an old file with the new code keeps every row, as a local row.
    let tracks = {
        let library = Library::open_at(&database).unwrap();
        let tracks = library.all_tracks().unwrap();
        assert_eq!(tracks.len(), 2, "every old row survives the migration");
        assert!(
            tracks
                .iter()
                .all(|track| track.source == "local" && track.remote_id.is_none()),
            "{tracks:?}"
        );
        tracks
    };

    // A second open neither fails nor duplicates.
    let reopened = Library::open_at(&database).unwrap();
    assert_eq!(reopened.all_tracks().unwrap().len(), tracks.len());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn one_server_may_not_hold_two_rows_with_the_same_remote_id() {
    let dir = temp_dir("unique");
    let database = dir.join("index.db");
    {
        // The library creates the schema; the raw connection then writes
        // rows that only a sync would write.
        drop(Library::open_at(&database).unwrap());
        let conn = Connection::open(&database).unwrap();
        let row = |source: &str, path: &str, remote_id: &str| {
            conn.execute(
                "INSERT INTO tracks (path, title, artist, album, format, mtime, size, source, remote_id)
                 VALUES (?1, 'T', 'A', 'Al', 'subsonic', 0, 0, ?2, ?3)",
                rusqlite::params![path, source, remote_id],
            )
        };
        row(
            "subsonic:music.example.com",
            "subsonic://music.example.com/s-1",
            "s-1",
        )
        .unwrap();

        // The same server, the same remote id: the constraint refuses it.
        let duplicate = row(
            "subsonic:music.example.com",
            "subsonic://music.example.com/s-1-again",
            "s-1",
        );
        assert!(duplicate.is_err(), "the unique constraint must hold");

        // Another server with the same song id is a different row.
        row(
            "subsonic:elsewhere.example.com",
            "subsonic://elsewhere.example.com/s-1",
            "s-1",
        )
        .unwrap();
    }
    let _ = fs::remove_dir_all(&dir);
}

// --------------------------------------------------------------------- source

#[test]
fn the_source_names_the_host_of_the_server() {
    assert_eq!(
        source_for("https://demo.navidrome.org"),
        "subsonic:demo.navidrome.org"
    );
    // A port is part of the address, not of the identity.
    assert_eq!(source_for("http://127.0.0.1:4533"), "subsonic:127.0.0.1");
    // A trailing slash or a sub-path changes neither.
    assert_eq!(
        source_for("https://music.example.com/navidrome/"),
        "subsonic:music.example.com"
    );
}

// --------------------------------------------------------------------- sync

#[test]
fn a_canned_server_produces_rows_with_every_field_filled() {
    let dir = TempIndex::new("mapping");

    let report = sync(&dir.library, sample_library_transport());

    assert_eq!(report.source, "subsonic:music.example.com");
    assert_eq!(report.albums, 2);
    assert_eq!(report.tracks, 3);
    assert_eq!(report.removed, 0);
    // One page request plus one request per album — never one per track.
    assert_eq!(report.requests, 3);

    let rows: Vec<velo::library::TrackEntry> = dir.library.all_tracks().unwrap();
    let row = |source: &str,
               artist: &str,
               album: &str,
               title: &str,
               duration_ms: i64,
               track_no: i64,
               remote_id: &str| {
        velo::library::TrackEntry {
            artist: artist.to_owned(),
            album: album.to_owned(),
            title: title.to_owned(),
            track_no: Some(track_no),
            disc_no: Some(1),
            duration_ms,
            path: PathBuf::from(format!("subsonic://music.example.com/{remote_id}")),
            source: source.to_owned(),
            remote_id: Some(remote_id.to_owned()),
        }
    };
    assert_eq!(
        rows,
        [
            row(
                "subsonic:music.example.com",
                "Baltazar",
                "Views",
                "Rising",
                45_500,
                1,
                "s-3"
            ),
            row(
                "subsonic:music.example.com",
                "Tide",
                "Harbor",
                "Dawn",
                213_400,
                1,
                "s-1"
            ),
            row(
                "subsonic:music.example.com",
                "Tide",
                "Harbor",
                "Tide",
                100_000,
                2,
                "s-2"
            ),
        ]
    );
}

#[test]
fn syncing_twice_does_not_duplicate_rows() {
    let dir = TempIndex::new("idempotent");

    sync(&dir.library, sample_library_transport());
    sync(&dir.library, sample_library_transport());

    assert_eq!(row_count(&dir.library), 3);
    let sources = dir.library.counts_by_source().unwrap();
    assert_eq!(
        sources,
        vec![
            ("local".to_owned(), 0),
            ("subsonic:music.example.com".to_owned(), 3),
        ]
    );
}

#[test]
fn a_row_the_server_dropped_is_removed_by_the_next_successful_sync() {
    let dir = TempIndex::new("dropped");
    sync(&dir.library, sample_library_transport());
    assert_eq!(row_count(&dir.library), 3);

    // The server now holds only one of the two albums.
    let shrunken = Canned::new(vec![
        (
            "id=al-1",
            Ok(album_body(
                "al-1",
                "Harbor",
                "Tide",
                &[("s-1", "Dawn", 213.4, 1)],
            )),
        ),
        (
            "getAlbumList2",
            Ok(album_list_body(&[("al-1", "Harbor", "Tide")])),
        ),
    ]);
    let report = sync(&dir.library, shrunken);

    assert_eq!(report.removed, 2, "the dropped songs are counted");
    assert_eq!(row_count(&dir.library), 1);
    let remaining: Vec<String> = dir
        .library
        .all_tracks()
        .unwrap()
        .into_iter()
        .map(|track| track.remote_id.unwrap())
        .collect();
    assert_eq!(remaining, ["s-1"]);
}

#[test]
fn a_failed_sync_removes_nothing() {
    let dir = TempIndex::new("failed");
    sync(&dir.library, sample_library_transport());
    assert_eq!(row_count(&dir.library), 3);

    // The album list answers, but the second album request fails mid-sync.
    let broken = Canned::new(vec![
        (
            "id=al-1",
            Ok(album_body(
                "al-1",
                "Harbor",
                "Tide",
                &[("s-1", "Dawn", 213.4, 1)],
            )),
        ),
        (
            "id=al-2",
            Err(Error::Server {
                code: 0,
                message: "the server fell over".to_owned(),
            }),
        ),
        (
            "getAlbumList2",
            Ok(album_list_body(&[
                ("al-1", "Harbor", "Tide"),
                ("al-2", "Views", "Baltazar"),
            ])),
        ),
    ]);

    let error = dir
        .library
        .sync_subsonic(&client(broken), &source_for("http://music.example.com"))
        .expect_err("a sync the server broke must fail");

    assert!(error.to_string().contains("fell over"), "{error}");
    // Nothing was removed and nothing was half-written.
    assert_eq!(row_count(&dir.library), 3);
    let ids: Vec<String> = dir
        .library
        .all_tracks()
        .unwrap()
        .into_iter()
        .filter_map(|track| track.remote_id)
        .collect();
    assert_eq!(ids.len(), 3, "{ids:?}");
}

// ------------------------------------------------- one index, both sources

#[test]
fn a_local_reindex_keeps_the_remote_rows() {
    let dir = TempIndex::new("both");
    dir.write_wav("Local Artist/2020 - Home/01 - Here.wav");
    dir.library.index(&dir.dir).unwrap();
    sync(&dir.library, sample_library_transport());
    assert_eq!(row_count(&dir.library), 4);

    // The incremental local scan knows nothing of the remote rows and must
    // leave them alone.
    let report = dir.library.index(&dir.dir).unwrap();
    assert_eq!(report.removed, 0);
    assert_eq!(row_count(&dir.library), 4);

    let sources = dir.library.counts_by_source().unwrap();
    assert_eq!(
        sources,
        vec![
            ("local".to_owned(), 1),
            ("subsonic:music.example.com".to_owned(), 3),
        ]
    );
}

#[test]
fn the_existing_queries_see_remote_rows_and_local_rows_are_unchanged() {
    let dir = TempIndex::new("queries");
    let local = dir.write_wav("Local Artist/2020 - Home/01 - Here.wav");
    dir.library.index(&dir.dir).unwrap();
    sync(&dir.library, sample_library_transport());

    // The search reads through all_tracks; remote rows are in it.
    let all = dir.library.all_tracks().unwrap();
    assert_eq!(all.len(), 4);
    assert_eq!(
        all[0].source, "subsonic:music.example.com",
        "Baltazar sorts first"
    );
    let local_row = all.iter().find(|track| track.path == local).unwrap();
    assert_eq!(local_row.source, "local");
    assert_eq!(local_row.remote_id, None);
    assert!(!local_row.is_remote());

    // The artist browser and the album browser reach the remote rows too.
    let artists: Vec<String> = dir
        .library
        .artists()
        .unwrap()
        .into_iter()
        .map(|artist| artist.name)
        .collect();
    assert_eq!(artists, ["Baltazar", "Local Artist", "Tide"]);
    let harbor: Vec<String> = dir
        .library
        .tracks_of("Tide", "Harbor")
        .unwrap()
        .into_iter()
        .map(|track| track.title)
        .collect();
    assert_eq!(harbor, ["Dawn", "Tide"]);
    let remote = dir.library.tracks_of("Tide", "Harbor").unwrap()[0].clone();
    assert!(remote.is_remote());
}

// ------------------------------------------------------------------ interface

/// The fake cache for the interface tests: every synced song id plays the
/// same small fixture, as if the download had already filled the cache.
fn serve_fixture(dir: &Path) -> Arc<dyn velo::audio::output::PathResolver> {
    let fixture = dir.join("fixture.wav");
    fs::write(&fixture, fixture_wav_bytes()).unwrap();
    Arc::new(serve_fixture::Fixture { fixture })
}

fn fixture_wav_bytes() -> Vec<u8> {
    let sample_rate: u32 = 44_100;
    let channels: u16 = 2;
    let frames: usize = 4_410;
    let data_len: u32 = u32::try_from(frames * usize::from(channels) * 2).unwrap();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate * u32::from(channels) * 2).to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    bytes.extend(
        std::iter::repeat_n(0_i16, frames * usize::from(channels)).flat_map(i16::to_le_bytes),
    );
    bytes
}

mod serve_fixture {
    use std::path::{Path, PathBuf};

    use velo::audio::AudioError;
    use velo::audio::output::PathResolver;

    /// Serves one fixture file for every `subsonic://` path; a local path
    /// passes through untouched, exactly like the real resolver.
    pub(super) struct Fixture {
        pub(super) fixture: PathBuf,
    }

    impl PathResolver for Fixture {
        fn resolve(&self, path: &Path) -> Result<PathBuf, AudioError> {
            match path
                .to_str()
                .and_then(|text| text.strip_prefix("subsonic://"))
            {
                Some(_) => Ok(self.fixture.clone()),
                None => Ok(path.to_path_buf()),
            }
        }
    }
}

#[test]
fn enter_on_a_remote_row_plays_through_the_cache_instead_of_refusing() {
    let dir = TempIndex::new("enter");
    sync(&dir.library, sample_library_transport());

    let mut app = App::browse(Some(&dir.library));
    app.set_resolver(Some(serve_fixture(&dir.dir)));
    app.apply(Action::Browse(BrowseBy::Tracks));
    let remote = app
        .tracks()
        .iter()
        .position(velo::library::TrackEntry::is_remote)
        .expect("the all-tracks view holds remote rows");
    while app.selected() < remote {
        app.on_key(KeyEvent::from(KeyCode::Char('j')));
    }

    let action = app.on_key(KeyEvent::from(KeyCode::Enter));
    app.apply(action);

    // The stream starts through the cache: where a device exists the queue
    // now holds the remote rows and the fixture plays; where it does not, the
    // attempt is reported. Either way the T12c refusal is gone.
    assert_ne!(
        app.status(),
        "velo: remote playback arrives with the cache (T12c)",
        "the refusal must be gone"
    );
}

#[test]
fn enter_on_a_remote_search_result_plays_through_the_cache_too() {
    let dir = TempIndex::new("search-enter");
    sync(&dir.library, sample_library_transport());

    let mut app = App::browse(Some(&dir.library));
    app.set_resolver(Some(serve_fixture(&dir.dir)));
    app.on_key(KeyEvent::from(KeyCode::Char('/'))); // opens the search
    for character in "Rising".chars() {
        app.on_key(KeyEvent::from(KeyCode::Char(character)));
    }
    let action = app.on_key(KeyEvent::from(KeyCode::Enter));
    app.apply(action);

    assert_ne!(
        app.status(),
        "velo: remote playback arrives with the cache (T12c)",
        "the refusal must be gone"
    );
}

#[test]
fn slash_reaches_the_search_when_only_a_server_is_configured() {
    // A remote-only user has no `library` root; the synced index is the
    // library, so `/` must search it instead of the path prompt blocking.
    let dir = TempIndex::new("slash");
    sync(&dir.library, sample_library_transport());

    let mut app = App::browse(Some(&dir.library));
    app.launch_without_library(true);
    assert!(!app.library_prompt_open(), "no folder to ask for");

    app.on_key(KeyEvent::from(KeyCode::Char('/')));
    assert!(app.search_open(), "/ must reach the search");
    for character in "Rising".chars() {
        app.on_key(KeyEvent::from(KeyCode::Char(character)));
    }
    assert!(!app.search_matches().is_empty(), "the remote row is found");
}

// --------------------------------------------------------------- the command

/// Serves the given bodies in order to sequential HTTP requests from a local
/// listener, one body per connection.
fn serve_bodies(listener: &TcpListener, bodies: &[Vec<u8>]) {
    for body in bodies {
        let (mut stream, _) = listener
            .accept()
            .unwrap_or_else(|error| panic!("no connection: {error}"));
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request);
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).unwrap();
        stream.write_all(body).unwrap();
    }
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = Path::new(&std::env::temp_dir())
        .join(format!("velo-sync-cmd-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("velo")).unwrap();
    dir
}

#[test]
fn the_sync_command_prints_counts_per_source_and_exits_zero() {
    let dir = temp_dir("ok");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    fs::write(
        dir.join("velo").join("config.toml"),
        format!("[subsonic]\nurl = \"http://127.0.0.1:{port}\"\nuser = \"admin\"\napi_key = \"sesame\"\n"),
    )
    .unwrap();

    let bodies = vec![
        ok_envelope(""),
        album_list_body(&[("al-1", "Harbor", "Tide")]),
        album_body(
            "al-1",
            "Harbor",
            "Tide",
            &[("s-1", "Dawn", 10.0, 1), ("s-2", "Tide", 20.0, 2)],
        ),
    ];
    let server = thread::spawn(move || serve_bodies(&listener, &bodies));

    let output = Command::new(env!("CARGO_BIN_EXE_velo"))
        .arg("--sync")
        .env("XDG_CONFIG_HOME", dir.as_os_str())
        .env("VELO_DB", dir.join("index.db").as_os_str())
        .output()
        .unwrap();
    server.join().unwrap();

    assert!(
        output.status.success(),
        "exit {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("synced subsonic:127.0.0.1: 2 tracks"),
        "{stdout}"
    );
    assert!(stdout.contains("3 requests"), "{stdout}");
    assert!(
        stdout.contains("local: 0 tracks · subsonic:127.0.0.1: 2 tracks"),
        "{stdout}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_sync_command_fails_with_a_reason_when_the_server_fails() {
    let dir = temp_dir("failure");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    fs::write(
        dir.join("velo").join("config.toml"),
        format!(
            "[subsonic]\nurl = \"http://127.0.0.1:{port}\"\nuser = \"admin\"\napi_key = \"wrong\"\n"
        ),
    )
    .unwrap();

    let bodies = vec![ok_envelope(""), error_envelope(40, "Wrong credentials")];
    let server = thread::spawn(move || serve_bodies(&listener, &bodies));

    let output = Command::new(env!("CARGO_BIN_EXE_velo"))
        .arg("--sync")
        .env("XDG_CONFIG_HOME", dir.as_os_str())
        .env("VELO_DB", dir.join("index.db").as_os_str())
        .output()
        .unwrap();
    server.join().unwrap();

    assert_eq!(output.status.code(), Some(1), "exit code must be 1");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("sync failed") && stderr.contains("Wrong credentials"),
        "{stderr}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_sync_command_without_a_subsonic_table_fails_with_a_reason() {
    let dir = temp_dir("no-table");
    fs::write(dir.join("velo").join("config.toml"), "volume = 40\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_velo"))
        .arg("--sync")
        .env("XDG_CONFIG_HOME", dir.as_os_str())
        .env("VELO_DB", dir.join("index.db").as_os_str())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1), "exit code must be 1");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("subsonic"), "{stderr}");
    let _ = fs::remove_dir_all(&dir);
}
