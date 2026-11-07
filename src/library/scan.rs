//! Parallel scan of the library root: extension filter, tag reading with
//! path-layout fallback, and the incremental unchanged-file shortcut.

use std::collections::{HashMap, HashSet};
use std::fs::Metadata;
use std::io;
use std::path::Path;
use std::time::UNIX_EPOCH;

use jwalk::WalkDir;
use lofty::prelude::*;
use lofty::probe::Probe;

/// Extensions that are tracks; nothing else enters the index (S5, S6).
pub const AUDIO_EXTENSIONS: &[&str] = &["mp3", "flac", "m4a", "ogg", "opus", "wav"];

/// A track row, ready for the database.
#[derive(Debug, Clone)]
pub struct TrackRecord {
    pub path: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: Option<String>,
    pub track_no: Option<i64>,
    pub disc_no: Option<i64>,
    pub year: Option<i64>,
    pub duration_ms: i64,
    pub sample_rate: Option<i64>,
    pub channels: Option<i64>,
    pub format: String,
    pub has_cover: bool,
    pub mtime: i64,
    pub size: i64,
}

/// What one pass over the tree produced.
pub struct ScanOutcome {
    /// New or changed files, in walk order.
    pub fresh: Vec<TrackRecord>,
    /// Every audio path found, so the caller can delete vanished rows.
    pub seen: HashSet<String>,
    /// Files whose mtime and size match the index; never re-read.
    pub unchanged: usize,
}

/// Walks `root` in parallel and returns records for every file that is new
/// or has changed on disk (different mtime or size than in `known`).
pub fn scan(root: &Path, known: &HashMap<String, (i64, i64)>) -> io::Result<ScanOutcome> {
    let mut fresh = Vec::new();
    let mut seen = HashSet::new();
    let mut unchanged = 0_usize;

    for entry in WalkDir::new(root).skip_hidden(true) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let Some(extension) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if !AUDIO_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str()) {
            continue;
        }

        let path_string = path.to_string_lossy().into_owned();
        seen.insert(path_string.clone());

        let Ok(metadata) = std::fs::metadata(&path) else {
            // The file vanished between the walk and the stat; it stays out
            // of the index this round and is deleted with the rest.
            continue;
        };
        let stamp = (mtime_nanos(&metadata), metadata_len(&metadata));
        if known.get(&path_string) == Some(&stamp) {
            unchanged += 1;
            continue;
        }

        fresh.push(read_track(root, &path, &metadata, extension));
    }

    Ok(ScanOutcome {
        fresh,
        seen,
        unchanged,
    })
}

/// Reads one track: tags first, path layout for whatever the tag lacks.
fn read_track(root: &Path, path: &Path, metadata: &Metadata, extension: &str) -> TrackRecord {
    let tags = read_tags(path);
    let layout = path_layout(root, path);

    // Tags are the source of truth; the folder layout only fills the gaps.
    let take = |tag: Option<String>, fallback: Option<String>| -> Option<String> {
        tag.filter(|value| !value.is_empty()).or(fallback)
    };

    let title = take(
        tags.as_ref().and_then(|t| t.title.clone()),
        Some(layout.title).filter(|t| !t.is_empty()),
    )
    .unwrap_or_else(|| "Unknown title".to_owned());
    let artist = take(
        tags.as_ref().and_then(|t| t.artist.clone()),
        layout.artist.clone(),
    )
    .unwrap_or_else(|| "Unknown artist".to_owned());
    let album = take(
        tags.as_ref().and_then(|t| t.album.clone()),
        layout.album.clone(),
    )
    .unwrap_or_else(|| "Unknown album".to_owned());

    TrackRecord {
        path: path.to_string_lossy().into_owned(),
        title,
        artist,
        album,
        album_artist: tags.as_ref().and_then(|t| t.album_artist.clone()),
        track_no: tags.as_ref().and_then(|t| t.track_no).or(layout.track_no),
        disc_no: tags.as_ref().and_then(|t| t.disc_no),
        year: tags.as_ref().and_then(|t| t.year).or(layout.year),
        duration_ms: tags.as_ref().map_or(0, |t| t.duration_ms),
        sample_rate: tags.as_ref().and_then(|t| t.sample_rate),
        channels: tags.as_ref().and_then(|t| t.channels),
        format: extension.to_ascii_lowercase(),
        has_cover: tags.as_ref().is_some_and(|t| t.has_cover),
        mtime: mtime_nanos(metadata),
        size: metadata_len(metadata),
    }
}

struct TagMeta {
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    album_artist: Option<String>,
    track_no: Option<i64>,
    disc_no: Option<i64>,
    year: Option<i64>,
    duration_ms: i64,
    sample_rate: Option<i64>,
    channels: Option<i64>,
    has_cover: bool,
}

/// Reads tags with lofty. A file that cannot be parsed is not an error: the
/// caller falls back to the path layout, so it still lands in the index.
fn read_tags(path: &Path) -> Option<TagMeta> {
    let Ok(tagged) = Probe::open(path).and_then(Probe::read) else {
        eprintln!(
            "velo: {}: unreadable tags, indexing from the path layout",
            path.display()
        );
        return None;
    };
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    let properties = tagged.properties();

    // Numeric tag values may carry totals or dates ("3/12", "2003-05-01");
    // only the leading number is meaningful here.
    let number = |key: ItemKey| -> Option<i64> {
        tag.and_then(|t| t.get_string(key)).and_then(|text| {
            let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
    };
    let text =
        |key: ItemKey| -> Option<String> { tag.and_then(|t| t.get_string(key)).map(str::to_owned) };

    Some(TagMeta {
        title: text(ItemKey::TrackTitle),
        artist: text(ItemKey::TrackArtist),
        album: text(ItemKey::AlbumTitle),
        album_artist: text(ItemKey::AlbumArtist),
        track_no: number(ItemKey::TrackNumber),
        disc_no: number(ItemKey::DiscNumber),
        // ID3v2 dates land under RecordingDate; older tags use Year.
        year: number(ItemKey::Year)
            .or_else(|| number(ItemKey::RecordingDate))
            .or_else(|| number(ItemKey::OriginalReleaseDate)),
        duration_ms: i64::try_from(properties.duration().as_millis()).unwrap_or(0),
        sample_rate: properties.sample_rate().map(i64::from),
        channels: properties.channels().map(i64::from),
        has_cover: tag.is_some_and(|t| !t.pictures().is_empty()),
    })
}

/// What the `Artist/YYYY - Album/NN - Title.ext` layout says about a file.
struct PathMeta {
    title: String,
    artist: Option<String>,
    album: Option<String>,
    track_no: Option<i64>,
    year: Option<i64>,
}

/// `root/Artist/2009 - Album/03 - Title.ext` → artist "Artist", album
/// "Album", year 2009, track 3, title "Title". Every component is optional:
/// a shallower file simply yields less.
fn path_layout(root: &Path, path: &Path) -> PathMeta {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let mut components = relative.components();

    let artist = components
        .next()
        .and_then(|c| c.as_os_str().to_str())
        .map(str::to_owned);
    let album_directory = components.next().and_then(|c| c.as_os_str().to_str());
    let file_name = components.next().map(|c| c.as_os_str().to_os_string());

    let (album, year) =
        album_directory.map_or((None, None), |directory| match split_prefix(directory, 4) {
            // `2009 - Album`: the digits are the year, the rest the album.
            Some((year, rest)) => (
                Some(rest.trim_start().to_owned()).filter(|a| !a.is_empty()),
                year,
            ),
            // No `YYYY - ` prefix: the whole directory name is the album.
            None => (Some(directory.to_owned()), None),
        });

    let title = file_name
        .as_ref()
        .and_then(|name| Path::new(name).file_stem().and_then(|s| s.to_str()))
        .unwrap_or("");
    let (track_no, title) = split_prefix(title, 2)
        .map_or((None, title.to_owned()), |(number, rest)| {
            (number, rest.trim_start().to_owned())
        });

    PathMeta {
        title,
        artist,
        album,
        track_no,
        year,
    }
}

/// If the first `digits` characters of `text` are all digits, returns them as
/// a number and the rest with one ` - ` separator stripped.
fn split_prefix(text: &str, digits: usize) -> Option<(Option<i64>, &str)> {
    let head = text.get(..digits)?;
    let rest = text.get(digits..)?;
    if !head.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let number: i64 = head.parse().ok()?;
    let rest = rest.strip_prefix(" - ").unwrap_or(rest);
    Some((Some(number), rest))
}

fn mtime_nanos(metadata: &Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| {
            i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX)
        })
}

fn metadata_len(metadata: &Metadata) -> i64 {
    i64::try_from(metadata.len()).unwrap_or(i64::MAX)
}
