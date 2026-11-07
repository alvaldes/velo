//! The library index, one test per S6 rule: extension filter, path-layout
//! fallback, idempotent re-indexing, deletions, and the browse queries.
//!
//! Every test builds its own tree in a temp directory and points the index
//! at a database inside it, so the user's real index is never touched (the
//! same guarantee `VELO_DB` gives the binary — the in-crate tests use
//! `Library::open_at` directly because `std::env::set_var` is `unsafe` in
//! edition 2024 and these tests run in parallel in one process).

use std::fs;
use std::path::{Path, PathBuf};

use lofty::config::WriteOptions;
use lofty::prelude::*;
use lofty::tag::{Tag, TagType};
use velo::library::{Counts, Library};

// helpers ---------------------------------------------------------------

/// A temp directory that removes itself, plus the index inside it.
struct TempLibrary {
    root: PathBuf,
    library: Library,
}

impl TempLibrary {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("velo-library-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let library = Library::open_at(&root.join("index.db")).unwrap();
        Self { root, library }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    /// Writes a small valid PCM WAV file (0.1 s, 44.1 kHz, stereo).
    fn write_wav(&self, relative: &str) -> PathBuf {
        let path = self.path(relative);
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
        bytes.extend_from_slice(&(channels * 2).to_le_bytes()); // block align
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

/// Adds real `ID3v2` tags to a file already on disk.
fn tag_file(path: &Path, set: impl FnOnce(&mut Tag)) {
    let mut tagged = lofty::read_from_path(path).unwrap();
    tagged.insert_tag(Tag::new(TagType::Id3v2));
    let tag = tagged.tag_mut(TagType::Id3v2).unwrap();
    set(tag);
    // Read+write: the WAV saver reads the existing chunks from the same
    // handle while rewriting it.
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    tagged.save_to(&mut file, WriteOptions::default()).unwrap();
}

impl TempLibrary {
    fn count_tracks(&self) -> usize {
        self.library
            .counts()
            .unwrap()
            .tracks
            .try_into()
            .expect("test counts are small")
    }
}

impl Drop for TempLibrary {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

// rules -----------------------------------------------------------------

#[test]
fn only_audio_extensions_become_rows() {
    let temp = TempLibrary::new("extensions");
    temp.write_wav("Tone/2020 - Waves/01 - A.wav");
    // Valid audio content under an mp3 name is still a track.
    temp.write_wav("Tone/2020 - Waves/02 - B.mp3");
    // Non-audio files of every kind stay out.
    fs::write(temp.path("Tone/2020 - Waves/notes.txt"), "not a track").unwrap();
    fs::write(temp.path("Tone/2020 - Waves/cover.jpg"), "not a track").unwrap();

    temp.library.index(&temp.root).unwrap();

    assert_eq!(temp.count_tracks(), 2);
    let mut names: Vec<String> = Vec::new();
    for album in temp.library.albums_of("Tone").unwrap() {
        for track in temp.library.tracks_of("Tone", &album.title).unwrap() {
            names.push(
                track
                    .path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    names.sort();
    assert_eq!(names, ["01 - A.wav", "02 - B.mp3"]);
}

#[test]
fn an_untagged_file_gets_metadata_from_the_path_layout() {
    let temp = TempLibrary::new("layout");
    temp.write_wav("Neon Fields/2019 - Night Drive/03 - Chrome Road.wav");
    // An album folder without the `YYYY - ` prefix is still an album.
    temp.write_wav("Neon Fields/Free Jazz Collection/02 - Pick.wav");

    temp.library.index(&temp.root).unwrap();

    let albums: Vec<(String, Option<i64>)> = temp
        .library
        .albums_of("Neon Fields")
        .unwrap()
        .into_iter()
        .map(|a| (a.title, a.year))
        .collect();
    assert_eq!(
        albums,
        [
            ("Free Jazz Collection".to_owned(), None),
            ("Night Drive".to_owned(), Some(2019)),
        ]
    );

    let tracks = temp
        .library
        .tracks_of("Neon Fields", "Free Jazz Collection")
        .unwrap();
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].title, "Pick");
    assert_eq!(tracks[0].track_no, Some(2));

    let tracks = temp
        .library
        .tracks_of("Neon Fields", "Night Drive")
        .unwrap();
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].title, "Chrome Road");
    assert_eq!(tracks[0].track_no, Some(3));
}

#[test]
fn indexing_twice_does_not_duplicate_rows() {
    let temp = TempLibrary::new("idempotent");
    temp.write_wav("Tone/2020 - Waves/01 - A.wav");
    temp.write_wav("Tone/2020 - Waves/02 - B.wav");

    let first = temp.library.index(&temp.root).unwrap();
    assert_eq!(first.added, 2);

    let second = temp.library.index(&temp.root).unwrap();
    assert_eq!(second.unchanged, 2);
    assert_eq!(second.added, 0);
    assert_eq!(second.removed, 0);
    assert_eq!(temp.count_tracks(), 2);
}

#[test]
fn a_deleted_file_disappears_from_the_index() {
    let temp = TempLibrary::new("deleted");
    let kept = temp.write_wav("Tone/2020 - Waves/01 - A.wav");
    let gone = temp.write_wav("Tone/2020 - Waves/02 - B.wav");
    temp.library.index(&temp.root).unwrap();
    assert_eq!(temp.count_tracks(), 2);

    fs::remove_file(gone).unwrap();
    let report = temp.library.index(&temp.root).unwrap();

    assert_eq!(report.removed, 1);
    assert_eq!(temp.count_tracks(), 1);
    let remaining: Vec<PathBuf> = temp
        .library
        .tracks_of("Tone", "Waves")
        .unwrap()
        .into_iter()
        .map(|track| track.path)
        .collect();
    assert_eq!(remaining, [kept]);
}

#[expect(
    clippy::too_many_lines,
    reason = "one flat, readable fixture setup per browse query"
)]
#[test]
fn queries_group_and_order_correctly() {
    let temp = TempLibrary::new("queries");

    // Artist "Aerial Bay", two albums; insertion order is deliberately
    // shuffled so the queries, not the walk, define the order.
    let lantern = temp.write_wav("Aerial Bay/2003 - Lantern/01 - Far Light.wav");
    tag_file(&lantern, |tag| {
        tag.set_title("Far Light".to_owned());
        tag.set_artist("Aerial Bay".to_owned());
        tag.set_album("Lantern".to_owned());
        tag.set_track(1);
        tag.set_disk(2);
        tag.insert_text(ItemKey::Year, "2003".to_owned());
    });
    let tide = temp.write_wav("Aerial Bay/2001 - Harbor/02 - Tide.wav");
    tag_file(&tide, |tag| {
        tag.set_title("Tide".to_owned());
        tag.set_artist("Aerial Bay".to_owned());
        tag.set_album("Harbor".to_owned());
        tag.set_track(2);
        tag.set_disk(1);
        tag.insert_text(ItemKey::Year, "2001".to_owned());
    });
    let dawn = temp.write_wav("Aerial Bay/2001 - Harbor/01 - Dawn.wav");
    tag_file(&dawn, |tag| {
        tag.set_title("Dawn".to_owned());
        tag.set_artist("Aerial Bay".to_owned());
        tag.set_album("Harbor".to_owned());
        tag.set_track(1);
        tag.set_disk(2);
        tag.insert_text(ItemKey::Year, "2001".to_owned());
    });
    let anchor = temp.write_wav("Aerial Bay/2001 - Harbor/01 - Anchor.wav");
    tag_file(&anchor, |tag| {
        tag.set_title("Anchor".to_owned());
        tag.set_artist("Aerial Bay".to_owned());
        tag.set_album("Harbor".to_owned());
        tag.set_track(1);
        tag.set_disk(1);
        tag.insert_text(ItemKey::Year, "2001".to_owned());
    });

    // A second artist, one album, tracks written in reverse order.
    let rolling = temp.write_wav("Boulder Set/2010 - Stone/02 - Rolling.wav");
    tag_file(&rolling, |tag| {
        tag.set_title("Rolling".to_owned());
        tag.set_artist("Boulder Set".to_owned());
        tag.set_album("Stone".to_owned());
        tag.set_track(2);
        tag.insert_text(ItemKey::Year, "2010".to_owned());
    });
    let still = temp.write_wav("Boulder Set/2010 - Stone/01 - Still.wav");
    tag_file(&still, |tag| {
        tag.set_title("Still".to_owned());
        tag.set_artist("Boulder Set".to_owned());
        tag.set_album("Stone".to_owned());
        tag.set_track(1);
        tag.insert_text(ItemKey::Year, "2010".to_owned());
    });

    temp.library.index(&temp.root).unwrap();

    assert_eq!(
        temp.library.counts().unwrap(),
        Counts {
            tracks: 6,
            albums: 3,
            artists: 2
        }
    );

    let artists: Vec<(String, u64, u64)> = temp
        .library
        .artists()
        .unwrap()
        .into_iter()
        .map(|a| (a.name, a.tracks, a.albums))
        .collect();
    assert_eq!(
        artists,
        [
            ("Aerial Bay".to_owned(), 4, 2),
            ("Boulder Set".to_owned(), 2, 1),
        ]
    );

    // Albums of one artist: ordered by year, and only that artist's.
    let albums: Vec<String> = temp
        .library
        .albums_of("Aerial Bay")
        .unwrap()
        .into_iter()
        .map(|a| a.title)
        .collect();
    assert_eq!(albums, ["Harbor", "Lantern"]);

    // Tracks of one album: disc then track number, regardless of insert order.
    let harbor: Vec<(i64, i64, String)> = temp
        .library
        .tracks_of("Aerial Bay", "Harbor")
        .unwrap()
        .into_iter()
        .map(|t| (t.disc_no.unwrap(), t.track_no.unwrap(), t.title))
        .collect();
    assert_eq!(
        harbor,
        [
            (1, 1, "Anchor".to_owned()),
            (1, 2, "Tide".to_owned()),
            (2, 1, "Dawn".to_owned()),
        ]
    );

    let stone: Vec<String> = temp
        .library
        .tracks_of("Boulder Set", "Stone")
        .unwrap()
        .into_iter()
        .map(|t| t.title)
        .collect();
    assert_eq!(stone, ["Still", "Rolling"]);
}

#[test]
fn all_tracks_lists_every_indexed_track() {
    let temp = TempLibrary::new("all-tracks");
    temp.write_wav("Tide/2019 - Harbor/01 - Dawn.wav");
    temp.write_wav("Tide/2019 - Harbor/02 - Tide.wav");
    temp.write_wav("Baltazar/2001 - Views/01 - Rising.wav");
    temp.library.index(&temp.root).unwrap();

    let tracks = temp.library.all_tracks().unwrap();
    assert_eq!(tracks.len(), temp.count_tracks());

    // The search builds its candidate rows from artist, album and title, so
    // those have to travel with every row.
    let rows: Vec<(String, String, String)> = tracks
        .into_iter()
        .map(|track| (track.artist, track.album, track.title))
        .collect();
    assert_eq!(
        rows,
        [
            (
                "Baltazar".to_owned(),
                "Views".to_owned(),
                "Rising".to_owned()
            ),
            ("Tide".to_owned(), "Harbor".to_owned(), "Dawn".to_owned()),
            ("Tide".to_owned(), "Harbor".to_owned(), "Tide".to_owned()),
        ]
    );
}

// genres and the whole-library views (S3) ---------------------------------

/// Writes a WAV tagged with an artist, an album, a title and, when given, a
/// genre string exactly as a tagger would store it.
fn tagged_wav(temp: &TempLibrary, artist: &str, album: &str, title: &str, genre: Option<&str>) {
    let path = temp.write_wav(&format!("{artist}/{album}/{title}.wav"));
    tag_file(&path, |tag| {
        tag.set_artist(artist.to_owned());
        tag.set_album(album.to_owned());
        tag.set_title(title.to_owned());
        if let Some(genre) = genre {
            tag.set_genre(genre.to_owned());
        }
    });
}

#[test]
fn a_multi_valued_genre_puts_the_track_in_each_genre_and_untagged_tracks_go_last() {
    let temp = TempLibrary::new("genres");
    tagged_wav(
        &temp,
        "Aerial",
        "Haze",
        "Drift",
        Some("Dub / Trip-Hop / Lo-Fi"),
    );
    tagged_wav(&temp, "Aerial", "Haze", "Swing", Some("jazz"));
    tagged_wav(
        &temp,
        "Boulder",
        "Stone",
        "Loop",
        Some("Lo-Fi; Jazz,ambient"),
    );
    tagged_wav(&temp, "Boulder", "Stone", "Bare", None);
    tagged_wav(&temp, "Boulder", "Stone", "Twice", Some("Jazz;jazz"));
    temp.library.index(&temp.root).unwrap();

    let genres: Vec<(String, u64)> = temp
        .library
        .genres()
        .unwrap()
        .into_iter()
        .map(|genre| (genre.name, genre.tracks))
        .collect();
    // Case-insensitive order and grouping; the first spelling met names the
    // genre; a track counts once per genre; `(no genre)` comes last.
    assert_eq!(
        genres,
        [
            ("ambient".to_owned(), 1),
            ("Dub".to_owned(), 1),
            ("jazz".to_owned(), 3),
            ("Lo-Fi".to_owned(), 2),
            ("Trip-Hop".to_owned(), 1),
            ("(no genre)".to_owned(), 1),
        ]
    );

    let titles = |genre: &str| -> Vec<String> {
        temp.library
            .tracks_of_genre(genre)
            .unwrap()
            .into_iter()
            .map(|track| track.title)
            .collect()
    };
    assert_eq!(titles("Lo-Fi"), ["Drift", "Loop"]);
    assert_eq!(titles("JAZZ"), ["Swing", "Loop", "Twice"]);
    assert_eq!(titles("(no genre)"), ["Bare"]);
    assert_eq!(titles("Polka"), [] as [String; 0]);
}

#[test]
fn every_album_is_listed_once_with_its_artist_year_and_track_count() {
    let temp = TempLibrary::new("all-albums");
    temp.write_wav("Tide/2019 - Harbor/01 - Dawn.wav");
    temp.write_wav("Tide/2019 - Harbor/02 - Tide.wav");
    temp.write_wav("Baltazar/2001 - Views/01 - Rising.wav");
    temp.write_wav("Aerial/Anthems/01 - Up.wav");
    temp.library.index(&temp.root).unwrap();

    let albums: Vec<(String, String, Option<i64>, u64)> = temp
        .library
        .all_albums()
        .unwrap()
        .into_iter()
        .map(|album| (album.title, album.artist, album.year, album.tracks))
        .collect();
    assert_eq!(
        albums,
        [
            ("Anthems".to_owned(), "Aerial".to_owned(), None, 1),
            ("Harbor".to_owned(), "Tide".to_owned(), Some(2019), 2),
            ("Views".to_owned(), "Baltazar".to_owned(), Some(2001), 1),
        ]
    );
    // The artist's own albums carry the artist too.
    assert_eq!(temp.library.albums_of("Tide").unwrap()[0].artist, "Tide");
}

/// The `tracks` table as velo 0.5.0 created it: no genre column, no schema
/// version.
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
    size         INTEGER NOT NULL
);
CREATE INDEX idx_tracks_artist_album ON tracks(artist, album);
CREATE INDEX idx_tracks_album ON tracks(album);
";

#[test]
fn an_index_built_without_genres_keeps_its_rows_and_the_next_scan_fills_them_in() {
    let root = std::env::temp_dir().join(format!("velo-library-migrate-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let database = root.join("index.db");

    // A tagged file, indexed by the old schema with its current mtime and
    // size, so the incremental scan would otherwise never read it again.
    let scratch = TempLibrary {
        root: root.join("music"),
        library: Library::open_at(&root.join("scratch.db")).unwrap(),
    };
    tagged_wav(&scratch, "Tide", "Harbor", "Dawn", Some("Lo-Fi"));
    let file = scratch.root.join("Tide/Harbor/Dawn.wav");
    let metadata = fs::metadata(&file).unwrap();
    let mtime = i64::try_from(
        metadata
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    )
    .unwrap();
    let size = i64::try_from(metadata.len()).unwrap();
    {
        let old = rusqlite::Connection::open(&database).unwrap();
        old.execute_batch(OLD_SCHEMA).unwrap();
        old.execute(
            "INSERT INTO tracks (path, title, artist, album, format, mtime, size)
             VALUES (?1, 'Dawn', 'Tide', 'Harbor', 'wav', ?2, ?3)",
            rusqlite::params![file.to_string_lossy(), mtime, size],
        )
        .unwrap();
    }

    // Opening it with this velo never fails and never drops a row.
    let library = Library::open_at(&database).unwrap();
    assert_eq!(library.counts().unwrap().tracks, 1, "the old row survives");
    assert_eq!(library.all_tracks().unwrap()[0].title, "Dawn");

    // The next index re-reads the file even though it did not change…
    let report = library.index(&scratch.root).unwrap();
    assert_eq!((report.added, report.updated, report.removed), (0, 1, 0));
    let genres: Vec<String> = library
        .genres()
        .unwrap()
        .into_iter()
        .map(|genre| genre.name)
        .collect();
    assert_eq!(genres, ["Lo-Fi"]);

    // …and only that once: reopening does not force another full rescan.
    drop(library);
    let library = Library::open_at(&database).unwrap();
    let again = library.index(&scratch.root).unwrap();
    assert_eq!(again.unchanged, 1);
    drop(scratch);
    let _ = fs::remove_dir_all(&root);
}
