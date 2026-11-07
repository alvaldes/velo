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
