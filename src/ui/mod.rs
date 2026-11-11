//! The terminal interface: an [`App`] whose keymap is pure and testable, and a
//! `draw` that only renders it.
//!
//! Every state change answers through an [`Action`], so the key rules are
//! tested in `tests/ui_state.rs` without a terminal. Wiring the [`Player`] to
//! those actions happens in [`App::apply`], which is never exercised by tests.
//!
//! `App` knows two ways to stand over a list of rows. `App::new` keeps the
//! flat list of files given on the command line. `App::browse` stands over the
//! library index instead, in one of four views ([`BrowseBy`], keys `1`–`4`):
//! artists, then the albums of the selected artist, then the tracks of the
//! selected album; every album, then its tracks; every track; every genre,
//! then its tracks. In browse mode the rows on screen come from an in-memory
//! snapshot that is refreshed when the position changes, never once per
//! frame.

pub mod artwork;
pub mod complete;
pub mod indexing;
pub mod radio;
pub mod radio_data;
pub mod rowtext;
pub mod search;
pub mod shuffle;
pub mod sources;
pub mod theme;
pub mod visualizer;

pub use ratatui;
pub use ratatui::crossterm;

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::mpsc::TryRecvError;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph};

use crate::audio::output::{DeviceList, Entry, PathResolver, Repeat, SharedQueue, TrackFailure};
use crate::audio::replay_gain::{Loudness, parse_db, parse_peak};
use crate::audio::spectrum::Spectrum;
use crate::audio::{Player, TrackSource, seek_target, skip_target};
use crate::config::{
    LoadedTheme, OpenNode, ReplayGainMode, Session, ThemeColors, ThemeKeys, UiParts,
};
use crate::library::{AlbumEntry, ArtistEntry, GenreEntry, IndexReport, Library, TrackEntry};
use crate::subsonic::cache::{CoverArt, is_pseudo_path, is_remote_url};
use crate::subsonic::scrobble::{ScrobbleTracker, Scrobbles};

use lofty::file::{AudioFile, FileType, TaggedFileExt};
use lofty::probe::Probe;
use lofty::tag::ItemKey;

use self::artwork::{Artwork, Picker, StatefulImage, StatefulProtocol};
use self::search::Search;
use self::theme::Theme;
use self::visualizer::Visualizer;

/// What the interface says at a launch with neither a library nor a Subsonic
/// server configured (T12c): one sentence naming both ways to configure, in
/// the place the floating prompt used to take over. `L` still opens the
/// prompt at any time.
pub const NO_LIBRARY_OR_SERVER_MESSAGE: &str = "velo: no library and no server configured — set \
    `library` in the config file (or run velo --library <PATH>), or add a \
    [subsonic] server and run velo --sync";

/// What mono and bit-perfect playback say about each other (issue #8, S3):
/// one sentence — at startup, on stderr, and in the status line when `M` is
/// pressed under bit-perfect. Never applied silently.
pub const MONO_WITH_BIT_PERFECT_REFUSED: &str = "velo: --mono cannot be combined with bit_perfect";

/// What one key press asks the runtime to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Nothing to do.
    None,
    /// Leave the interface.
    Quit,
    /// Start playing the track at this index.
    Play { from: usize },
    /// Start playing the queue from this index.
    PlayQueue { from: usize },
    /// Go one level deeper into the current selection.
    Descend,
    /// Go one level up; at the top level this does nothing.
    Ascend,
    /// Pause or resume.
    TogglePause,
    /// The new volume, in percent.
    SetVolume(u8),
    /// Move the position by this many seconds; negative is back.
    Seek { delta_secs: i64 },
    /// Jump to this position, in seconds from the start of the track.
    SeekTo { secs: u64 },
    /// Play the next track of the stream.
    Next,
    /// Restart the track, or play the one before it when it just began.
    Previous,
    /// Shuffle what is left of the queue, or put it back in order.
    ToggleShuffle,
    /// What follows the end of a track from now on.
    SetRepeat(Repeat),
    /// Fold the mix down to mono — or back out of it — for the rest of the
    /// session (issue #8, S2). Never persisted.
    ToggleMono,
    /// Low power for the rest of the session — or back out of it (issue
    /// #11): a quarter of the redraw cadence and no spectrum. Remembered
    /// under `[ui] low_power`, the way shuffle is remembered under `[state]`.
    ToggleLowPower,
    /// Check the path typed into the library prompt; save and index it when
    /// it is a directory, otherwise say why in the prompt.
    SetLibrary,
    /// Complete the folder being typed in the library prompt.
    CompleteLibraryPath,
    /// Show the library in this view, from its top level.
    Browse(BrowseBy),
    /// Refresh the index of the configured library in the background; the
    /// shell runs it and reports back with [`App::index_finished`].
    Reindex,
}

/// The four ways to look at the library, on the keys `1` to `4`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BrowseBy {
    /// Artists, then their albums, then an album's tracks.
    #[default]
    Artists,
    /// Every album, then its tracks.
    Albums,
    /// Every track in one list.
    Tracks,
    /// Every genre, then its tracks.
    Genres,
}

impl BrowseBy {
    /// Every view, in the order of its key.
    const ALL: [Self; 4] = [Self::Artists, Self::Albums, Self::Tracks, Self::Genres];

    /// The view a number key asks for.
    const fn from_key(key: char) -> Option<Self> {
        match key {
            '1' => Some(Self::Artists),
            '2' => Some(Self::Albums),
            '3' => Some(Self::Tracks),
            '4' => Some(Self::Genres),
            _ => None,
        }
    }

    /// The view's name in the list header.
    const fn label(self) -> &'static str {
        match self {
            Self::Artists => "artists",
            Self::Albums => "albums",
            Self::Tracks => "tracks",
            Self::Genres => "genres",
        }
    }
}

/// What the integration tests' fixture reports for the transport (T6c):
/// the values a playing `Player` would answer. A reader consults it only
/// where no player exists to answer, which in production is never.
#[derive(Debug)]
struct TransportFixture {
    /// The position the clock shows.
    position: Duration,
    /// The total the clock shows, when the track declares one.
    duration: Option<Duration>,
    /// The ICY title a live station is announcing (S4), when one is
    /// published.
    title: Option<String>,
    /// The queue index the transport is fetching, stored the way the real
    /// player's cell answers it (issue #60): `None` is no fetch going on.
    fetching: Option<usize>,
}

/// What a finished fetch starts, exactly as the request named it — kept,
/// never re-derived, so the start the wait ends is the start that was asked
/// for (issue #60's second half).
enum PendingStart {
    /// A fresh stream: the entries as the caller built them and the
    /// playback kind.
    Stream {
        /// The entries the stream was asked to open.
        paths: Vec<Entry>,
        /// Which list the playback follows.
        playback: Playback,
    },
    /// A rebuilt player: the standing queue at `track`, `offset` into it —
    /// a skip's or a seek's rebuild.
    TrackAt {
        /// The queue index the rebuild starts at.
        track: usize,
        /// How far into the track it starts.
        offset: Duration,
    },
}

/// The first entry's resolve, running on its worker while the event loop
/// paints (issue #60's second half). The slot is the whole state: a newer
/// play, skip or seek replaces it, and the displaced worker's answer is
/// dropped unread — its download, if one started, lands in the cache and is
/// simply reused later.
struct PendingResolve {
    /// The entry being resolved, for the status line: the file's path.
    path: PathBuf,
    /// The worker's answer, polled on the tick and never waited on: the
    /// resolved file for a finished fetch, the reason for a failed one.
    /// The error is carried as its own sentence — the status line's shape —
    /// so the answer needs nothing else.
    answer: std::sync::mpsc::Receiver<Result<PathBuf, String>>,
    /// What the finished fetch starts.
    start: PendingStart,
}

/// The floating prompt that asks for the library root when none is set.
#[derive(Debug)]
struct LibraryPrompt {
    /// The path as typed, always starting with `~/`: the library is a folder
    /// under home, never a path relative to where velo was launched.
    input: String,
    /// Why the last answer was refused, shown in the card.
    error: Option<String>,
    /// The folders the last `Tab` could not choose between.
    candidates: Vec<String>,
}

/// The seek prompt (issue #6): what is being typed at `:`, and why the last
/// answer was refused. Nothing here touches the player.
#[derive(Debug, Default)]
struct SeekPrompt {
    /// The position as typed.
    input: String,
    /// Why the last answer was refused, shown in the card.
    error: Option<String>,
}

/// The playlist-name prompt (issue #4): what is being typed and why the
/// last answer was refused. The same card the seek prompt draws, a question
/// over the typed name and the reason a refused one stays.
#[derive(Debug)]
struct PlaylistPrompt {
    /// The name as typed.
    input: String,
    /// Why the last answer was refused, shown in the card.
    error: Option<String>,
    /// The name being renamed, when the prompt renames (issue #4, S5):
    /// `None` saves the queue as a new playlist (S3).
    rename: Option<String>,
}

/// The URL prompt (issue #42): what is being typed and why the last answer
/// was refused. The same card the seek prompt draws, a question over the
/// typed URL and the reason a refused one stays.
#[derive(Debug, Default)]
struct UrlPrompt {
    /// The URL as typed.
    input: String,
    /// Why the last answer was refused, shown in the card.
    error: Option<String>,
}

/// The save prompt (issue #45): where a remote track's copy lands. The same
/// card the seek prompt draws, prefilled with `~/Downloads/<the file's own
/// name>`; `esc` cancels, and an answer naming a file that is already on
/// disk is refused there — the name is kept, velo never overwrites.
#[derive(Debug)]
struct SavePrompt {
    /// The destination as typed.
    input: String,
    /// Why the last answer was refused, shown in the card.
    error: Option<String>,
    /// The entry the copy reads out: the `subsonic://…` row or the
    /// `http(s)://` URL, exactly as the queue carries it.
    source: String,
    /// The file's own name — the prefill's tail, and the name the status
    /// line reports the copy under.
    name: String,
}

/// One saving copy's events: the progress the worker can honestly report,
/// then its one last word — where the file landed, or why it could not.
enum SaveEvent {
    /// `done` bytes written, of `total` when the copy knows the whole size.
    Progress { done: u64, total: Option<u64> },
    /// The copy is finished, the file is whole at the destination.
    Saved { destination: PathBuf },
    /// Nothing was left behind: the reason is the worker's sentence.
    Failed(String),
}

/// The saving copy, on the thread it runs on: [`App::pending_save`] holds
/// the channel it answers on, and the tick drains it — the pattern the
/// cover fetch and the radio wiring already use.
struct SaveRun {
    /// The file's own name, what the status line reports the copy under.
    name: String,
    /// The worker's events, drained once per tick.
    receiver: std::sync::mpsc::Receiver<SaveEvent>,
}

/// The small overlay `S` opens on a library track row (issue #4, S3): the
/// saved playlists' names, one row each; `enter` appends the row's track to
/// the chosen one, `esc` cancels.
#[derive(Debug)]
struct PlaylistPicker {
    /// The track the cursor was on, exactly the path the row carries — a
    /// `subsonic://` path stays itself, the way a favourite's does.
    track: String,
    /// The saved names, in the store's order, cloned in when it opened.
    names: Vec<String>,
    /// The row the cursor is on.
    selected: usize,
}

impl LibraryPrompt {
    fn new() -> Self {
        Self {
            input: complete::PROMPT_PREFIX.to_owned(),
            error: None,
            candidates: Vec::new(),
        }
    }
}

/// Setting the library up because none is configured (S4): where an answer
/// is saved and the prompt, while it is open.
#[derive(Debug, Default)]
struct LibrarySetup {
    /// The config file an accepted library path is saved to.
    config_file: Option<PathBuf>,
    /// The home folder `~/` stands for, read once when the prompt opens.
    home: Option<PathBuf>,
    /// The open prompt; `None` once it was skipped with `Esc`.
    prompt: Option<LibraryPrompt>,
}

/// The recently played file (issue #5), as the interface holds it between
/// ticks. `seen` is the identity written last: the tick sees the same entry
/// on every frame (a pause, a seek that rebuilds the player), so only a
/// change of identity — a file path or a station's URL key — counts as a
/// beginning.
#[derive(Debug, Default)]
struct Recent {
    entries: crate::history::History,
    seen: Option<String>,
}

/// Refreshing the index (library-refresh T3): which folder `I` re-indexes
/// and which one is being indexed right now.
#[derive(Debug, Default)]
struct Refresh {
    /// The configured library root, or the one chosen in the prompt.
    root: Option<PathBuf>,
    /// The folder a background index is scanning, while it runs.
    running: Option<PathBuf>,
    /// Why the index could not be opened at launch, when it could not.
    open_error: Option<String>,
}

/// Where in the library the interface stands while browsing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Level {
    /// Every artist in the index.
    Artists,
    /// The albums of one artist.
    Albums { artist: String },
    /// The tracks of one album of one artist.
    Tracks { artist: String, album: String },
    /// Every album in the index.
    AllAlbums,
    /// Every track in the index.
    AllTracks,
    /// Every genre in the index.
    Genres,
    /// The tracks of one genre.
    Genre { genre: String },
    /// The history's entries (issue #5): the `Recently played` source, not a
    /// level of the index.
    Recent,
    /// The saved store's favourites (issue #7): the `Favourites` source, not
    /// a level of the index.
    Favourites,
    /// The saved store's playlists (issue #4): the `Playlists` source, not
    /// a level of the index.
    Playlists,
    /// The tracks of one saved playlist (issue #4), in the order the store
    /// holds them.
    Playlist {
        /// The playlist's name, the store's identity for it.
        name: String,
    },
}

/// Where the interface stands, including the flat command-line list.
#[derive(Debug)]
enum Position {
    /// One flat level over the files given on the command line.
    Files,
    Artists,
    Albums {
        artist: String,
    },
    Tracks {
        artist: String,
        album: String,
    },
    AllAlbums,
    AllTracks,
    Genres,
    Genre {
        genre: String,
    },
    /// The history's entries (issue #5), newest first — the `Recently
    /// played` source. Not a level of the index: it is never read from the
    /// library and never persisted as a node.
    Recent,
    /// The saved store's favourites (issue #7) — the `Favourites` source.
    /// Not a level of the index: its rows are the store's entries, cloned in
    /// when the source opens, and nothing here is persisted as a node.
    Favourites,
    /// The saved store's playlists (issue #4) — the `Playlists` source: the
    /// names, one row each. Not a level of the index: its rows are the
    /// store's playlists, cloned in when the source opens, and nothing here
    /// is persisted as a node.
    Playlists,
    /// One saved playlist's tracks (issue #4), in the order the store holds
    /// them. The rows are read from the store as it stands, so a move is on
    /// screen the frame after it is saved, and nothing here is persisted as
    /// a node.
    Playlist {
        /// The playlist's name, the store's identity for it.
        name: String,
    },
}

/// Where the stream that is playing came from. The row it plays is found by
/// the file's path, so the stream's start index is no longer kept: a
/// shuffled queue does not follow any list's order.
#[derive(Debug)]
enum Playback {
    /// The files given on the command line.
    Files,
    /// The queue itself, rotated so the entry it started from is its head.
    Queue,
    /// An album, from the selected track onwards.
    Album,
    /// A library list — every track, or one genre — from the selected track
    /// onwards.
    List,
}

/// Which list the arrow keys and the editing commands act on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum View {
    /// The library browser, or the flat command-line list.
    #[default]
    Library,
    /// The queue, in play order.
    Queue,
    /// The sources — the first screen (issue #3), one level above the
    /// library and the radio: it names what velo can play. Not a tab —
    /// `Tab` keeps toggling the two below it.
    Sources,
}

/// A player handed over mid-crossfade (issue #65): the previous source keeps
/// sounding while the new one rises. Its fade-out is armed only once the new
/// stream has started sounding — two fades of silence are not an overlap —
/// and the player stops once the fade has fully fallen.
struct Crossfading {
    player: Player,
    /// Whether the fade-out has been armed.
    armed: bool,
}

/// Where playback stands, for the header and the footer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayState {
    Stopped,
    Playing,
    Paused,
}

impl PlayState {
    const fn label(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Playing => "playing",
            Self::Paused => "paused",
        }
    }
}

/// The default URL opener (S3, issue #19): the system's own — `open` on
/// macOS, `xdg-open` elsewhere — spawned without waiting for it, so the
/// interface never blocks on a browser. A failure comes back as the error's
/// text; the caller puts the URL on the status line so nothing is lost.
fn open_url(url: &str) -> Result<(), String> {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(program)
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// The prefilled GitHub issue URL for one radio report (S3, issue #19):
/// the title and the body percent-encoded, the `bug` label attached. The
/// URL is all velo builds — the browser shows the draft and the human
/// sends it; nothing here ever posts.
#[must_use]
fn issue_url(title: &str, body: &str) -> String {
    format!(
        "https://github.com/alvaldes/velo/issues/new?title={}&body={}&labels=bug",
        percent_encode(title),
        percent_encode(body),
    )
}

/// Percent-encodes a URL component: every byte but the unreserved ones
/// (`A`–`Z`, `a`–`z`, `0`–`9`, `-`, `_`, `.`, `~`) becomes `%XX` with the
/// hex digits in upper case — including the space, so no `+` ambiguity.
#[must_use]
fn percent_encode(text: &str) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => {
                let _ = write!(encoded, "%{byte:02X}");
            }
        }
    }
    encoded
}

/// `1 track`, `2 tracks`: a count reads as English, not as a log line.
#[must_use]
pub fn plural(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {many}")
    }
}

/// Characters a destination file name cannot carry are replaced, not
/// dropped: a title keeps its shape (`AC/DC` becomes `AC-DC`) and stays a
/// single path component. Empty after trimming, [`saved_file_name`] says
/// `track` — the one honest name when nothing names the file.
#[must_use]
fn saved_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|character| {
            if matches!(character, '/' | '\\') {
                '-'
            } else {
                character
            }
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        String::from("track")
    } else {
        trimmed.to_owned()
    }
}

/// The copy's progress, as it can honestly be said (issue #45, S2): a
/// percentage when the copy knows the whole size, the saving note alone
/// when it does not. Never a made-up number.
#[doc(hidden)]
#[must_use]
pub fn save_progress_line(name: &str, done: u64, total: Option<u64>) -> String {
    let percent = total.filter(|total| *total > 0).map(|total| {
        let percent = u128::from(done) * 100 / u128::from(total);
        u64::try_from(percent).unwrap_or(u64::MAX)
    });
    percent.map_or_else(
        || format!("⟳ saving {name}…"),
        |percent| format!("⟳ saving {name}… {percent}%"),
    )
}

/// Spawns the saving copy's worker (issue #45, S2): it resolves the entry
/// through the resolver — the exact call the producer makes, so a cache hit
/// calls no network and a miss is the cache's ordinary fetch, never a
/// duplicated download — then copies the cached file out to the
/// destination, reporting its progress through the channel. The UI never
/// waits on it: the tick drains the channel.
fn spawn_save_worker(
    resolver: Arc<dyn PathResolver>,
    source: String,
    destination: PathBuf,
) -> std::sync::mpsc::Receiver<SaveEvent> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(SaveEvent::Progress {
            done: 0,
            total: None,
        });
        let cached_file = match resolver.resolve(Path::new(&source)) {
            Ok(file) => file,
            Err(error) => {
                let _ = sender.send(SaveEvent::Failed(error.to_string()));
                return;
            }
        };
        let total = fs::metadata(&cached_file)
            .ok()
            .map(|metadata| metadata.len())
            .filter(|len| *len > 0);
        let outcome = match copy_out(&cached_file, &destination, &sender, total) {
            Ok(()) => sender.send(SaveEvent::Saved { destination }),
            Err(reason) => sender.send(SaveEvent::Failed(reason)),
        };
        let _ = outcome;
    });
    receiver
}

/// Copies the cached file to the destination (issue #45, S4): the bytes go
/// through a temporary sibling in the destination's own directory, renamed
/// into place at the end — so a failure anywhere leaves no half file, only
/// a removed temporary — and a file that appeared at the destination while
/// the copy ran is not overwritten. Progress rides the channel as it
/// honestly is: percentages only when the whole size is known, and no more
/// often than the number moves.
fn copy_out(
    from: &Path,
    destination: &Path,
    sender: &std::sync::mpsc::Sender<SaveEvent>,
    total: Option<u64>,
) -> Result<(), String> {
    /// The copy reads in chunks of this size — small enough that the
    /// progress it reports moves in visible steps, large enough that a
    /// multi-megabyte track is a handful of reads.
    const CHUNK: usize = 64 * 1024;
    /// Between honest reports of an unmeasured copy: 8 MiB of news at a
    /// time, so a stream of unknown length does not flood the channel.
    const UNMETERED_STEP: u64 = 8 * 1024 * 1024;

    let mut input = fs::File::open(from).map_err(|error| error.to_string())?;
    let folder = destination.parent().unwrap_or(Path::new("/"));
    let file_name = destination.file_name().map_or_else(
        || "velo".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    let temp = folder.join(format!(".{file_name}.velo-part-{}", std::process::id()));
    let mut copy = || -> Result<(), String> {
        let mut output = fs::File::create(&temp).map_err(|error| error.to_string())?;
        let mut buffer = vec![0_u8; CHUNK];
        let mut done: u64 = 0;
        let mut next_step: u64 = 0;
        let mut reported_percent: i64 = -1;
        loop {
            let read = input.read(&mut buffer).map_err(|error| error.to_string())?;
            if read == 0 {
                break;
            }
            output
                .write_all(&buffer[..read])
                .map_err(|error| error.to_string())?;
            done = done.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
            let report = match total {
                Some(total) => {
                    let percent = i64::try_from(u128::from(done) * 100 / u128::from(total))
                        .unwrap_or(i64::MAX);
                    if percent > reported_percent {
                        reported_percent = percent;
                        true
                    } else {
                        false
                    }
                }
                None => {
                    if done >= next_step {
                        next_step += UNMETERED_STEP;
                        true
                    } else {
                        false
                    }
                }
            };
            if report {
                let _ = sender.send(SaveEvent::Progress { done, total });
            }
        }
        output.flush().map_err(|error| error.to_string())?;
        drop(output);
        if destination.exists() {
            return Err(String::from(
                "the destination appeared while the copy ran — it was not overwritten",
            ));
        }
        fs::rename(&temp, destination).map_err(|error| error.to_string())
    };
    let outcome = copy();
    if outcome.is_err() {
        let _ = fs::remove_file(&temp);
    }
    outcome
}

/// The whole interface state, minus the terminal.
#[expect(
    clippy::struct_excessive_bools,
    reason = "the flags are independent UI facts: whether art was explained,\n    whether shuffle is on, whether help is open, and — since bit-perfect\n    (T17) — whether playback is bit-perfect and whether its volume rule\n    was explained once; bundling them would obscure each"
)]
pub struct App<'lib> {
    position: Position,
    /// The first screen (issue #3): which source the cursor is on, while
    /// the sources view is up.
    sources: sources::Sources,
    /// The files given on the command line, in [`Position::Files`].
    argv: Vec<PathBuf>,
    library: Option<&'lib Library>,
    /// Which of the four library views the browser shows.
    browse_by: BrowseBy,
    artists: Vec<ArtistEntry>,
    /// The albums of one artist, or every album in [`Position::AllAlbums`].
    albums: Vec<AlbumEntry>,
    genres: Vec<GenreEntry>,
    /// The tracks of an album, of a genre, or every track.
    tracks: Vec<TrackEntry>,
    /// The rows the running stream was started from, so the now-playing
    /// block still names its track after the browser moved elsewhere.
    played: Vec<TrackEntry>,
    selected: usize,
    /// The entries of the running stream, in queue order, so a seek can
    /// rebuild the player from the current track at an offset.
    queue: Vec<Entry>,
    /// The stream that is playing, if any.
    playing: Option<Playback>,
    /// Which list the keys act on.
    view: View,
    /// The selected row of the queue view.
    queue_selected: usize,
    /// The handle the running player reads its queue from, so an edit reaches
    /// the audio instead of only the screen (T18).
    shared: Option<SharedQueue>,
    /// The area the last frame was drawn into, so a hidden cover can say why.
    seen_area: std::cell::Cell<Rect>,
    /// Whether the reason a cover is missing has already been said.
    art_explained: bool,
    /// How `ReplayGain` is applied, from the config; the player reads it when a
    /// stream is built, so a seek keeps honouring it (T11).
    replay_gain: ReplayGainMode,
    /// The palette the interface draws with, from the config's `[theme]` (S4).
    theme: Theme,
    /// Which parts of the interface are drawn, from the config's `[ui]` (S5).
    ui: UiParts,
    /// Whether what is left of the queue plays in a mixed order.
    shuffle: bool,
    /// The queue as it was before it was shuffled, to put it back.
    unshuffled: Vec<Entry>,
    /// What follows the end of a track; handed to every player built.
    repeat: Repeat,
    /// The open search, if any (T7). While it is open the body lists its
    /// results and nearly every key types into it; the browse state below this
    /// field is never touched, which is how `Esc` restores it exactly.
    search: Option<Search>,
    /// Every track of the library, read once when the search opens.
    search_pool: Vec<TrackEntry>,
    /// `"artist album title"` per pooled track, in pool order: the haystacks
    /// the query matches across.
    search_haystacks: Vec<String>,
    /// Ranked indices into `search_pool`.
    search_results: Vec<usize>,
    /// The selected row of the result list.
    search_selected: usize,
    /// The picker the shell detected once at startup, `None` when the
    /// terminal speaks no graphics protocol (T8). No picker, no art.
    picker: Option<Picker>,
    /// The decoded cover of the track that is playing, if its file has one.
    /// Replaced only when the track changes; a file is never re-read per frame.
    artwork: Option<Artwork>,
    /// The file the current artwork was decoded from, so a track change is
    /// seen with one comparison instead of a re-extraction.
    artwork_path: Option<PathBuf>,
    /// The rendered image protocol, keyed to the current artwork. Interior
    /// mutability because `draw` takes `&self`: the stateful protocol re-encodes
    /// only when its area changes, so redrawing stays cheap.
    protocol: RefCell<Option<Box<dyn StatefulProtocol>>>,
    /// The help card while it is open: its scroll and its filter (S1).
    help: Option<Help>,
    /// The settings card while it is open: its cursor and its scroll (S6).
    settings: Option<SettingsCard>,
    /// The info card while it is open (issue #43): the rows it shows,
    /// gathered once at the key press, never re-read afterwards.
    info: Option<InfoCard>,
    /// The open radio view (T6a): `None` while only the library and the
    /// queue are on screen.
    radio: Option<radio::RadioView>,
    /// The transport fixture the integration tests install (T6c): no test
    /// can build a `Player` — building one opens an audio device — so the
    /// tests stand in what a playing player would report. Production never
    /// sets it, and every reader takes the real player first, so the player
    /// stays the transport's only source of truth.
    transport_fixture: Option<TransportFixture>,
    /// The first entry's resolve, running on its worker (issue #60):
    /// when the entry that would be opened first is remote and not cached,
    /// `apply` hands the resolve here, says `⟳ fetching …` and returns —
    /// the event loop paints at once, the download runs off it — and the
    /// tick polls the answer, starting the stream the way it always starts
    /// once the file is on disk. One slot: a newer play, skip or seek
    /// supersedes the one waiting, so two workers never race for the device.
    pending_resolve: Option<PendingResolve>,
    /// The radio's data wiring (T6b): the preference store and the
    /// directory requests, which run on a worker thread the tick polls.
    /// `None` until the first `R` opens the radio; built once, kept for
    /// the session so loaded indexes survive closing the view.
    radio_wire: Option<Box<dyn radio_data::RadioWire>>,
    /// The screen the radio was opened from, so closing it lands there (S6):
    /// `Sources` when it was chosen there, the library or the queue when `R`
    /// opened it over them.
    radio_from: View,
    /// The refusal note the status line last showed for the radio's cursor
    /// (S1, issue #19): `None` when the cursor is not on a rejected station
    /// or the note is not on the line. The note is only rewritten when this
    /// changes, so no tick rewrites the line, and it is only cleared when
    /// it still carries the note — never another writer's message.
    radio_refusal: Option<String>,
    /// The opener the radio's report key hands issue URLs to (S3, issue
    /// #19): the system's own (`open` on macOS, `xdg-open` elsewhere),
    /// spawned without waiting for it. A function pointer so the tests can
    /// install a recorder — no test ever spawns a browser. velo holds no
    /// token and posts nothing: the browser shows the draft, and the human
    /// reads it and submits.
    url_opener: fn(&str) -> Result<(), String>,
    /// The screen `tab` returns to (issue #23): the round trip's other end.
    /// It is remembered when the queue is entered from anywhere and returned
    /// to when the queue is left. Default `View::Sources`.
    tab_origin: View,
    /// The titles a wrapper gave its songs, keyed by the song's URL (issue
    /// #34). Session-scoped on purpose: the names come from expanding the
    /// channel, and a fresh session re-derives them when the channel is
    /// opened again — a song that was never expanded in this session keeps
    /// today's fallback, its URL. Read only by [`describe`], which names a
    /// path for display; identity (history, favourites, playlists) stays the
    /// URL.
    song_titles: HashMap<String, String>,
    /// The expansion verdicts of this session, keyed by the configured
    /// station's URL (issue #34): opening the same channel again does not
    /// fetch the wrapper again. A FAILED expansion is never remembered — a
    /// transient error must stay retryable — and a `Live` verdict is not
    /// remembered either: a plain stream never needed the probe.
    expansion_cache: HashMap<String, crate::radio::stream::Expansion>,
    /// The palette as the `[theme]` table wrote it (S4): what the settings
    /// card rebuilds with one role replaced when a colour is cycled.
    colors: ThemeColors,
    /// The themes the picker offers (S2, issue #37, issue #54): the
    /// factory theme `velo` first, then the built-in catalogue sorted by
    /// name, then the files of the `themes/` folder that do not override a
    /// factory theme.
    theme_files: Vec<LoadedTheme>,
    /// The theme applied right now (S2): the name the config file carries,
    /// `velo` — the factory theme — when none is selected or the selected
    /// one is not on disk.
    theme_name: String,
    /// The theme picker while it is open (issue #37): its cursor and the
    /// palette in force when it opened, which `esc` restores.
    theme_picker: Option<ThemePicker>,
    /// The colour keys that win over the selected theme file (S2): the
    /// config's own `[theme]` colour keys at launch.
    theme_overrides: ThemeKeys,
    /// The theme notice still to say (S3): a selected name not on disk or a
    /// theme file that does not parse. Delivered into the status line once,
    /// on the first tick, then gone.
    theme_notice: Option<String>,
    /// The visualizer picker while it is open (issue #40): its cursor and
    /// the visualizer in force when it opened, which `esc` restores.
    visualizer_picker: Option<VisualizerPicker>,
    /// The visualizer the config file carries (issue #40, S5): what a
    /// preview moves around and what a write compares against, the way
    /// `theme_name` is the written theme while the palette previews.
    saved_visualizer: Visualizer,
    /// The view the full-screen visualizer (issue #40, S4) came from, while
    /// it is up: `V` and `esc` land back there.
    visualizer_fullscreen: Option<View>,
    /// The newest samples the tap produced (issue #40): the raw window the
    /// `scope` shape reads, oldest first, kept beside the spectrum's bands.
    waveform: Vec<f32>,
    /// When the session began (issue #40, S2): the elapsed time the falling
    /// peaks and the rain read, so the same input draws the same frame.
    started: std::time::Instant,
    /// Present while no library root is set (S4): the empty browser then
    /// says how to set one, and while its prompt is open every key edits it.
    library_setup: Option<LibrarySetup>,
    /// The seek prompt (issue #6), while it is open.
    seek_prompt: Option<SeekPrompt>,
    /// The config file a library chosen with `L` is saved to, and the file
    /// every persisted value is written to (S2).
    config_file: Option<PathBuf>,
    /// The recently played file (issue #5): loaded once from beside the
    /// config file, recorded into when a track begins.
    recent: Recent,
    /// The rows the `Recently played` screen stands on (issue #5, issue
    /// #33): the history's entries as the file holds them — tracks and
    /// stations alike — cloned in when the source opens. The library
    /// snapshot `tracks` holds stays where it was.
    recent_rows: Vec<crate::history::Entry>,
    /// The saved store (issue #7): what the user marked, in `library.toml`
    /// beside the config file, loaded with the config the way the history
    /// is. Holds both tables; only `favorite` is consumed today.
    favourites: crate::saved::Saved,
    /// The rows the `Favourites` screen stands on (issue #7): the store's
    /// favourites as the file holds them, cloned in when the source opens.
    favourite_rows: Vec<crate::saved::Favorite>,
    /// The rows the `Playlists` screen stands on (issue #4): the store's
    /// playlists — name and tracks — cloned in when the source opens. The
    /// track list of the open playlist is not here: its rows are read from
    /// the store as it stands, so a move is on screen the frame after it is
    /// saved.
    playlist_rows: Vec<crate::saved::Playlist>,
    /// The playlist-name prompt (issue #4), while it is open: saving the
    /// queue as a new playlist, or renaming one.
    playlist_prompt: Option<PlaylistPrompt>,
    /// The URL prompt (issue #42), while it is open: a one-off `http`/`https`
    /// URL to resolve and play, persisted nowhere.
    url_prompt: Option<UrlPrompt>,
    /// The save prompt (issue #45), while it is open: where the remote
    /// track's copy lands, prefilled `~/Downloads/<the file's own name>`.
    save_prompt: Option<SavePrompt>,
    /// The save currently running (issue #45), with the channel its worker
    /// answers on. One copy at a time: a second `ctrl+s` while one runs is
    /// refused, never a second thread over the same cache.
    pending_save: Option<SaveRun>,
    /// The append-to-playlist overlay (issue #4), while it is open.
    playlist_picker: Option<PlaylistPicker>,
    /// Turns queue entries into files just before the producer opens them
    /// (T12c): the remote rows' `subsonic://…` entries resolve through the
    /// subsonic cache. `None` (a local-only launch) means every entry already
    /// is a file.
    resolver: Option<Arc<dyn PathResolver>>,
    /// Fetches a remote row's cover into the covers cache (T12d), off the
    /// drawing path. `None` when no server is configured: remote covers then
    /// simply never arrive, like any other missing cover.
    covers: Option<Arc<dyn CoverArt>>,
    /// The remote cover still being fetched for `artwork_path`, with the
    /// channel its background thread answers on. A fetch never blocks a
    /// tick: the cover arrives whenever it arrives, or never.
    pending_cover: Option<(PathBuf, std::sync::mpsc::Receiver<Option<PathBuf>>)>,
    /// The scrobble tracker (T12d), fed once per tick; `None` when no
    /// scrobble sink was installed — a local-only or unconfigured launch.
    scrobbles: Option<ScrobbleTracker>,
    /// What the file already says (S2): the session it launched from and the
    /// volume it launched at. Every persisted write compares against this,
    /// so a value the file holds is never written back.
    saved: Session,
    saved_volume: u8,
    /// An accepted library root, or the one `I` refreshes, waiting for the
    /// shell to index it.
    library_root: Option<PathBuf>,
    refresh: Refresh,
    /// Whether playback is bit-perfect (T17): the volume is pinned to 100%
    /// and `ReplayGain` is skipped, both explained once in the status line.
    bit_perfect: bool,
    /// Whether the mono downmix is in force (issue #8, S2): a session
    /// setting, toggled by `M`, never persisted. It travels to the producer
    /// through the player's shared cell.
    mono: bool,
    /// Whether the bit-perfect volume pinning has been explained already, so
    /// the status line says it once, not once per volume key press.
    bit_perfect_volume_said: bool,
    volume: u8,
    state: PlayState,
    player: Option<Player>,
    /// The stream handed over mid-crossfade (issue #65): it keeps sounding
    /// while the new one rises, its fade-out armed once the new stream has
    /// started, and it stops when the fade has fully fallen. `None` outside
    /// a handover.
    crossfading: Option<Crossfading>,
    status: String,
    /// The analyzer the spectrum draws (T5): fed from the player's tap on
    /// every tick, shown or not, so the tap never holds stale audio.
    spectrum: Spectrum,
    /// The tap's samples between draining and analysis, reused per tick.
    heard: Vec<f32>,
    /// How many bands the last frame had room for.
    spectrum_bands: std::cell::Cell<usize>,
    /// The credential warning still to say (a group- or world-readable
    /// `subsonic.key`, T12c's debt): delivered into the status line once, on
    /// the first tick, then gone — a warning that repeats is noise, and one
    /// that only surfaces inside a skip reason is no warning at all.
    subsonic_warning: Option<String>,
    /// The one-shot update check (issue #69, slice 1): its thread asks off
    /// the event loop; the tick polls the one answer and drops the handle.
    update_check: Option<crate::update::UpdateCheck>,
    /// The newer version a finished check found (issue #69): `None` means
    /// nothing found yet — and says nothing about a failed check, which is
    /// never said at all.
    update_latest: Option<String>,
    /// The media keys (issue #12), when the launch could start them: the OS
    /// client lives on its own thread; the tick drains its commands and
    /// publishes what is playing. `None` — headless refusal, no session
    /// bus, a client that said no — is a session without them, never a
    /// session that failed (S6).
    media: Option<crate::media_keys::MediaKeys>,
    /// What the OS was last told is playing (issue #12, S5): a tick that
    /// finds the same answer sends nothing, so a still session is silent.
    media_published: Option<crate::media_keys::MediaState>,
    /// What the config file already said about low power (issue #11, S1):
    /// a `P` press writes only when it moves away from it, the way `s`
    /// compares against the session.
    saved_low_power: bool,
    /// The output device this session opens (issue #44, S3): the config's
    /// `[audio] device` at launch, the picker's choice afterwards. `None`
    /// is the system's default output. Every player this session builds
    /// opens it.
    audio_device: Option<String>,
    /// What the config file already said about the device (issue #44, S3):
    /// a switch writes only when it moves away from it, the way the
    /// visualizer writes only when it changed.
    saved_audio_device: Option<String>,
    /// The device picker while it is open (issue #44, S1): the machine's
    /// outputs, the one in use marked; `enter` switches, `esc` closes
    /// without changing anything.
    device_picker: Option<DevicePicker>,
    /// The machine's audio outputs, as the device picker lists them: a
    /// function so the tests can install a fixture list — no test
    /// enumerates real devices, the way no test opens a browser.
    device_lister: Box<dyn Fn() -> DeviceList>,
}

impl<'lib> App<'lib> {
    /// An interface over the given files, selection at the top.
    #[must_use]
    #[expect(
        clippy::too_many_lines,
        reason = "the constructor is one flat list of every field a session\n    starts with; splitting it would scatter that story"
    )]
    pub fn new(tracks: Vec<PathBuf>) -> App<'static> {
        App {
            position: Position::Files,
            sources: sources::Sources::default(),
            radio_from: View::Library,
            url_opener: open_url,
            tab_origin: View::Sources,
            song_titles: HashMap::new(),
            expansion_cache: HashMap::new(),
            argv: tracks,
            library: None,
            browse_by: BrowseBy::Artists,
            artists: Vec::new(),
            albums: Vec::new(),
            genres: Vec::new(),
            tracks: Vec::new(),
            played: Vec::new(),
            selected: 0,
            queue: Vec::new(),
            playing: None,
            view: View::Library,
            queue_selected: 0,
            shared: None,
            seen_area: std::cell::Cell::new(Rect::new(0, 0, 0, 0)),
            art_explained: false,
            replay_gain: ReplayGainMode::default(),
            theme: Theme::default(),
            ui: UiParts::default(),
            shuffle: false,
            unshuffled: Vec::new(),
            repeat: Repeat::Off,
            search: None,
            search_pool: Vec::new(),
            search_haystacks: Vec::new(),
            search_results: Vec::new(),
            search_selected: 0,
            picker: None,
            artwork: None,
            artwork_path: None,
            protocol: RefCell::new(None),
            help: None,
            settings: None,
            info: None,
            radio: None,
            transport_fixture: None,
            pending_resolve: None,
            radio_wire: None,
            radio_refusal: None,
            colors: ThemeColors::default(),
            theme_files: theme::builtins::catalogue(),
            theme_name: crate::config::DEFAULT_THEME_NAME.to_owned(),
            theme_picker: None,
            theme_overrides: ThemeKeys::default(),
            theme_notice: None,
            visualizer_picker: None,
            saved_visualizer: Visualizer::default(),
            visualizer_fullscreen: None,
            waveform: Vec::new(),
            started: std::time::Instant::now(),
            seek_prompt: None,
            library_setup: None,
            config_file: None,
            recent: Recent::default(),
            recent_rows: Vec::new(),
            favourites: crate::saved::Saved::default(),
            favourite_rows: Vec::new(),
            playlist_rows: Vec::new(),
            playlist_prompt: None,
            url_prompt: None,
            playlist_picker: None,
            resolver: None,
            covers: None,
            pending_cover: None,
            save_prompt: None,
            pending_save: None,
            scrobbles: None,
            subsonic_warning: None,
            update_check: None,
            update_latest: None,
            media: None,
            media_published: None,
            saved_low_power: false,
            audio_device: None,
            saved_audio_device: None,
            device_picker: None,
            device_lister: Box::new(crate::audio::output::output_devices),
            saved: Session::default(),
            saved_volume: crate::config::DEFAULT_VOLUME,
            library_root: None,
            refresh: Refresh::default(),
            bit_perfect: false,
            bit_perfect_volume_said: false,
            mono: false,
            volume: 100,
            state: PlayState::Stopped,
            player: None,
            crossfading: None,
            status: String::new(),
            spectrum: Spectrum::new(),
            heard: Vec::new(),
            spectrum_bands: std::cell::Cell::new(0),
        }
    }

    /// An interface browsing the library index. The artists snapshot is read
    /// here, once; deeper levels are read when the position changes.
    #[must_use]
    #[expect(
        clippy::too_many_lines,
        reason = "the constructor is one flat list of every field a session\n    starts with; splitting it would scatter that story"
    )]
    pub fn browse(library: Option<&'lib Library>) -> Self {
        let mut app = Self {
            position: Position::Artists,
            sources: sources::Sources::default(),
            radio_from: View::Library,
            url_opener: open_url,
            tab_origin: View::Sources,
            song_titles: HashMap::new(),
            expansion_cache: HashMap::new(),
            argv: Vec::new(),
            library,
            browse_by: BrowseBy::Artists,
            artists: Vec::new(),
            albums: Vec::new(),
            genres: Vec::new(),
            tracks: Vec::new(),
            played: Vec::new(),
            selected: 0,
            queue: Vec::new(),
            playing: None,
            view: View::Library,
            queue_selected: 0,
            shared: None,
            seen_area: std::cell::Cell::new(Rect::new(0, 0, 0, 0)),
            art_explained: false,
            replay_gain: ReplayGainMode::default(),
            theme: Theme::default(),
            ui: UiParts::default(),
            shuffle: false,
            unshuffled: Vec::new(),
            repeat: Repeat::Off,
            search: None,
            search_pool: Vec::new(),
            search_haystacks: Vec::new(),
            search_results: Vec::new(),
            search_selected: 0,
            picker: None,
            artwork: None,
            artwork_path: None,
            protocol: RefCell::new(None),
            help: None,
            settings: None,
            info: None,
            radio: None,
            transport_fixture: None,
            pending_resolve: None,
            radio_wire: None,
            radio_refusal: None,
            colors: ThemeColors::default(),
            theme_files: theme::builtins::catalogue(),
            theme_name: crate::config::DEFAULT_THEME_NAME.to_owned(),
            theme_picker: None,
            theme_overrides: ThemeKeys::default(),
            theme_notice: None,
            visualizer_picker: None,
            saved_visualizer: Visualizer::default(),
            visualizer_fullscreen: None,
            waveform: Vec::new(),
            started: std::time::Instant::now(),
            seek_prompt: None,
            library_setup: None,
            config_file: None,
            recent: Recent::default(),
            recent_rows: Vec::new(),
            favourites: crate::saved::Saved::default(),
            favourite_rows: Vec::new(),
            playlist_rows: Vec::new(),
            playlist_prompt: None,
            url_prompt: None,
            playlist_picker: None,
            resolver: None,
            covers: None,
            pending_cover: None,
            save_prompt: None,
            pending_save: None,
            scrobbles: None,
            subsonic_warning: None,
            update_check: None,
            update_latest: None,
            media: None,
            media_published: None,
            saved_low_power: false,
            audio_device: None,
            saved_audio_device: None,
            device_picker: None,
            device_lister: Box::new(crate::audio::output::output_devices),
            saved: Session::default(),
            saved_volume: crate::config::DEFAULT_VOLUME,
            library_root: None,
            refresh: Refresh::default(),
            bit_perfect: false,
            bit_perfect_volume_said: false,
            mono: false,
            volume: 100,
            state: PlayState::Stopped,
            player: None,
            crossfading: None,
            status: String::new(),
            spectrum: Spectrum::new(),
            heard: Vec::new(),
            spectrum_bands: std::cell::Cell::new(0),
        };
        app.reload();
        app
    }

    /// The index of the selected row.
    #[must_use]
    pub const fn selected(&self) -> usize {
        self.selected
    }

    /// Which list the keys act on.
    #[must_use]
    pub const fn view(&self) -> View {
        self.view
    }

    /// Opens the sources screen (issue #3): the first screen, one level
    /// above the library and the radio. The flat command-line list has no
    /// source to choose from, so a file launch stays on its list.
    pub fn open_sources(&mut self) {
        if !matches!(self.position, Position::Files) {
            self.view = View::Sources;
        }
    }

    /// Whether the radio view is open (T6a).
    #[must_use]
    pub const fn radio_open(&self) -> bool {
        self.radio.is_some()
    }

    /// Whether the screen is the library itself, rather than one of the
    /// lists that borrow its view: `Recent`, `Favourites`, `Playlists` and a
    /// flat command-line list are not the index, so the library's tabs and
    /// its `1`–`4` keys are none of their business (issue #49).
    fn in_library(&self) -> bool {
        matches!(
            self.position,
            Position::Artists
                | Position::Albums { .. }
                | Position::Tracks { .. }
                | Position::AllAlbums
                | Position::AllTracks
                | Position::Genres
                | Position::Genre { .. }
        )
    }

    /// The rows of the sources screen, in the order it shows them.
    #[must_use]
    pub fn sources_labels(&self) -> Vec<String> {
        self.sources.labels()
    }

    /// The row of the sources screen the cursor is on.
    #[must_use]
    pub const fn sources_selected(&self) -> usize {
        self.sources.selected()
    }

    /// The selected row of the queue view.
    #[must_use]
    pub const fn queue_selected(&self) -> usize {
        self.queue_selected
    }

    /// How `ReplayGain` is applied, as the config resolved it. Bit-perfect
    /// playback forces `off`: the gain scales samples, which is what the mode
    /// refuses (T17), so the interface must not claim it either.
    pub fn set_replay_gain(&mut self, mode: ReplayGainMode) {
        self.replay_gain = if self.bit_perfect {
            ReplayGainMode::Off
        } else {
            mode
        };
    }

    /// The `ReplayGain` mode in force, as the transport row shows it.
    #[must_use]
    pub const fn replay_gain(&self) -> ReplayGainMode {
        self.replay_gain
    }

    /// Turns bit-perfect playback on or off (T17). On, it pins the volume to
    /// 100% — a setting of `70` is refused, and the status line says why once
    /// — and skips `ReplayGain`.
    pub fn set_bit_perfect(&mut self, bit_perfect: bool) {
        self.bit_perfect = bit_perfect;
        if bit_perfect {
            self.set_replay_gain(self.replay_gain);
            self.set_volume(100);
        }
    }

    /// Starts the session in mono (issue #8, S2), from `--mono`. The launch
    /// has already refused the combination with bit-perfect, so this never
    /// has to negotiate with it.
    pub fn set_mono(&mut self, mono: bool) {
        self.mono = mono;
    }

    /// Whether the mono downmix is in force.
    #[must_use]
    pub const fn mono(&self) -> bool {
        self.mono
    }

    /// The palette the interface draws with, from the config's `[theme]` (S4):
    /// `accent_bright` stays derived from the accent. The palette as the file
    /// wrote it is kept too, so the settings card can rebuild it with one
    /// role replaced (S7).
    pub fn set_theme(&mut self, colors: ThemeColors) {
        self.colors = colors;
        self.theme = colors.into();
    }

    /// Wires the theme library in the way the launch paths do (S1–S4, issue
    /// #10): the themes the picker offers, the theme applied right now (the
    /// launch palette is already in [`App::set_theme`]), the colour keys
    /// that win over the selected theme file, and the notice — if any — the
    /// first tick says once.
    pub fn set_theme_library(
        &mut self,
        selection: crate::config::ThemeSelection,
        overrides: ThemeKeys,
    ) {
        self.set_theme(selection.palette);
        self.theme_files = selection.themes;
        self.theme_name = selection.applied;
        self.theme_overrides = overrides;
        self.theme_notice = (!selection.notices.is_empty()).then(|| selection.notices.join("; "));
    }

    /// The theme applied right now, as the config file names it (S2).
    #[must_use]
    pub fn theme_name(&self) -> &str {
        &self.theme_name
    }

    /// Whether the theme picker is open (issue #37): it owns every key
    /// while it is, the way the settings card does.
    #[must_use]
    pub const fn theme_picker_open(&self) -> bool {
        self.theme_picker.is_some()
    }

    /// The row the picker's cursor is on (issue #37).
    #[must_use]
    pub const fn theme_picker_cursor(&self) -> usize {
        match &self.theme_picker {
            Some(picker) => picker.cursor,
            None => 0,
        }
    }

    /// Whether the session's transparent-background switch is on (issue
    /// #61): the same `ThemeKeys` field the launch and the picker's
    /// preview resolve through, never a second rule of its own.
    #[must_use]
    pub fn background_transparent(&self) -> bool {
        self.theme_overrides.transparent_background == Some(true)
    }

    /// The picker's rows, in order (issue #37, S2; issue #54): the factory
    /// theme `velo` first, then the built-ins sorted, then the folder files
    /// that override nothing.
    #[must_use]
    pub fn theme_picker_names(&self) -> Vec<String> {
        self.theme_files
            .iter()
            .map(|theme| theme.name.clone())
            .collect()
    }

    /// The visualizer in force (issue #40), as the config resolves it.
    #[must_use]
    pub const fn visualizer(&self) -> Visualizer {
        self.ui.visualizer
    }

    /// Whether the visualizer picker is open (issue #40): it owns every key
    /// while it is, the way the theme picker does.
    #[must_use]
    pub const fn visualizer_picker_open(&self) -> bool {
        self.visualizer_picker.is_some()
    }

    /// The row the visualizer picker's cursor is on (issue #40).
    #[must_use]
    pub const fn visualizer_picker_cursor(&self) -> usize {
        match &self.visualizer_picker {
            Some(picker) => picker.cursor,
            None => 0,
        }
    }

    /// The picker's rows, in order (issue #40, S3): `none` first, then the
    /// eight, in their stable order.
    #[must_use]
    pub fn visualizer_picker_names(&self) -> Vec<String> {
        Visualizer::ALL
            .iter()
            .map(|visualizer| visualizer.name().to_owned())
            .collect()
    }

    /// Whether the visualizer is up over the whole body (issue #40, S4).
    #[must_use]
    pub const fn visualizer_fullscreen(&self) -> bool {
        self.visualizer_fullscreen.is_some()
    }

    /// Whether the device picker is open (issue #44): it owns every key
    /// while it is, the way the theme and visualizer pickers do.
    #[must_use]
    pub const fn device_picker_open(&self) -> bool {
        self.device_picker.is_some()
    }

    /// The row the device picker's cursor is on (issue #44, S1).
    #[must_use]
    pub const fn device_picker_cursor(&self) -> usize {
        match &self.device_picker {
            Some(picker) => picker.cursor,
            None => 0,
        }
    }

    /// The picker's rows, in the order the machine's outputs were listed
    /// (issue #44, S1).
    #[must_use]
    pub fn device_picker_names(&self) -> Vec<String> {
        match &self.device_picker {
            Some(picker) => picker.names.clone(),
            None => Vec::new(),
        }
    }

    /// The device the picker marks as in use (issue #44, S1): the player's
    /// own while one plays, else the session's choice, else the default.
    #[must_use]
    pub fn device_picker_in_use(&self) -> Option<String> {
        self.device_picker
            .as_ref()
            .map(|picker| picker.in_use.clone())
    }

    /// The output device the session opens (issue #44, S3); `None` is the
    /// system's default output.
    #[must_use]
    pub fn audio_device(&self) -> Option<&str> {
        self.audio_device.as_deref()
    }

    /// The output device every player this session builds opens (issue
    /// #44, S3): the config's `[audio] device` at launch. Also records
    /// what the file already said, so a switch writes only when it moved
    /// away from it, the way [`App::set_ui`] records low power.
    pub fn set_audio_device(&mut self, device: Option<String>) {
        self.saved_audio_device.clone_from(&device);
        self.audio_device = device;
    }

    /// Installs the device list the picker offers (issue #44, S1): a
    /// function so the tests can stand in for the machine's outputs, the
    /// way [`App::set_url_opener`] stands in for the browser.
    pub fn set_device_lister(&mut self, lister: impl Fn() -> DeviceList + 'static) {
        self.device_lister = Box::new(lister);
    }

    /// The palette as the `[theme]` table wrote it (S4): the colours the
    /// settings card rebuilds and the tests observe.
    #[must_use]
    pub const fn theme_colors(&self) -> ThemeColors {
        self.colors
    }

    /// Which parts of the interface are drawn, from the config's `[ui]` (S5):
    /// the switches the layout reads, everything on by default. Also records
    /// what the file already said about low power (issue #11, S1), so a `P`
    /// press writes only when it moves away from it.
    pub fn set_ui(&mut self, ui: UiParts) {
        self.saved_low_power = ui.low_power;
        self.saved_visualizer = ui.visualizer;
        self.ui = ui;
    }

    /// Starts the media keys (issue #12, S6): the OS client lives on its
    /// own thread; the tick drains its commands and publishes what is
    /// playing. A launch without one — headless, no session bus, a client
    /// that refused — is a session without them, at most one status line,
    /// never a wait.
    pub fn set_media_keys(&mut self, media: crate::media_keys::MediaKeys) {
        self.media = Some(media);
    }

    /// Whether low power is in force (issue #11): a quarter of the redraw
    /// cadence and no spectrum, with playback and the layout otherwise
    /// unchanged.
    #[must_use]
    pub const fn low_power(&self) -> bool {
        self.ui.low_power
    }

    /// Applies one media-key command (issue #12, S4): the same action the
    /// keyboard key answers, through the same [`App::apply`]. Returns the
    /// action, the way [`App::on_key`] does, so the tests can see the
    /// routing without a player.
    #[doc(hidden)]
    pub fn media_command(&mut self, command: crate::media_keys::MediaCommand) -> Action {
        let action = crate::media_keys::action_of(command);
        self.apply(action);
        action
    }

    /// Comes back to what the config file says the last session stood at
    /// (S3): the shuffle and repeat it saved, then — in the browser — the
    /// library view and the node that was open. The recorded tab is retired
    /// (S14): it still parses, and it is never applied — the launch opens
    /// `Sources` and the library entry lands on the library (issue #27).
    /// Call it after [`App::set_volume`], as the launch paths do: the volume
    /// on the app then is remembered as what the file says, so restoring
    /// never writes. A node the index no longer has falls back silently to
    /// the level above.
    pub fn set_session(&mut self, session: Session) {
        // Every one of these came from the file, and the change hooks below
        // compare against `saved`: nothing here is ever written back.
        let Session {
            shuffle,
            repeat,
            view,
            browse_by,
            node,
        } = session;
        self.saved = Session {
            shuffle,
            repeat,
            view,
            browse_by,
            node: node.clone(),
        };
        self.saved_volume = self.volume;
        self.shuffle = shuffle;
        self.repeat = repeat;
        // The library view and the node belong to the browser; a flat
        // command-line list has neither to come back to.
        if matches!(self.position, Position::Files) {
            return;
        }
        self.browse_view(browse_by);
        if let Some(node) = &node {
            self.reopen_node(browse_by, node);
        }
    }

    /// Re-opens the saved node inside `by`, level by level, reading each
    /// child list from the index. A level the index no longer has — a gone
    /// artist, album or genre, or an empty list where one should be — stops
    /// the descent right there, silently: the level above stays on screen.
    fn reopen_node(&mut self, by: BrowseBy, node: &OpenNode) {
        let Some(library) = self.library else {
            return;
        };
        match by {
            BrowseBy::Artists => {
                let Some(artist) = node.artist.as_deref() else {
                    return;
                };
                let Ok(albums) = library.albums_of(artist) else {
                    return;
                };
                if albums.is_empty() {
                    return;
                }
                self.position = Position::Albums {
                    artist: artist.to_owned(),
                };
                self.albums = albums;
                self.selected = 0;
                let Some(album) = node.album.as_deref() else {
                    return;
                };
                let Ok(tracks) = library.tracks_of(artist, album) else {
                    return;
                };
                if tracks.is_empty() {
                    return;
                }
                self.position = Position::Tracks {
                    artist: artist.to_owned(),
                    album: album.to_owned(),
                };
                self.tracks = tracks;
                self.selected = 0;
            }
            BrowseBy::Albums | BrowseBy::Tracks => {
                let (Some(artist), Some(album)) = (node.artist.as_deref(), node.album.as_deref())
                else {
                    return;
                };
                let Ok(tracks) = library.tracks_of(artist, album) else {
                    return;
                };
                if tracks.is_empty() {
                    return;
                }
                self.position = Position::Tracks {
                    artist: artist.to_owned(),
                    album: album.to_owned(),
                };
                self.tracks = tracks;
                self.selected = 0;
            }
            BrowseBy::Genres => {
                let Some(genre) = node.genre.as_deref() else {
                    return;
                };
                let Ok(tracks) = library.tracks_of_genre(genre) else {
                    return;
                };
                if tracks.is_empty() {
                    return;
                }
                self.position = Position::Genre {
                    genre: genre.to_owned(),
                };
                self.tracks = tracks;
                self.selected = 0;
            }
        }
    }

    /// Writes one key of the user's config file (S2). Renders nothing; a
    /// failed write becomes the status line and the app keeps running.
    /// Without a config file there is nothing to write and nothing to say.
    fn remember(&mut self, section: Option<&str>, key: &str, value: &str) {
        let Some(path) = &self.config_file else {
            return;
        };
        if let Err(error) = crate::config::set_value(path, section, key, value) {
            self.status = format!("velo: {error}");
        }
    }

    /// Removes one key of the user's config file, with the same silence and
    /// the same error rule as [`App::remember`].
    fn forget(&mut self, section: Option<&str>, key: &str) {
        let Some(path) = &self.config_file else {
            return;
        };
        if let Err(error) = crate::config::remove_value(path, section, key) {
            self.status = format!("velo: {error}");
        }
    }

    /// A string as the TOML literal `set_value` writes after the `=`.
    fn toml_string(value: &str) -> String {
        toml::Value::String(value.to_owned()).to_string()
    }

    /// Opens the radio view over its wiring (T6a, T6b): the store and the
    /// country load at once (one small file read, no network), the directory
    /// follows lazily as the sections are entered.
    fn open_radio(&mut self) -> Action {
        let wire = self.radio_wire.get_or_insert_with(|| {
            let store_path = self
                .config_file
                .as_ref()
                .map(|config| config.with_file_name("radio.toml"));
            Box::new({
                // The host the config names, else the community's
                // round-robin — decided here and nowhere else (issue #14).
                // The cliamp switch is decided here too (issue #48, S1):
                // shown unless the config says `false`.
                let host = crate::config::radio_host_from(self.config_file.as_deref());
                let cliamp = crate::config::radio_cliamp_from(self.config_file.as_deref());
                radio_data::RadioData::open(store_path, &host, cliamp)
            })
        });
        self.radio_from = self.view;
        self.radio = Some(wire.view());
        // The playing fact is handed over at birth too (S15, issue #32):
        // the view must not wait for the next tick to learn what plays —
        // a station already sounding when the radio opens is marked at
        // once.
        self.radio_poll();
        Action::None
    }

    /// Opens the source the sources screen's cursor is on (issue #3): the
    /// library always lands on the library (issue #27) — the saved tab is
    /// the queue's half of the round trip, not the library's address — and
    /// with no library configured, the prompt asks for its path, the same
    /// card `o` (or `ctrl+l`) opens.
    fn open_source(&mut self) -> Action {
        match self.sources.current() {
            sources::Source::Library => {
                self.view = View::Library;
                if self.library.is_none()
                    && self.refresh.root.is_none()
                    && !self.library_prompt_open()
                {
                    self.ask_for_library(self.config_file.clone());
                }
                Action::None
            }
            sources::Source::Radio => self.open_radio(),
            sources::Source::RecentlyPlayed => {
                self.open_recent();
                Action::None
            }
            sources::Source::Favourites => {
                self.open_favourites();
                Action::None
            }
            sources::Source::Playlists => {
                self.open_playlists();
                Action::None
            }
        }
    }

    /// Opens the `Favourites` source (issue #7): the saved store's favourites,
    /// in the order they were marked, as the rows the screen stands on —
    /// cloned in here, never re-read while the screen is open. This is not a
    /// level of the library and never becomes a `BrowseBy`: nothing here is
    /// persisted as a node, and the node the file holds is left alone.
    fn open_favourites(&mut self) {
        self.favourite_rows = self.favourites.favorite.clone();
        self.view = View::Library;
        self.position = Position::Favourites;
        self.selected = 0;
        self.queue_selected = 0;
    }

    /// The rows the `Favourites` screen stands on (issue #7): the store's
    /// favourites as the file holds them, in the order they were marked.
    /// Empty outside that screen.
    #[must_use]
    pub fn favourite_rows(&self) -> &[crate::saved::Favorite] {
        &self.favourite_rows
    }

    /// Opens the `Playlists` source (issue #4): the saved store's playlists,
    /// one row per name, in the order the file holds them — cloned in here,
    /// never re-read while the screen is open. This is not a level of the
    /// library and never becomes a `BrowseBy`: nothing here is persisted as
    /// a node.
    fn open_playlists(&mut self) {
        self.playlist_rows = self.favourites.playlist.clone();
        self.view = View::Library;
        self.position = Position::Playlists;
        self.selected = 0;
        self.queue_selected = 0;
    }

    /// The rows the `Playlists` screen stands on (issue #4): the store's
    /// playlists as the file holds them, in the order they were saved.
    /// Empty outside that screen.
    #[must_use]
    pub fn playlist_rows(&self) -> &[crate::saved::Playlist] {
        &self.playlist_rows
    }

    /// Opens one playlist's track list (issue #4): its name is the store's
    /// identity, and the rows are read from the store as it stands, so a
    /// move or an append is on screen the frame after it is saved.
    fn open_playlist(&mut self, name: String) {
        self.position = Position::Playlist { name };
        self.selected = 0;
    }

    /// The tracks of the playlist on screen (issue #4): empty when no
    /// playlist is open or the name is no longer in the store.
    fn open_playlist_tracks(&self) -> &[String] {
        match &self.position {
            Position::Playlist { name } => self
                .favourites
                .playlist(name)
                .map(|playlist| playlist.track.as_slice())
                .unwrap_or_default(),
            _ => &[],
        }
    }

    /// `f` on a track row of the library views (issue #7): marks the row's
    /// path — exactly the path the row carries, a `subsonic://` path stays
    /// itself — or unmarks it again, keeping `artist`, `album` and `title`
    /// copied from the row when it has them: there is no lookup by path in
    /// the index, so the song must still be nameable from the file alone.
    /// The store is written immediately, best effort: a store that cannot be
    /// written is lost, not fatal — the same courtesy rule `history_tick`
    /// follows. An absent field of the row (a path the index cannot
    /// describe) stays absent, never an empty string.
    fn toggle_favourite(&mut self) {
        let Some(track) = self.tracks.get(self.selected) else {
            return;
        };
        let known = |text: &str| (!text.is_empty()).then(|| text.to_owned());
        let favorite = crate::saved::Favorite {
            path: track.path.to_string_lossy().into_owned(),
            artist: known(&track.artist),
            album: known(&track.album),
            title: known(&track.title),
        };
        self.favourites.toggle_favorite(favorite);
        self.save_marks();
    }

    /// Writes the saved store beside the config file at once, best effort
    /// (issue #7): a store that cannot be written is lost, not fatal.
    fn save_marks(&mut self) {
        let Some(config_file) = self.config_file.clone() else {
            return;
        };
        let file = crate::saved::path_beside(&config_file);
        let _ = self.favourites.save(&file);
    }

    /// `S` in the queue view (issue #4, S3): opens the name prompt when the
    /// queue holds something to save; an empty queue is refused on the
    /// status line, and so is a queue that holds a station — a playlist's
    /// `[[playlist]] track` entries are paths, and a station has none.
    fn ask_to_save_queue_as_playlist(&mut self) {
        if self.queue.is_empty() {
            self.status = String::from("velo: the queue is empty — nothing to save");
            return;
        }
        if self.queue.iter().any(|entry| entry.as_path().is_none()) {
            self.status =
                String::from("velo: the queue holds a station — a playlist carries tracks only");
            return;
        }
        self.playlist_prompt = Some(PlaylistPrompt {
            input: String::new(),
            error: None,
            rename: None,
        });
    }

    /// `S` on a library track row (issue #4, S3): opens the append overlay
    /// listing the saved names, the row's path captured exactly as the row
    /// carries it. No saved names, nothing to choose from: the status line
    /// says so.
    fn ask_to_append_to_playlist(&mut self) {
        let names: Vec<String> = self
            .favourites
            .playlist
            .iter()
            .map(|playlist| playlist.name.clone())
            .collect();
        if names.is_empty() {
            self.status = String::from("velo: no playlists yet — save the queue with S");
            return;
        }
        let Some(track) = self.tracks.get(self.selected) else {
            return;
        };
        self.playlist_picker = Some(PlaylistPicker {
            track: track.path.to_string_lossy().into_owned(),
            names,
            selected: 0,
        });
    }

    /// `e` on a playlist row of the names list (issue #4, S5): opens the
    /// rename prompt with the name as it stands, so the edit starts from
    /// what is already there. `esc` cancels; nothing is written until
    /// `enter`.
    fn ask_to_rename_playlist(&mut self) {
        let Some(entry) = self.playlist_rows.get(self.selected) else {
            return;
        };
        self.playlist_prompt = Some(PlaylistPrompt {
            input: entry.name.clone(),
            error: None,
            rename: Some(entry.name.clone()),
        });
    }

    /// `x` on a playlist row of the names list (issue #4, S5): deletes it
    /// from the store and says so in the status line. The store's write is
    /// best effort: a file that cannot be written loses the change, not the
    /// session — the same courtesy rule every store write follows.
    fn delete_playlist(&mut self) {
        let Some(entry) = self.playlist_rows.get(self.selected) else {
            return;
        };
        let name = entry.name.clone();
        if self.favourites.delete_playlist(&name) {
            self.playlist_rows.remove(self.selected);
            self.selected = self
                .selected
                .min(self.playlist_rows.len().saturating_sub(1));
            self.save_marks();
            self.status = format!("velo: deleted playlist {name}");
        }
    }

    /// `J`/`K` inside a playlist's track list (issue #4, S4): the selected
    /// track swaps with its neighbour and the store is written at once — the
    /// move survives the process like the list does. A refused move (at
    /// either end) changes nothing; the store's write is best effort.
    fn move_in_playlist(&mut self, delta: i64) {
        let name = match &self.position {
            Position::Playlist { name } => name.clone(),
            _ => return,
        };
        if let Some(selected) = self
            .favourites
            .move_in_playlist(&name, self.selected, delta)
        {
            self.selected = selected;
            self.save_marks();
        }
    }

    /// The keys while the playlist-name prompt is open (issue #4): printable
    /// keys type into it, `backspace` deletes, `enter` answers, `esc` closes
    /// it. Nothing reaches the lists underneath.
    fn on_playlist_prompt_key(&mut self, code: ratatui::crossterm::event::KeyCode) -> Action {
        use ratatui::crossterm::event::KeyCode;

        match code {
            KeyCode::Esc => {
                self.playlist_prompt = None;
            }
            KeyCode::Backspace => {
                if let Some(prompt) = self.playlist_prompt.as_mut() {
                    prompt.input.pop();
                    prompt.error = None;
                }
            }
            KeyCode::Char(character) => {
                if let Some(prompt) = self.playlist_prompt.as_mut() {
                    prompt.input.push(character);
                    prompt.error = None;
                }
            }
            KeyCode::Enter => self.answer_playlist_prompt(),
            _ => return Action::None,
        }
        Action::None
    }

    /// Answers the playlist-name prompt (issue #4): the typed name saves the
    /// queue as a new playlist, or renames the one the prompt carries. A
    /// taken name is refused — the prompt stays open with the reason under
    /// what was typed, the way a refused seek stays (S1: never a silent
    /// merge, never a duplicate) — and an empty answer is refused the same
    /// way. The names list, when it is on screen, follows the store.
    fn answer_playlist_prompt(&mut self) {
        let (name, rename) = match &self.playlist_prompt {
            Some(prompt) => (prompt.input.clone(), prompt.rename.clone()),
            None => return,
        };
        if name.is_empty() {
            if let Some(prompt) = &mut self.playlist_prompt {
                prompt.error = Some(String::from("a name is needed"));
            }
            return;
        }
        let outcome = match rename {
            // The queue's file entries become the playlist's tracks, in play
            // order; the ask already refused a station, so there is nothing
            // to skip here.
            None => {
                let tracks: Vec<String> = self
                    .queue
                    .iter()
                    .filter_map(|entry| entry.as_path())
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect();
                let count = tracks.len();
                self.favourites.save_playlist(&name, tracks).map(|()| {
                    format!(
                        "velo: saved playlist {name}, {}",
                        plural(count, "track", "tracks")
                    )
                })
            }
            Some(original) => self
                .favourites
                .rename_playlist(&original, &name)
                .map(|()| format!("velo: renamed playlist {original} to {name}")),
        };
        match outcome {
            Ok(message) => {
                self.playlist_prompt = None;
                self.status = message;
                self.playlist_rows = self.favourites.playlist.clone();
                self.save_marks();
            }
            Err(crate::saved::DuplicateName) => {
                if let Some(prompt) = &mut self.playlist_prompt {
                    prompt.error = Some(format!("a playlist called {name} already exists"));
                }
            }
        }
    }

    /// The keys while the append overlay is open (issue #4): `j`/`k` move
    /// between the saved names, `enter` appends the captured track to the
    /// chosen one, `esc` cancels. Nothing reaches the lists underneath.
    fn on_playlist_picker_key(&mut self, code: ratatui::crossterm::event::KeyCode) -> Action {
        use ratatui::crossterm::event::KeyCode;

        let Some(picker) = self.playlist_picker.as_mut() else {
            return Action::None;
        };
        match code {
            KeyCode::Esc => self.playlist_picker = None,
            KeyCode::Char('j') | KeyCode::Down => {
                picker.selected = (picker.selected + 1).min(picker.names.len() - 1);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                picker.selected = picker.selected.saturating_sub(1);
            }
            KeyCode::Char('g') => picker.selected = 0,
            KeyCode::Char('G') => picker.selected = picker.names.len() - 1,
            KeyCode::Enter => {
                let track = picker.track.clone();
                let name = picker.names[picker.selected].clone();
                self.playlist_picker = None;
                self.favourites.append_to_playlist(&name, &track);
                self.save_marks();
                self.status = format!("velo: added to playlist {name}");
            }
            _ => return Action::None,
        }
        Action::None
    }

    /// Opens the `Recently played` source (issue #5): the history's entries,
    /// newest first, as the rows the screen stands on. The rows are the
    /// file's entries themselves (issue #33), tracks and stations alike —
    /// cloned in here, never re-read while the screen is open — and a queue
    /// entry built from a file row carries the history's path verbatim, so a
    /// `subsonic://…` row resolves through the cache at play time. This is
    /// not a level of the library and never becomes a `BrowseBy`: nothing
    /// here is persisted, and the node the file holds is left alone.
    fn open_recent(&mut self) {
        self.recent_rows = self.recent.entries.entry.clone();
        self.view = View::Library;
        self.position = Position::Recent;
        self.selected = 0;
        self.queue_selected = 0;
    }

    /// The node the current position names, in the file's terms (S3).
    fn open_node(&self) -> OpenNode {
        match &self.position {
            Position::Albums { artist } => OpenNode {
                artist: Some(artist.clone()),
                ..OpenNode::default()
            },
            Position::Tracks { artist, album } => OpenNode {
                artist: Some(artist.clone()),
                album: Some(album.clone()),
                ..OpenNode::default()
            },
            Position::Genre { genre } => OpenNode {
                genre: Some(genre.clone()),
                ..OpenNode::default()
            },
            _ => OpenNode::default(),
        }
    }

    /// Saves the open node, one key at a time: a key the position still
    /// names is written when it changed, and a key it left behind is
    /// removed, so the file never keeps a node the user has walked out of
    /// (S3).
    fn remember_node(&mut self) {
        let node = self.open_node();
        let saved = if node == OpenNode::default() {
            None
        } else {
            Some(node.clone())
        };
        if saved == self.saved.node {
            return;
        }
        let was = self.saved.node.take().unwrap_or_default();
        self.saved.node = saved;
        for (key, now, before) in [
            ("artist", node.artist.clone(), was.artist),
            ("album", node.album.clone(), was.album),
            ("genre", node.genre.clone(), was.genre),
        ] {
            match now {
                Some(name) if Some(&name) != before.as_ref() => {
                    self.remember(Some("state"), key, &Self::toml_string(&name));
                }
                None if before.is_some() => self.forget(Some("state"), key),
                _ => {}
            }
        }
    }

    /// Whether the band area is in place (issue #40): the visualizer draws
    /// something and low power has not taken it away — low power still wins
    /// over the visualizer, and `none` gives the rows back exactly as it
    /// does (S6).
    fn shows_bands(&self) -> bool {
        self.ui.visualizer != Visualizer::None && !self.ui.low_power
    }

    /// How many rows the now-playing block takes out of a content area this
    /// tall, with the `[ui]` switches applied (S5). The band belongs to
    /// the tall block: with no visualizer drawing it the block never
    /// reserves its rows — the tall tier is capped at the seven-row block,
    /// and the bars' rows go to the list instead.
    fn now_playing_rows(&self, height: u16) -> u16 {
        // Content tall enough for the spectrum tier starts at 22 rows; any
        // shorter height keeps the block at its ordinary size. Low power
        // (issue #11, S2) leaves the spectrum out entirely: its rows go
        // back to the list, the rest of the layout unchanged (S3).
        const SPECTRUM_TIER: u16 = 22;
        now_playing_rows(if self.shows_bands() {
            height
        } else {
            height.min(SPECTRUM_TIER - 1)
        })
    }

    /// The level the config asks for. Applied to a running player and kept for
    /// the next one, so a stream built after a seek keeps it (T11). Under
    /// bit-perfect the level is pinned to 100% whatever is asked — the
    /// callback must not scale samples — and one status line explains why.
    pub fn set_volume(&mut self, percent: u8) {
        if self.bit_perfect {
            self.volume = 100;
            if percent != 100 && !self.bit_perfect_volume_said {
                self.bit_perfect_volume_said = true;
                self.status = String::from(
                    "velo: bit-perfect playback pins the volume to 100% and skips ReplayGain",
                );
            }
            if let Some(player) = &self.player {
                player.set_volume(100);
            }
            return;
        }
        self.volume = percent;
        if let Some(player) = &self.player {
            player.set_volume(percent);
        }
    }

    /// The queue, in play order.
    #[must_use]
    pub fn queue(&self) -> &[Entry] {
        &self.queue
    }

    /// Where in the library the interface stands, or `None` over a flat
    /// command-line list.
    #[must_use]
    pub fn level(&self) -> Option<Level> {
        match &self.position {
            Position::Files => None,
            Position::Artists => Some(Level::Artists),
            Position::Albums { artist } => Some(Level::Albums {
                artist: artist.clone(),
            }),
            Position::Tracks { artist, album } => Some(Level::Tracks {
                artist: artist.clone(),
                album: album.clone(),
            }),
            Position::AllAlbums => Some(Level::AllAlbums),
            Position::AllTracks => Some(Level::AllTracks),
            Position::Genres => Some(Level::Genres),
            Position::Genre { genre } => Some(Level::Genre {
                genre: genre.clone(),
            }),
            Position::Recent => Some(Level::Recent),
            Position::Favourites => Some(Level::Favourites),
            Position::Playlists => Some(Level::Playlists),
            Position::Playlist { name } => Some(Level::Playlist { name: name.clone() }),
        }
    }

    /// Which of the four library views the browser shows.
    #[must_use]
    pub const fn browse_by(&self) -> BrowseBy {
        self.browse_by
    }

    /// The genres snapshot, empty outside the genres view.
    #[must_use]
    pub fn genres(&self) -> &[GenreEntry] {
        &self.genres
    }

    /// The files `Enter` on row `from` plays, in play order: the rest of the
    /// list the row belongs to — its album, its genre, or every track. Empty
    /// on a level whose rows are not tracks.
    #[must_use]
    pub fn playlist_from(&self, from: usize) -> Vec<PathBuf> {
        match &self.position {
            // From the file Enter was pressed on, as an album plays from its
            // selected track: handing the whole list over started the stream
            // at the first file whichever one was chosen.
            Position::Files if from < self.argv.len() => self.argv[from..].to_vec(),
            // From the file rows onwards; a station is never in a playlist
            // slice (issue #33): it has no path to hand a stream of files.
            Position::Recent => self
                .recent_rows
                .get(from..)
                .unwrap_or_default()
                .iter()
                .filter(|entry| !entry.is_station())
                .map(|entry| PathBuf::from(&entry.path))
                .collect(),
            Position::Tracks { .. } | Position::AllTracks | Position::Genre { .. } => self
                .tracks
                .get(from..)
                .unwrap_or_default()
                .iter()
                .map(|entry| entry.path.clone())
                .collect(),
            Position::Favourites => self
                .favourite_rows
                .get(from..)
                .unwrap_or_default()
                .iter()
                .map(|favorite| PathBuf::from(&favorite.path))
                .collect(),
            // A playlist's track list (issue #4): from the selected track
            // onwards, in the order the store holds, exactly as the store
            // says it — the stream is built from the store's paths.
            Position::Playlist { name } => self
                .favourites
                .playlist(name)
                .map(|playlist| {
                    playlist
                        .track
                        .get(from..)
                        .unwrap_or_default()
                        .iter()
                        .map(PathBuf::from)
                        .collect()
                })
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    /// The artists snapshot, empty when not browsing.
    #[must_use]
    pub fn artists(&self) -> &[ArtistEntry] {
        &self.artists
    }

    /// The albums of the artist being browsed, or every album in the albums
    /// view; empty elsewhere.
    #[must_use]
    pub fn albums(&self) -> &[AlbumEntry] {
        &self.albums
    }

    /// The tracks of the album or genre being browsed, or every track in the
    /// tracks view; empty elsewhere.
    #[must_use]
    pub fn tracks(&self) -> &[TrackEntry] {
        &self.tracks
    }

    /// Opens the floating prompt asking for the library root, because none
    /// is configured. An accepted path is saved to `config_file` (when there
    /// is one) as the `library` key. `~/` stands for `$HOME`.
    pub fn ask_for_library(&mut self, config_file: Option<PathBuf>) {
        self.config_file.clone_from(&config_file);
        let home = std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        self.ask_for_library_under(config_file, home);
    }

    /// [`App::ask_for_library`] with `~/` standing for `home`, so the prompt
    /// can be driven against a folder of the caller's choosing.
    pub fn ask_for_library_under(&mut self, config_file: Option<PathBuf>, home: Option<PathBuf>) {
        self.library_setup = Some(LibrarySetup {
            config_file,
            home,
            prompt: Some(LibraryPrompt::new()),
        });
    }

    /// The config file a library chosen later with `L` is saved to, and the
    /// file the history (issue #5) and the saved store (issue #7) live
    /// beside: both are loaded here, eagerly, so a missing or corrupt file
    /// leaves an empty state — the interface never complains about either.
    pub fn set_config_file(&mut self, config_file: Option<PathBuf>) {
        self.recent = Recent::default();
        if let Some(path) = &config_file
            && let Ok(history) = crate::history::History::load(&crate::history::path_beside(path))
        {
            self.recent.entries = history;
        }
        self.favourites = crate::saved::Saved::default();
        if let Some(path) = &config_file
            && let Ok(saved) = crate::saved::Saved::load(&crate::saved::path_beside(path))
        {
            self.favourites = saved;
        }
        self.config_file = config_file;
    }

    /// The resolver the player uses to turn queue entries into files (T12c):
    /// a remote row's `subsonic://…` entry resolves through the subsonic
    /// cache when the producer reaches it. `None` (a local-only launch)
    /// means every entry already is a file.
    pub fn set_resolver(&mut self, resolver: Option<Arc<dyn PathResolver>>) {
        self.resolver = resolver;
    }

    /// Installs the remote cover-art source (T12d): the same resolver the
    /// queue resolves through, seen as the covers cache it also serves.
    /// `None` leaves remote rows with no cover, exactly as a local file with
    /// no picture draws none.
    pub fn set_covers(&mut self, covers: Option<Arc<dyn CoverArt>>) {
        self.covers = covers;
    }

    /// Installs the scrobble sink (T12d). `None` scrobbles nothing: a
    /// local-only launch, or a server whose credentials never resolved.
    pub fn set_scrobbler(&mut self, scrobbler: Option<Arc<dyn Scrobbles>>) {
        self.scrobbles = scrobbler.map(ScrobbleTracker::new);
    }

    /// Takes the credential resolver's warnings — today, a group- or
    /// world-readable `subsonic.key` (T12c's debt). Empty means the key file
    /// is private (or no key file is in play) and the status line says
    /// nothing. The warnings ride to the first tick, which says them once.
    /// Installs the one-shot update check (issue #69, slice 1): its thread
    /// runs off the event loop and never touches the config file; the tick
    /// polls the one answer and records the timestamp itself.
    pub fn set_update_check(&mut self, check: crate::update::UpdateCheck) {
        self.update_check = Some(check);
    }

    /// The newer version a finished check found, with the leading `v` off
    /// (issue #69); `None` when nothing has been found yet.
    #[must_use]
    pub fn update_latest(&self) -> Option<&str> {
        self.update_latest
            .as_deref()
            .map(crate::update::display_version)
    }

    /// The value the settings card's `version` row shows (issue #69): the
    /// running version in force, plus the newer one when a check found it.
    #[must_use]
    pub fn version_setting_value(&self) -> String {
        match &self.update_latest {
            Some(latest) => format!(
                "{} — {} available",
                crate::update::RUNNING_VERSION,
                crate::update::display_version(latest)
            ),
            None => crate::update::RUNNING_VERSION.to_owned(),
        }
    }

    /// What a finished check said, said once (issue #69, slice 1): the
    /// timestamp of the check is recorded here first, on the loop thread —
    /// the one writer the config file has — through the same
    /// [`App::remember`] seam every user setting uses, whatever the answer
    /// was: a failed check must also wait a whole interval, or a refused
    /// request would be repeated on every launch. If the process exits
    /// before a tick consumes the answer, nothing is recorded — an accepted,
    /// harmless edge: the next launch simply checks again, once. Then a
    /// newer version is one status line, and the settings row learns it; a
    /// failed check is silence — no error line, no retry from here. The
    /// handle drops either way: the check is one-shot.
    fn update_tick(&mut self) {
        let Some(outcome) = self
            .update_check
            .as_mut()
            .and_then(crate::update::UpdateCheck::poll)
        else {
            return;
        };
        self.update_check = None;
        // The recording comes before the status line, so the write's rule —
        // a failed write is the status line — can never mask the news of a
        // newer version: the newer-version line is set last and wins.
        if let Ok(elapsed) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            self.remember(Some("update"), "last_check", &elapsed.as_secs().to_string());
        }
        if let crate::update::CheckOutcome::Tag(tag) = outcome
            && crate::update::newer_version(crate::update::RUNNING_VERSION, &tag)
        {
            self.status = format!(
                "velo {} is available — you have {}",
                crate::update::display_version(&tag),
                crate::update::RUNNING_VERSION
            );
            self.update_latest = Some(tag);
        }
    }

    pub fn set_subsonic_warnings(&mut self, warnings: &[String]) {
        self.subsonic_warning = (!warnings.is_empty()).then(|| warnings.join("; "));
    }

    /// The launch with no `library` configured (T12c): with a Subsonic server
    /// configured, the synced index is the library — nothing is asked, and
    /// `/` reaches its search; with neither, one sentence says what to
    /// configure instead of asking for a folder. `o` (or `ctrl+l`) still
    /// opens the prompt at any time.
    pub fn launch_without_library(&mut self, subsonic_configured: bool) {
        if subsonic_configured {
            return;
        }
        self.note(NO_LIBRARY_OR_SERVER_MESSAGE);
    }

    /// The folders the last `Tab` in the library prompt offered.
    #[must_use]
    pub fn library_prompt_candidates(&self) -> &[String] {
        self.library_prompt()
            .map_or(&[], |prompt| prompt.candidates.as_slice())
    }

    /// Completes the folder being typed in the library prompt, as a shell's
    /// `Tab` does (T7). Reads the disk, so it runs from `apply`, not the keys.
    fn complete_library_path(&mut self) {
        let Some(setup) = &mut self.library_setup else {
            return;
        };
        let (Some(prompt), Some(home)) = (&mut setup.prompt, &setup.home) else {
            return;
        };
        let (input, candidates) = complete::complete(&prompt.input, home);
        prompt.input = input;
        prompt.candidates = candidates;
        prompt.error = None;
    }

    /// Whether the library prompt is open.
    #[must_use]
    pub fn library_prompt_open(&self) -> bool {
        self.library_prompt().is_some()
    }

    fn library_prompt(&self) -> Option<&LibraryPrompt> {
        self.library_setup.as_ref()?.prompt.as_ref()
    }

    /// The path typed into the library prompt, empty when it is closed.
    #[must_use]
    pub fn library_prompt_input(&self) -> &str {
        self.library_prompt()
            .map_or("", |prompt| prompt.input.as_str())
    }

    /// Why the prompt refused the last path, if it did.
    #[must_use]
    pub fn library_prompt_error(&self) -> Option<&str> {
        self.library_prompt()
            .and_then(|prompt| prompt.error.as_deref())
    }

    /// The library root accepted in the prompt, handed over once, for the
    /// shell to index and browse.
    pub const fn take_library_root(&mut self) -> Option<PathBuf> {
        self.library_root.take()
    }

    /// Whether the seek prompt (issue #6) is open.
    #[must_use]
    pub const fn seek_prompt_open(&self) -> bool {
        self.seek_prompt.is_some()
    }

    /// What is typed into the seek prompt, empty when it is closed.
    #[must_use]
    pub fn seek_prompt_input(&self) -> &str {
        self.seek_prompt
            .as_ref()
            .map_or("", |prompt| prompt.input.as_str())
    }

    /// Whether the URL prompt (issue #42) is open.
    #[must_use]
    pub const fn url_prompt_open(&self) -> bool {
        self.url_prompt.is_some()
    }

    /// What is typed into the URL prompt, empty when it is closed.
    #[must_use]
    pub fn url_prompt_input(&self) -> &str {
        self.url_prompt
            .as_ref()
            .map_or("", |prompt| prompt.input.as_str())
    }

    /// Why the URL prompt refused the last answer, if it did.
    #[must_use]
    pub fn url_prompt_error(&self) -> Option<&str> {
        self.url_prompt
            .as_ref()
            .and_then(|prompt| prompt.error.as_deref())
    }

    /// Whether the save prompt (issue #45) is open.
    #[must_use]
    pub const fn save_prompt_open(&self) -> bool {
        self.save_prompt.is_some()
    }

    /// What is typed into the save prompt, empty when it is closed.
    #[must_use]
    pub fn save_prompt_input(&self) -> &str {
        self.save_prompt
            .as_ref()
            .map_or("", |prompt| prompt.input.as_str())
    }

    /// Why the save prompt refused the last answer, if it did.
    #[must_use]
    pub fn save_prompt_error(&self) -> Option<&str> {
        self.save_prompt
            .as_ref()
            .and_then(|prompt| prompt.error.as_deref())
    }

    /// Whether a saving copy (issue #45) is running right now.
    #[must_use]
    pub const fn save_running(&self) -> bool {
        self.pending_save.is_some()
    }

    /// The library root `I` refreshes: the configured one, or `None` when no
    /// library is set. A root accepted in the prompt replaces it.
    pub fn set_library_root(&mut self, root: Option<PathBuf>) {
        self.refresh.root = root;
    }

    /// The folder a background index is scanning, while one runs.
    #[must_use]
    pub fn indexing(&self) -> Option<&Path> {
        self.refresh.running.as_deref()
    }

    /// The shell started indexing `root` in the background: the title bar
    /// shows it until [`App::index_finished`].
    pub fn index_started(&mut self, root: &Path) {
        self.refresh.running = Some(root.to_path_buf());
        self.status = format!("indexing {} …", root.display());
    }

    /// The background index ended: its counts, or why it failed, go to the
    /// status line, and after a success every list on screen is read again.
    pub fn index_finished(&mut self, outcome: Result<IndexReport, String>) {
        self.refresh.running = None;
        match outcome {
            Ok(report) => {
                self.status = format!(
                    "indexed: {} added, {} updated, {} unchanged, {} removed",
                    report.added, report.updated, report.unchanged, report.removed
                );
                // A read error replaces the counts: it matters more.
                self.reload();
                self.reload_search();
            }
            // The index is written in one transaction, so a failure left it
            // as it was and the lists on screen are still true.
            Err(error) => self.status = format!("velo: indexing failed: {error}"),
        }
    }

    /// The shell could not open the library index at launch: `u` then says
    /// so, with `error`, instead of asking for a library.
    pub fn index_unavailable(&mut self, error: impl Into<String>) {
        let error = error.into();
        self.status = format!("velo: cannot open the library index: {error}");
        self.refresh.open_error = Some(error);
    }

    /// What `I` answers: the refresh to run, or why there is none.
    fn ask_to_reindex(&mut self) -> Action {
        if let Some(running) = &self.refresh.running {
            self.status = format!("already indexing {}", running.display());
            return Action::None;
        }
        // Without an open index there is nothing to write to, whether or not
        // a library is set: the shell would never start the refresh.
        if self.library.is_none()
            && let Some(error) = &self.refresh.open_error
        {
            self.status =
                format!("velo: cannot open the library index: {error} — nothing to refresh");
            return Action::None;
        }
        if self.refresh.root.is_none() {
            self.note("no library set — press L to choose one");
            return Action::None;
        }
        if self.library.is_none() {
            self.note("the library index is not open — nothing to refresh");
            return Action::None;
        }
        Action::Reindex
    }

    /// Whether the help overlay is open.
    #[must_use]
    pub const fn help_open(&self) -> bool {
        self.help.is_some()
    }

    /// Whether the settings card is open (S6).
    #[must_use]
    pub const fn settings_open(&self) -> bool {
        self.settings.is_some()
    }

    /// What the settings card focuses, as the row's label (S6): the section
    /// headers are never focused, and a closed card focuses nothing.
    #[must_use]
    pub fn settings_focused(&self) -> &'static str {
        self.settings
            .as_ref()
            .map_or("replay gain", SettingsCard::focused_label)
    }

    /// The volume, in percent.
    #[must_use]
    pub const fn volume(&self) -> u8 {
        self.volume
    }

    /// Writes the status line, e.g. indexing progress.
    pub fn note(&mut self, message: impl Into<String>) {
        self.status = message.into();
    }

    /// The current status line, empty when the transport line is shown.
    #[must_use]
    pub fn status(&self) -> &str {
        &self.status
    }

    /// The selected row of the result list while a search is open.
    #[must_use]
    pub const fn search_selected(&self) -> usize {
        self.search_selected
    }

    /// Whether the library search is open.
    #[must_use]
    pub const fn search_open(&self) -> bool {
        self.search.is_some()
    }

    /// The query as typed, empty when the search is closed.
    #[must_use]
    pub fn search_query(&self) -> &str {
        self.search.as_ref().map_or("", Search::query)
    }

    /// The matching entries in ranked order, empty when the search is closed.
    #[must_use]
    pub fn search_matches(&self) -> Vec<&TrackEntry> {
        if self.search.is_none() {
            return Vec::new();
        }
        self.search_results
            .iter()
            .filter_map(|&index| self.search_pool.get(index))
            .collect()
    }

    /// Installs the cover-art capability detected once at startup. `None`
    /// leaves the interface exactly as it draws without art (T8).
    pub fn show_artwork(&mut self, picker: Option<Picker>) {
        self.picker = picker;
    }

    /// Installs a decoded cover without a stream, the way [`App::show_artwork`]
    /// installs the capability: the integration tests' hook for the decoded-art
    /// leg of the cover gates, which no fixture can reach while a track plays.
    /// Production never calls this, which is why it is hidden from the docs.
    #[doc(hidden)]
    pub fn show_decoded_artwork(&mut self, artwork: Option<artwork::Artwork>) {
        self.artwork = artwork;
        self.artwork_path = None;
    }

    /// Plants the newer version a finished check found (issue #69): the
    /// integration tests' hook, because no fixture can reach a real release
    /// tag without a network. Production never calls this, which is why it
    /// is hidden from the docs; the wiring sets it through
    /// [`crate::update::UpdateCheck::poll`] inside the tick instead.
    #[doc(hidden)]
    pub fn show_update_latest(&mut self, latest: Option<String>) {
        self.update_latest = latest;
    }

    /// Stands the open radio view in `place` and sets its catalog (T6c):
    /// the integration tests' hook, because no fixture can reach a station
    /// row through the real wiring — that needs the directory. The view
    /// stands in the place the stations belong to (S9); production never
    /// calls this, which is why it is hidden from the docs; the wiring sets
    /// the catalog through [`radio_data::RadioWire::poll`] instead.
    #[doc(hidden)]
    pub fn show_radio_catalog(
        &mut self,
        place: crate::ui::radio::Place,
        stations: Vec<crate::radio::client::Station>,
    ) {
        if let Some(view) = self.radio.as_mut() {
            view.open_place(place.clone());
            view.set_catalog(place, stations);
        }
    }

    /// Stands the open radio view in one cliamp channel's place and sets
    /// its songs (issue #48): the integration tests' hook, because no
    /// fixture can reach a song row through the real wiring — that needs
    /// the cliamp service. Production never calls this, which is why it is
    /// hidden from the docs; the wiring sets the songs through
    /// [`radio_data::RadioWire::poll`] instead.
    #[doc(hidden)]
    pub fn show_radio_cliamp_songs(
        &mut self,
        place: crate::ui::radio::Place,
        songs: Vec<crate::radio::cliamp::Song>,
    ) {
        if let Some(view) = self.radio.as_mut() {
            view.open_place(place.clone());
            view.set_cliamp_songs(place, songs);
        }
    }

    /// Installs the radio's data wiring (the integration tests' hook): no
    /// test can reach the directory, so the tests drive a wiring over a
    /// canned transport through the same [`App`] the event loop runs.
    /// Production never calls this — `R` builds the real wiring when none
    /// is installed.
    #[doc(hidden)]
    pub fn set_radio_wire(&mut self, wire: Box<dyn radio_data::RadioWire>) {
        self.radio_wire = Some(wire);
    }

    /// Installs the URL opener (the integration tests' hook): no test can
    /// spawn a browser, so the tests inject a recorder and assert on the
    /// URL a report hands over. Production never calls this — [`App::new`]
    /// installs the real system opener ([`open_url`]), which is why it is
    /// hidden from the docs.
    #[doc(hidden)]
    pub fn set_url_opener(&mut self, opener: fn(&str) -> Result<(), String>) {
        self.url_opener = opener;
    }

    /// Stands in for a playing player (T6c), which no test can build —
    /// building one opens an audio device: the entry becomes the queue the
    /// transport reads, and the position, total and ICY title are held in
    /// the fixture, exactly the shape a `Player` answers. The state fields
    /// are set the way building a player sets them. Production never calls
    /// this, which is why it is hidden from the docs.
    #[doc(hidden)]
    pub fn show_playing_for_tests(
        &mut self,
        entry: Entry,
        position: Duration,
        duration: Option<Duration>,
        title: Option<String>,
    ) {
        self.queue = vec![entry];
        self.playing = Some(Playback::Queue);
        self.state = PlayState::Playing;
        self.transport_fixture = Some(TransportFixture {
            position,
            duration,
            title,
            fetching: None,
        });
    }

    /// Says which queue entry the transport is fetching, the way the real
    /// player's fetching cell answers [`Player::fetching_index`] (issue
    /// #60): `Some(index)` while the download runs, `None` when it is over.
    /// No test can build a player — building one opens an audio device —
    /// so the fixture carries the fact. Production never calls this, which
    /// is why it is hidden from the docs.
    #[doc(hidden)]
    pub fn set_fetching_for_tests(&mut self, fetching: Option<usize>) {
        if let Some(fixture) = self.transport_fixture.as_mut() {
            fixture.fetching = fetching;
        }
    }

    /// Moves the transport to a state no key reaches without a player
    /// (issue #53): the queue and the playing fact stay exactly where they
    /// were, only the state changes — the tests' stand-in for the moment a
    /// stream stops or stands paused while the queue still stands on its
    /// entry. Production never calls this, which is why it is hidden from
    /// the docs.
    #[doc(hidden)]
    pub fn set_play_state_for_tests(&mut self, state: PlayState) {
        self.state = state;
    }

    /// Queues one remote song entry (cliamp S4), the queue edit an expanded
    /// channel makes: the entry is the song's URL as a file row, its identity
    /// (S3). Production queues through [`App::play_station`]; a test cannot
    /// call that with a real resolution, so this stands in for the edit
    /// alone. Hidden from the docs.
    #[doc(hidden)]
    pub fn queue_song_for_tests(&mut self, url: &str) {
        self.edit_queue(|queue| queue.push(Entry::File(PathBuf::from(url))));
    }

    /// Drives [`Self::start_track_at`] — the one rebuild behind a skip and
    /// a seek — the way a skip lands, zero offset, for the tests: no test
    /// can hold a `Player` — building one opens an audio device — so the
    /// rebuild's own shape is reached through this hook; production reaches
    /// it through `skip` and `seek_by`. Hidden from the docs.
    #[doc(hidden)]
    pub fn start_track_at_for_tests(&mut self, track: usize) {
        self.start_track_at(track, Duration::ZERO);
    }

    /// Runs once per turn of the event loop — the same cadence as key
    /// polling — so the producer's announcements surface without a key
    /// press, the cover art follows the track it plays (T8), and the server
    /// learns what is playing (T12d). Without a player there is nothing to
    /// collect.
    pub fn tick(&mut self) {
        // A theme notice (S3, issue #10) is said once, here, where the user
        // is looking — a fallback that repeats is noise, and one that only
        // ever lives in the config file is never seen. The `take` is the
        // once: later ticks find nothing left to say.
        if let Some(notice) = self.theme_notice.take() {
            self.status = format!("velo: {notice}");
        }
        // A credential warning is said once, here, where the user is looking
        // — not only inside a skip reason when a download later fails. The
        // `take` is the once: later ticks find nothing left to say.
        if let Some(warning) = self.subsonic_warning.take() {
            self.status = format!("velo: warning: {warning}");
        }
        // The update check's one word (issue #69): a newer version is one
        // status line, said once; a failed check is silence.
        self.update_tick();
        // The radio wiring (T6b) turns once per tick too: a finished
        // directory request lands without a key press, and an in-flight
        // one says `⟳ loading…` on the status line.
        self.radio_poll();
        // The fetching fact (issue #60): a remote entry the queue is
        // downloading says so on the status line until it sounds. The start's
        // own worker is polled first, so a finished fetch begins its stream
        // before the line is decided for this turn.
        self.poll_pending_resolve();
        self.fetch_status();
        // The handover's clock (issue #65): the old stream's fade-out armed
        // once the new one sounds, and stopped once it has fully fallen.
        self.poll_crossfade();
        self.poll_cover();
        // The saving copy's worker (issue #45): its progress and its last
        // word land without a key press, the way a finished cover and a
        // finished directory request do.
        self.poll_save();
        self.refresh_artwork();
        self.explain_hidden_art();
        self.listen();
        // The media keys (issue #12, S6): what the OS sent reaches the same
        // actions the keys perform, and what is playing goes back — but only
        // when it changed. Both halves ride a channel, so the tick never
        // waits on the OS.
        self.media_tick();
        let path = self.playing_path().map(Path::to_path_buf);
        let (position, duration) = match &self.player {
            Some(player) => (player.position(), self.current_track_duration()),
            None => (Duration::ZERO, None),
        };
        self.scrobble_tick(path.clone(), position, duration);
        let entry = self.playing_entry().cloned();
        self.history_tick(entry.as_ref());
        let Some(player) = &self.player else {
            return;
        };
        if let Some(failure) = player.take_failure() {
            self.announce_failure(&failure);
            return;
        }
        // A bit-perfect fallback and its kin: one line, said once (T17).
        if let Some(notice) = player.take_notice() {
            self.status = format!("velo: {notice}");
        }
    }

    /// The media keys' half of the tick (issue #12, S6): commands the OS
    /// delivered since the last turn are applied, what is playing is
    /// published when it changed, and a client that refused its own start
    /// said its one line. Without media keys there is nothing to collect.
    fn media_tick(&mut self) {
        if self.media.is_none() {
            return;
        }
        // The client's one word about its own start (S6), said once.
        if let Some(message) = self
            .media
            .as_mut()
            .and_then(crate::media_keys::MediaKeys::take_init_error)
        {
            self.status = format!("velo: {message}");
        }
        // What the OS sent reaches the keys' own actions.
        for command in self
            .media
            .as_ref()
            .map_or_else(Vec::new, crate::media_keys::MediaKeys::drain_commands)
        {
            self.media_command(command);
        }
        // And what plays goes back, but only when it changed: a still
        // session sends nothing.
        let snapshot = self.media_snapshot();
        if self.media_published.as_ref() != Some(&snapshot)
            && let Some(media) = self.media.as_ref()
        {
            media.publish(&snapshot);
            self.media_published = Some(snapshot);
        }
    }

    /// What the OS should show right now (issue #12, S5): the track's title
    /// and artist when a file plays, the station's own name — or the ICY
    /// title it is announcing — when a station plays, and nothing when
    /// nothing does.
    fn media_snapshot(&self) -> crate::media_keys::MediaState {
        crate::media_keys::MediaState {
            now: self.playing_entry().map(|entry| self.now_playing_of(entry)),
            playing: self.state == PlayState::Playing,
        }
    }

    /// What one queue entry means to the OS (S5): nothing is invented — a
    /// cold file is its file name, a station without an ICY title is its
    /// own name, an unknown artist is no artist.
    fn now_playing_of(&self, entry: &Entry) -> crate::media_keys::NowPlaying {
        match entry {
            Entry::File(path) => {
                let known = self.known_tracks().find(|track| track.path == *path);
                let title = match known {
                    Some(track) if !track.title.is_empty() => track.title.clone(),
                    _ => entry.label().into_owned(),
                };
                let artist = known
                    .map(|track| track.artist.clone())
                    .filter(|artist| !artist.is_empty());
                crate::media_keys::NowPlaying {
                    title,
                    artist,
                    duration: self.current_track_duration(),
                }
            }
            Entry::Station { name, .. } => crate::media_keys::NowPlaying {
                title: self.station_title(name),
                artist: None,
                duration: None,
            },
        }
    }

    /// The scrobble decision, fed once per tick with what is playing (T12d).
    /// Production calls this from [`App::tick`] with the player's own
    /// values; the tests drive it headlessly. A local track sends nothing,
    /// and nothing here ever changes what the user sees. The path is taken
    /// by value because the tick must end the borrow of the queue before
    /// the tracker inside `self` can be fed.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "the owned path ends the tick's immutable borrow of self before the mutable one"
    )]
    #[doc(hidden)]
    pub fn scrobble_tick(
        &mut self,
        path: Option<PathBuf>,
        position: Duration,
        duration: Option<Duration>,
    ) {
        if let Some(tracker) = self.scrobbles.as_mut() {
            tracker.on_tick(path.as_deref(), position, duration);
        }
    }

    /// One history write per beginning (issue #5): a playing entry the tick
    /// has not seen as the playing one is a beginning, and every beginning
    /// records one entry — the same identity moves to the top instead of
    /// duplicating — and saves the file atomically. A history that cannot be
    /// written is lost, not fatal: the save is best effort, and a tick with
    /// nothing playing forgets what it saw, so the next beginning of the
    /// same entry counts again. A file beginning records the path the queue
    /// carries, with what the index can say about it; a station beginning
    /// (issue #33) records the stream URL and the station's own name, with
    /// no lookup. The seen key is the entry's identity, so the same station
    /// twice is one beginning however its URL is written, and a station
    /// never collapses into a file. Production calls this from [`App::tick`];
    /// the tests drive it headlessly.
    #[doc(hidden)]
    pub fn history_tick(&mut self, playing: Option<&Entry>) {
        let Some(playing) = playing else {
            self.recent.seen = None;
            return;
        };
        let recorded = match playing {
            Entry::File(path) => {
                // What the index can say about the path: everything, when it
                // is a library row; nothing, when it is a file the queue
                // opened cold. An empty field is a field the index does not
                // know: S3 keeps it out of the file instead of writing `""`.
                let known = |text: &str| (!text.is_empty()).then(|| text.to_owned());
                let (artist, album, title) = self
                    .known_tracks()
                    .find(|track| track.path == *path)
                    .map_or((None, None, None), |track| {
                        (
                            known(&track.artist),
                            known(&track.album),
                            known(&track.title),
                        )
                    });
                crate::history::Entry {
                    path: path.to_string_lossy().into_owned(),
                    kind: crate::history::Kind::File,
                    played_at: crate::history::now_unix(),
                    artist,
                    album,
                    title,
                }
            }
            Entry::Station { url, name } => {
                crate::history::Entry::station(url, name, crate::history::now_unix())
            }
        };
        let identity = recorded.identity();
        if self.recent.seen.as_deref() == Some(identity.as_str()) {
            return;
        }
        let Some(config_file) = self.config_file.clone() else {
            return;
        };
        let file = crate::history::path_beside(&config_file);
        self.recent.entries.record(recorded);
        self.recent.seen = Some(identity);
        let _ = self.recent.entries.save(&file);
    }

    /// The recently played entries, newest first (issue #5): the same list
    /// the history file holds, for the `Recently played` screen to read.
    #[must_use]
    pub fn recently_played(&self) -> &[crate::history::Entry] {
        &self.recent.entries.entry
    }

    /// The rows the `Recently played` screen stands on (issue #5): the
    /// history's entries as the file holds them, newest first — tracks and
    /// stations alike (issue #33). Empty outside that screen.
    #[must_use]
    pub fn recent_rows(&self) -> &[crate::history::Entry] {
        &self.recent_rows
    }

    /// Collects a remote cover that finished arriving (T12d): the fetch ran
    /// on a background thread, so a tick never waited on the network. A
    /// result for a track that has since moved on is dropped — its
    /// replacement fetch is already running.
    fn poll_cover(&mut self) {
        let delivered: Option<PathBuf> = match self
            .pending_cover
            .as_ref()
            .map(|(_, receiver)| receiver.try_recv())
        {
            Some(Ok(file)) => file,
            Some(Err(TryRecvError::Disconnected)) => None,
            Some(Err(TryRecvError::Empty)) | None => return,
        };
        let Some((path, _)) = self.pending_cover.take() else {
            return;
        };
        let playing = self.playing_path().map(Path::to_path_buf);
        if playing.as_deref() == Some(path.as_path()) {
            self.artwork = delivered.as_deref().and_then(Artwork::from_image_file);
        }
    }

    /// Puts a skipped entry into the status line: `velo: skipped <label>:
    /// <why>`. The label is exactly what the failure record carries — a
    /// file's file name, a station's own name — so it is printed as is, with
    /// no path to extract anything from (T5c2a). The message stays until the
    /// next status update — a play, a seek or another failure — because a
    /// skipped track is not a sequence error and nothing else has happened
    /// worth saying (T19).
    pub fn announce_failure(&mut self, failure: &TrackFailure) {
        self.status = format!("velo: skipped {}: {}", failure.label, failure.message);
    }

    /// Swaps the decoded cover when the track that is playing changes, and
    /// only then: the file is read once per track, never per frame, and never
    /// from the audio callback (T8). A track whose file carries no picture
    /// draws no art until a different track plays.
    /// Feeds the spectrum what the device played since the last tick and
    /// moves its bars one step: up to the music, or down when none came.
    /// Runs whether or not the spectrum is on screen, so the tap is always
    /// drained and never shows half a second of old audio when it returns.
    fn listen(&mut self) {
        if let Some(player) = &self.player {
            player.drain_tap(&mut self.heard);
            self.spectrum.push(&self.heard);
            // The waveform visualizers (issue #40) read the same tap: keep
            // the newest window, oldest first, the way the analyzer keeps
            // its own. Without a player the window stands still, the way
            // the bars only fall.
            self.waveform.extend_from_slice(&self.heard);
            let stale = self
                .waveform
                .len()
                .saturating_sub(visualizer::WAVEFORM_WINDOW);
            self.waveform.drain(..stale);
            self.heard.clear();
        }
        let rate = self
            .player
            .as_ref()
            .map_or(48_000, |player| player.spec().sample_rate);
        self.spectrum.update(self.spectrum_bands.get(), rate);
    }

    /// How long the event loop may wait for a key before drawing again:
    /// about thirty frames a second while bars on screen move, ten otherwise
    /// — a window too short for the spectrum has nothing to animate. In low
    /// power (issue #11) the rate is a quarter of whatever it would be.
    #[must_use]
    pub fn frame_interval(&self) -> Duration {
        cadence(
            self.spectrum_bands.get() > 0,
            self.state == PlayState::Playing,
            self.spectrum.levels().iter().any(|&level| level > 0.0),
            self.ui.low_power,
        )
    }

    /// Tells the user once why a cover they own is not on screen, instead of
    /// leaving them staring at a panel that never appears (T8). The
    /// `[ui] advisories` switch silences exactly this explanation (S5.4); it
    /// never silences errors or ordinary messages.
    fn explain_hidden_art(&mut self) {
        // A cover the user switched off is hidden on purpose (S5): that is a
        // decision, not a malfunction, and nothing explains it.
        if !self.ui.cover_art
            || !self.ui.advisories
            || self.art_explained
            || !self.status.is_empty()
        {
            return;
        }
        let area = self.seen_area.get();
        let Some(reason) =
            artwork::hidden_reason(self.picker.is_some(), self.artwork.is_some(), area)
        else {
            return;
        };
        self.status = format!("velo: cover art hidden — {reason}");
        self.art_explained = true;
    }

    fn refresh_artwork(&mut self) {
        let path = self.playing_path().map(Path::to_path_buf);
        if path == self.artwork_path {
            return;
        }
        self.protocol.borrow_mut().take();
        self.artwork_path.clone_from(&path);
        self.artwork = None;
        self.art_explained = false;
        let Some(path) = path else {
            return;
        };
        // A remote row's cover is not in any file's tags: it is fetched once
        // from the server into the covers cache (T12d) and decoded from
        // there — off this path, on a background thread, so a slow server
        // never stalls a tick or a frame.
        if !crate::subsonic::cache::is_pseudo_path(&path) {
            self.artwork = Artwork::from_path(&path);
            return;
        }
        let Some(cover_id) = self
            .known_tracks()
            .find(|entry| entry.path == path)
            .and_then(|entry| entry.cover_id.clone())
        else {
            return;
        };
        let Some(covers) = self.covers.clone() else {
            return;
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(covers.cover_file(&cover_id));
        });
        self.pending_cover = Some((path, receiver));
    }

    /// Re-reads from the index, e.g. right after indexing, the artists, the
    /// list the interface stands on and the one above it, keeping the view
    /// and the level. The selected row stays selected when it still exists;
    /// otherwise the selection stays inside the (possibly shorter) list. A
    /// level whose artist, album or genre is gone returns to the top of its
    /// view. A list that cannot be read keeps its last snapshot.
    pub fn reload(&mut self) {
        let Some(library) = self.library else {
            return;
        };
        let kept = self.row_key(self.selected);
        // One snapshot: a commit from the background indexer cannot land
        // between the artists and the level's lists.
        if let Err(error) = library.snapshot(|library| self.read_level(library)) {
            self.status = format!("velo: {error}");
        }
        let deep = matches!(
            self.position,
            Position::Albums { .. } | Position::Tracks { .. } | Position::Genre { .. }
        );
        if deep && self.row_count() == 0 {
            // Back to the top of the view, on whichever tab the user is: an
            // index they did not start must not move them off the queue.
            let view = self.view;
            self.browse_view(self.browse_by);
            self.view = view;
            return;
        }
        let found = kept.and_then(|key| {
            (0..self.row_count()).find(|&index| self.row_key(index).as_deref() == Some(&key))
        });
        self.selected =
            found.unwrap_or_else(|| self.selected.min(self.row_count().saturating_sub(1)));
    }

    /// Reads the artists, the current level's list and its parent's.
    fn read_level(&mut self, library: &Library) -> Result<(), crate::library::LibraryError> {
        self.artists = library.artists()?;
        match &self.position {
            Position::Files
            | Position::Artists
            | Position::Recent
            | Position::Favourites
            | Position::Playlists
            | Position::Playlist { .. } => {}
            Position::Albums { artist } => self.albums = library.albums_of(artist)?,
            Position::Tracks { artist, album } => {
                self.tracks = library.tracks_of(artist, album)?;
                // The list `esc` returns to, which ascending does not re-read.
                self.albums = if self.browse_by == BrowseBy::Albums {
                    library.all_albums()?
                } else {
                    library.albums_of(artist)?
                };
            }
            Position::AllAlbums => self.albums = library.all_albums()?,
            Position::AllTracks => self.tracks = library.all_tracks()?,
            Position::Genres => self.genres = library.genres()?,
            Position::Genre { genre } => {
                self.tracks = library.tracks_of_genre(genre)?;
                self.genres = library.genres()?;
            }
        }
        Ok(())
    }

    /// What identifies row `index` of the current level across a reload.
    fn row_key(&self, index: usize) -> Option<String> {
        match &self.position {
            Position::Files => self.argv.get(index).map(|path| path.display().to_string()),
            Position::Artists => self.artists.get(index).map(|entry| entry.name.clone()),
            Position::Albums { .. } | Position::AllAlbums => self
                .albums
                .get(index)
                .map(|entry| format!("{}\u{0}{}", entry.artist, entry.title)),
            Position::Tracks { .. } | Position::AllTracks | Position::Genre { .. } => self
                .tracks
                .get(index)
                .map(|entry| entry.path.display().to_string()),
            // The recent list holds the file's entries (issue #33): a row's
            // identity is the entry's — the station key for a station, the
            // path for a file.
            Position::Recent => self
                .recent_rows
                .get(index)
                .map(crate::history::Entry::identity),
            // A favourite row's identity is its path, exactly as the store
            // holds it (issue #7).
            Position::Favourites => self
                .favourite_rows
                .get(index)
                .map(|favorite| favorite.path.clone()),
            // A playlist row's identity is its name (issue #4); a track row's
            // is its path, exactly as the store holds it.
            Position::Playlists => self
                .playlist_rows
                .get(index)
                .map(|playlist| playlist.name.clone()),
            Position::Playlist { .. } => self.open_playlist_tracks().get(index).cloned(),
            Position::Genres => self.genres.get(index).map(|entry| entry.name.clone()),
        }
    }

    /// Stands at the top of `by` and reads its list; the library tab comes
    /// to the front. The view is saved here (S3), but only when it changed:
    /// the launch restore and the reload fallback call this with the view
    /// the file already holds, and must write nothing.
    fn browse_view(&mut self, by: BrowseBy) {
        self.view = View::Library;
        self.browse_by = by;
        self.position = match by {
            BrowseBy::Artists => Position::Artists,
            BrowseBy::Albums => Position::AllAlbums,
            BrowseBy::Tracks => Position::AllTracks,
            BrowseBy::Genres => Position::Genres,
        };
        self.selected = 0;
        self.load_top();
        if by != self.saved.browse_by {
            self.saved.browse_by = by;
            self.remember(Some("state"), "browse_by", &Self::toml_string(by.label()));
        }
    }

    /// Reads the list of the view's top level from the index.
    fn load_top(&mut self) {
        let Some(library) = self.library else {
            return;
        };
        let loaded = match self.position {
            Position::Artists => library.artists().map(|rows| self.artists = rows),
            Position::AllAlbums => library.all_albums().map(|rows| self.albums = rows),
            Position::AllTracks => library.all_tracks().map(|rows| self.tracks = rows),
            Position::Genres => library.genres().map(|rows| self.genres = rows),
            _ => Ok(()),
        };
        if let Err(error) = loaded {
            self.status = format!("velo: {error}");
        }
    }

    /// Whether the radio view is what the keys act on: it is open, and the
    /// queue is not the screen the round trip left us on (issue #23) — while
    /// the queue is what the keys act on, the radio's keys must not act
    /// behind it. The help card also reads this to open on the radio's rows
    /// (issue #31).
    fn radio_on_screen(&self) -> bool {
        self.radio.is_some() && self.view != View::Queue
    }

    /// Applies one key press. Pure with respect to the terminal and the
    /// library: it only moves state and answers with an [`Action`].
    #[expect(
        clippy::too_many_lines,
        reason = "the keymap is one match read top to bottom; the overlays' keys live in their own handlers"
    )]
    pub fn on_key(&mut self, event: ratatui::crossterm::event::KeyEvent) -> Action {
        use ratatui::crossterm::event::{KeyCode, KeyModifiers};

        // An open search owns almost every key: `j`/`k` move through the
        // results, printable characters type in, `Esc` cancels. The browse
        // state is never touched while it is open.
        if self.search.is_some() {
            return self.on_search_key(event.code);
        }
        if self.library_prompt_open() {
            return self.on_prompt_key(event.code);
        }
        // The seek prompt owns what is typed into it, the way the library
        // prompt does (issue #6).
        if self.seek_prompt.is_some() {
            return self.on_seek_key(event.code);
        }
        // The playlist-name prompt owns what is typed into it (issue #4),
        // the way the seek prompt does.
        if self.playlist_prompt.is_some() {
            return self.on_playlist_prompt_key(event.code);
        }
        // The URL prompt owns what is typed into it (issue #42), the way the
        // seek prompt does.
        if self.url_prompt.is_some() {
            return self.on_url_key(event.code);
        }
        // The save prompt owns what is typed into it (issue #45), the way the
        // URL prompt does.
        if self.save_prompt.is_some() {
            return self.on_save_key(event.code);
        }
        // The append overlay owns every key too (issue #4): `j`/`k` move
        // between the saved names, `enter` applies, `esc` cancels.
        if self.playlist_picker.is_some() {
            return self.on_playlist_picker_key(event.code);
        }
        // The theme picker (issue #37) owns every key while it is open,
        // exactly as the settings card it opens from does.
        if self.theme_picker.is_some() {
            return self.on_theme_picker_key(event.code);
        }
        // The visualizer picker (issue #40) owns every key while it is
        // open, the same way.
        if self.visualizer_picker.is_some() {
            return self.on_visualizer_picker_key(event.code);
        }
        // The device picker (issue #44) owns every key while it is open,
        // the same way.
        if self.device_picker.is_some() {
            return self.on_device_picker_key(event.code);
        }
        // The full-screen visualizer (issue #40, S4) gives up every key but
        // its own two: `V` and `esc` leave it — to exactly the view it came
        // from — and everything else, the transport and the volume and `q`
        // included, reaches the interface underneath.
        if self.visualizer_fullscreen.is_some()
            && matches!(event.code, KeyCode::Char('V') | KeyCode::Esc)
        {
            self.close_visualizer_fullscreen();
            return Action::None;
        }
        // An open settings card owns every key too (S6): nothing reaches the
        // lists or the transport while it is up, `q` and `space` included.
        if self.settings.is_some() {
            return self.on_settings_key(event.code);
        }
        // The info card (issue #43): `ctrl+i` opens it over whatever screen
        // is on — the sources screen and the radio's lists included — and
        // while it is open it owns every key its own `esc` does not.
        if self.info.is_some() {
            return self.on_info_key(event.code);
        }
        if event.modifiers.contains(KeyModifiers::CONTROL) && event.code == KeyCode::Char('i') {
            self.open_info_card();
            return Action::None;
        }
        // An open help owns every key too, so `j`/`k` scroll it and never
        // the list underneath.
        if self.help.is_some() {
            return self.on_help_key(event.code);
        }
        // The radio view (T6a) owns its keys while it is open and on screen;
        // the keys it does not claim still reach the lists and the transport.
        // A queue on screen keeps it asleep (issue #23): the round trip holds
        // the radio alive underneath, and while the queue is what the keys
        // act on, the radio's keys must not act behind it.
        if self.radio_on_screen()
            && let Some(action) = self.radio_key(event)
        {
            return action;
        }
        // The sources screen (issue #3) owns its list keys while it is up;
        // the keys it does not claim keep reaching the transport and the
        // global commands.
        if self.view == View::Sources
            && let Some(action) = self.sources_key(event)
        {
            return action;
        }

        // The ctrl-modified keys (issue #38) sit between the views that own
        // their own lists and the plain keymap: every ctrl pair below would
        // otherwise collapse into the plain arm of its letter (`ctrl+j`
        // into `j`, `ctrl+d` into `d`), and an overlay above has already
        // kept them where it owns them (S2).
        if event.modifiers.contains(KeyModifiers::CONTROL) {
            match event.code {
                KeyCode::Char('c') => return Action::Quit,
                KeyCode::Char('k') => {
                    self.help = Some(Help::for_radio(
                        self.radio_on_screen(),
                        self.seen_area.get(),
                    ));
                    return Action::None;
                }
                KeyCode::Char('j') => {
                    self.seek_prompt = Some(SeekPrompt::default());
                    return Action::None;
                }
                KeyCode::Char('p') => {
                    self.settings = Some(SettingsCard::default());
                    return Action::None;
                }
                // `ctrl+s` (issue #45): saving the remote track the cursor
                // or the transport stands on, out of the cache. The local
                // file and the live station are refused inside, each with
                // its own line.
                KeyCode::Char('s') => {
                    self.ask_to_save_remote();
                    return Action::None;
                }
                KeyCode::Char('l') if !matches!(self.position, Position::Files) => {
                    self.ask_for_library(self.config_file.clone());
                    return Action::None;
                }
                KeyCode::Char('u') => {
                    self.select_page(false);
                    return Action::None;
                }
                KeyCode::Char('d') => {
                    self.select_page(true);
                    return Action::None;
                }
                // `ctrl+r` stays the radio's refresh and `ctrl+v` the
                // visualizer picker's; the plain keymap's own guards keep
                // them apart below.
                _ => {}
            }
        }

        match event.code {
            // `shift+↓`/`shift+↑` (issue #38) beside `J`/`K`: the same move of
            // the selected entry, in the queue or in a playlist's track list.
            // They must stand above the plain arrow arms, which a shifted
            // arrow would otherwise reach.
            KeyCode::Down if event.modifiers.contains(KeyModifiers::SHIFT) => {
                self.move_selection(1);
                Action::None
            }
            KeyCode::Up if event.modifiers.contains(KeyModifiers::SHIFT) => {
                self.move_selection(-1);
                Action::None
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.select_next();
                Action::None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.select_previous();
                Action::None
            }
            KeyCode::Char('g') | KeyCode::Home => {
                self.select_first();
                Action::None
            }
            KeyCode::Char('G') | KeyCode::End => {
                self.select_last();
                Action::None
            }
            KeyCode::Tab => {
                self.toggle_view();
                Action::None
            }
            KeyCode::Char('a') => {
                self.append_selection();
                Action::None
            }
            // `f` on a track row of the library views marks it, and again
            // unmarks it (issue #7). The radio view claimed its own `f`
            // above; the sources screen and the flat file list have no track
            // row to mark.
            KeyCode::Char('f')
                if self.view == View::Library
                    && matches!(
                        self.position,
                        Position::Tracks { .. } | Position::AllTracks | Position::Genre { .. }
                    ) =>
            {
                self.toggle_favourite();
                Action::None
            }
            // `A` (issue #38): add the selection to the queue and start
            // playing from the entry it landed on — the queue keeps its
            // order, the stream starts there. Nothing appended, nothing to
            // play: `append_selection` has already said why in the status
            // line, and the radio owns its own screen.
            KeyCode::Char('A') if self.radio.is_none() => {
                let before = self.queue.len();
                self.append_selection();
                if self.queue.len() > before {
                    Action::PlayQueue { from: before }
                } else {
                    Action::None
                }
            }
            // `S` in the queue view saves the queue as a new playlist
            // (issue #4, S3), asking for a name with the seek prompt's card.
            KeyCode::Char('S') if self.view == View::Queue => {
                self.ask_to_save_queue_as_playlist();
                Action::None
            }
            // `S` on a library track row appends the row's track to a
            // playlist chosen from the saved names (issue #4, S3).
            KeyCode::Char('S')
                if self.view == View::Library
                    && matches!(
                        self.position,
                        Position::Tracks { .. } | Position::AllTracks | Position::Genre { .. }
                    ) =>
            {
                self.ask_to_append_to_playlist();
                Action::None
            }
            KeyCode::Char('x') if self.view == View::Queue => {
                self.remove_from_queue();
                Action::None
            }
            // `x` on a playlist row of the names list deletes it (issue #4,
            // S5), and the status line says so.
            KeyCode::Char('x') if matches!(self.position, Position::Playlists) => {
                self.delete_playlist();
                Action::None
            }
            // `x` where nothing can be removed — a library track row, a
            // playlist's own track list — says so instead of doing nothing
            // (issue #38, S4); the radio's rows are not removable either.
            KeyCode::Char('x') if self.radio.is_none() => {
                "velo: nothing to remove here".clone_into(&mut self.status);
                Action::None
            }
            // `e` on a playlist row of the names list renames it (issue #4,
            // S5), with the seek prompt's card; `esc` cancels.
            KeyCode::Char('e') if matches!(self.position, Position::Playlists) => {
                self.ask_to_rename_playlist();
                Action::None
            }
            KeyCode::Char('J') if self.view == View::Queue => {
                self.move_in_queue(1);
                Action::None
            }
            KeyCode::Char('K') if self.view == View::Queue => {
                self.move_in_queue(-1);
                Action::None
            }
            // Inside a playlist's track list (issue #4, S4): the queue's own
            // reordering keys, saved to the store at once. The queue's arms
            // above take the queue view; a playlist's list is the library
            // view's rows, and the store is the queue's only constraint —
            // there is no playing entry to protect here.
            KeyCode::Char('J') if matches!(self.position, Position::Playlist { .. }) => {
                self.move_in_playlist(1);
                Action::None
            }
            KeyCode::Char('K') if matches!(self.position, Position::Playlist { .. }) => {
                self.move_in_playlist(-1);
                Action::None
            }
            // `pgdn`/`pgup` beside `g`/`G` (issue #38): a page of rows in
            // every list these keys act on. A page never wraps — jumping
            // from the last page to the first reads as a mistake, the way
            // it does in the queue's `j`/`k`.
            KeyCode::PageDown => {
                self.select_page(true);
                Action::None
            }
            KeyCode::PageUp => {
                self.select_page(false);
                Action::None
            }
            // `shift+↓`/`shift+↑` beside `J`/`K` (issue #38): the same move,
            // without leaving the arrow keys.
            KeyCode::Enter if self.view == View::Queue => {
                if self.queue.is_empty() {
                    Action::None
                } else {
                    Action::PlayQueue {
                        from: self.queue_selected,
                    }
                }
            }
            KeyCode::Enter => self.enter(),
            KeyCode::Char(key @ '1'..='4') if self.in_library() => {
                BrowseBy::from_key(key).map_or(Action::None, Action::Browse)
            }
            KeyCode::Char(' ') => Action::TogglePause,
            // The second seek gear (issue #6): shift on an arrow, which a
            // terminal does report as a modifier, unlike a character key that
            // arrives already shifted.
            KeyCode::Left if event.modifiers.contains(KeyModifiers::SHIFT) => {
                Action::Seek { delta_secs: -30 }
            }
            KeyCode::Right if event.modifiers.contains(KeyModifiers::SHIFT) => {
                Action::Seek { delta_secs: 30 }
            }
            KeyCode::Char(':') => {
                self.seek_prompt = Some(SeekPrompt::default());
                Action::None
            }
            // The plain keys keep the coarse five points; the brackets are the
            // fine gear, a point a press (issue #16). Shift cannot carry it: a
            // terminal sends the shifted *character*, and on a US layout
            // `shift`+`=` arrives as the `+` that already means five points
            // up, so only the down side would have a modifier to read.
            KeyCode::Char('+' | '=') => Action::SetVolume(self.change_volume(5)),
            KeyCode::Char('-') => Action::SetVolume(self.change_volume(-5)),
            KeyCode::Char(']') => Action::SetVolume(self.change_volume(1)),
            KeyCode::Char('[') => Action::SetVolume(self.change_volume(-1)),
            KeyCode::Left => Action::Seek { delta_secs: -5 },
            KeyCode::Right => Action::Seek { delta_secs: 5 },
            // The cliamp transport (issue #38): next is `>`, `.` or `n`,
            // previous is `<` or `,` — the comma keys no longer seek, and
            // `p` no longer skips: it opens the queue, below.
            KeyCode::Char('>' | '.' | 'n') => Action::Next,
            KeyCode::Char('<' | ',') => Action::Previous,
            // The queue view, the way `tab` opens it — cliamp's `p` (issue
            // #38); `tab` remains the round trip back out.
            KeyCode::Char('p') => {
                self.open_queue();
                Action::None
            }
            // Shuffle (issue #38): cliamp's `z`, where `s` used to sit.
            KeyCode::Char('z') => Action::ToggleShuffle,
            // The mono downmix (issue #8, S2): a session setting, `m` on and
            // off (issue #38 moved it here from `M`). The refusal under
            // bit-perfect lives in `apply`, with the rest of the answers.
            KeyCode::Char('v') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.open_visualizer_picker();
                Action::None
            }
            // The visualizer (issue #40, S3): `v` cycles through every
            // shape, `none` included, wrapping back to `bars`, and the
            // choice is written the way `z` writes shuffle.
            KeyCode::Char('v') => {
                self.set_visualizer(self.ui.visualizer.next());
                Action::None
            }
            // `V` takes the visualizer to the whole body (issue #40, S4);
            // it leaves again with `V` or `esc`, handled above.
            KeyCode::Char('V') => {
                self.open_visualizer_fullscreen();
                Action::None
            }
            KeyCode::Char('m') => Action::ToggleMono,
            // Low power (issue #11, S1): a session switch on and off, and
            // the key that remembers it under `[ui]`, the way `z` remembers
            // shuffle under `[state]`.
            KeyCode::Char('P') => Action::ToggleLowPower,
            // The library folder prompt (issue #38): cliamp's `o`, with
            // `ctrl+l` beside it; `L` now opens the saved playlists. The
            // files given on the command line have no folder to set. It can
            // be set again at any time (library-refresh T1).
            KeyCode::Char('o') if !matches!(self.position, Position::Files) => {
                self.ask_for_library(self.config_file.clone());
                Action::None
            }
            // The audio output picker (issue #44, S1): `d` lists every
            // output the machine has, the one in use marked; `enter` moves
            // the playback there, `esc` closes without changing anything.
            KeyCode::Char('d') => {
                self.open_device_picker();
                Action::None
            }
            // The saved playlists source (issue #38): cliamp's `L`. The
            // radio keeps its own screen underneath — opening a source over
            // it would leave it half asleep.
            KeyCode::Char('L')
                if !matches!(self.position, Position::Files) && self.radio.is_none() =>
            {
                self.open_playlists();
                Action::None
            }
            // The URL prompt (issue #42): a one-off `http`/`https` URL — a
            // plain stream, or a wrapper with a single entry, plays live;
            // a wrapper listing several songs opens as those songs. Nothing
            // is persisted: no station in `radio.toml`.
            KeyCode::Char('u') => {
                self.url_prompt = Some(UrlPrompt::default());
                Action::None
            }
            // Refreshing the index moved here from `u` (issue #42), which
            // now opens the URL prompt: `I` beside `i`, which in the radio
            // reports a station. The command-line list has no index.
            KeyCode::Char('I') if !matches!(self.position, Position::Files) => {
                self.ask_to_reindex()
            }
            // `ctrl+r` is the radio's refresh (issue #25): the keymap must
            // not collapse the modified key into the plain arm, so a
            // `ctrl`-modified `r` never cycles the repeat.
            KeyCode::Char('r') if !event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.repeat = self.repeat.next();
                Action::SetRepeat(self.repeat)
            }
            KeyCode::Char('?') => {
                self.help = Some(Help::for_radio(
                    self.radio_on_screen(),
                    self.seen_area.get(),
                ));
                Action::None
            }
            // The radio view (T6a): `R` opens it closed, and its own key
            // handler closes it again once it is open.
            KeyCode::Char('R') => self.open_radio(),
            KeyCode::Char('/') if self.library.is_some() => {
                self.open_search();
                Action::None
            }
            KeyCode::Esc | KeyCode::Backspace => {
                // Ascending is browse-only; an open help took `Esc` above.
                if matches!(self.position, Position::Files) {
                    Action::None
                } else {
                    Action::Ascend
                }
            }
            KeyCode::Char('q') => Action::Quit,
            _ => Action::None,
        }
    }

    /// The keys the sources screen owns (issue #3); `None` lets the key
    /// fall through to the rest of the keymap, so the transport, `q`, `?`,
    /// `z`, `r` and the seek keys stay global (S7).
    fn sources_key(&mut self, event: ratatui::crossterm::event::KeyEvent) -> Option<Action> {
        use ratatui::crossterm::event::{KeyCode, KeyModifiers};
        let code = event.code;

        match code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.sources.select_next();
                Some(Action::None)
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.sources.select_previous();
                Some(Action::None)
            }
            // The sources list is short: a page covers it (issue #38), so
            // the paging keys land on its ends — `ctrl+d`/`ctrl+u` beside
            // them, or they would page the list underneath.
            KeyCode::Char('g') | KeyCode::Home | KeyCode::PageUp => {
                self.sources.select_first();
                Some(Action::None)
            }
            KeyCode::Char('G') | KeyCode::End | KeyCode::PageDown => {
                self.sources.select_last();
                Some(Action::None)
            }
            KeyCode::Char('u') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.sources.select_first();
                Some(Action::None)
            }
            KeyCode::Char('d') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.sources.select_last();
                Some(Action::None)
            }
            KeyCode::Enter => Some(self.open_source()),
            // The way out is where you are (S6): `Esc` stays on the sources
            // screen, which is already the top. The library's keys are not
            // this screen's to act on (S7): while it is up they arrive as
            // no-ops. `Tab` is the round trip (issue #23): it falls through
            // to the keymap, which takes it to the queue and back.
            KeyCode::Esc
            | KeyCode::Backspace
            | KeyCode::Char('1'..='4' | 'a' | 'A' | 'x' | 'J' | 'K' | '/') => Some(Action::None),
            _ => None,
        }
    }

    /// The keys the open radio view owns (T6a); `None` lets the key fall
    /// through to the rest of the keymap.
    fn radio_key(&mut self, event: ratatui::crossterm::event::KeyEvent) -> Option<Action> {
        let outcome = self.radio.as_mut()?.key(event);
        // The wiring follows every key at once (T6b): a mutation saves,
        // an entered section starts its request. Nothing here blocks: a
        // request runs on its own thread and lands through the tick, the
        // way the library index does.
        self.radio_poll();
        match outcome {
            radio::RadioOutcome::Handled => Some(Action::None),
            radio::RadioOutcome::Close => {
                self.radio = None;
                // Closing lands where the radio was opened from (S6): the
                // sources screen when it was chosen there, the library or the
                // queue when `R` opened it over them — the way out is the way
                // in. The flat command-line list has no sources screen, and
                // the radio can only be opened over the library there.
                self.view = self.radio_from;
                Some(Action::None)
            }
            radio::RadioOutcome::ToQueue => {
                // The round trip keeps the radio alive (issue #23): the
                // queue shows over it, and `tab` returns to the screen the
                // radio was opened from, its place and its loaded list
                // untouched. Its keys sleep while the queue is on screen.
                self.tab_origin = self.radio_from;
                self.view = View::Queue;
                Some(Action::None)
            }
            radio::RadioOutcome::Play {
                url,
                name,
                configured,
            } => {
                // A configured station's URL is resolved before anything is
                // queued (cliamp S1): a multi-entry wrapper opens as the
                // songs it lists, a wrapper that cannot be expanded is one
                // refusal line, and anything else plays live as before.
                // The verdict is remembered for the session (S4, issue #34):
                // a station is expanded once, and opening it again fetches
                // nothing. Only a Songs verdict is remembered — a FAILED
                // expansion stays retryable, and a plain stream never needed
                // the probe.
                let expansion = if configured {
                    match self.expansion_cache.get(&url) {
                        Some(cached) => Ok(cached.clone()),
                        None => {
                            match crate::radio::stream::expand(
                                &url,
                                crate::radio::stream::Timeouts::default(),
                            ) {
                                Ok(songs @ crate::radio::stream::Expansion::Songs(_)) => {
                                    self.expansion_cache.insert(url.clone(), songs.clone());
                                    Ok(songs)
                                }
                                other => other,
                            }
                        }
                    }
                } else {
                    Ok(crate::radio::stream::Expansion::Live)
                };
                Some(self.play_station(url, name, expansion))
            }
            radio::RadioOutcome::Queue { url, name } => {
                self.queue_station(url, name);
                Some(Action::None)
            }
            radio::RadioOutcome::PlaySongs { songs, from } => {
                // The cliamp songs are finite remote files (issue #48, S3):
                // they join the queue as file entries — the URL is the
                // path, the resolver caches them — and the player starts
                // at the selected one, the way `enter` on a queue row
                // plays from it. Never a live station (S3): the transport
                // shows a position, not `● live`.
                let start = self.queue.len() + from;
                for song in &songs {
                    if !song.title.is_empty() {
                        self.song_titles
                            .insert(song.url.clone(), song.title.clone());
                    }
                }
                self.edit_queue(|queue| {
                    queue.extend(
                        songs
                            .into_iter()
                            .map(|song| Entry::File(PathBuf::from(song.url))),
                    );
                });
                Some(Action::PlayQueue { from: start })
            }
            radio::RadioOutcome::QueueSong { url, title } => {
                if !title.is_empty() {
                    self.song_titles.insert(url.clone(), title.clone());
                }
                self.edit_queue(|queue| queue.push(Entry::File(PathBuf::from(url))));
                self.status = format!("velo: added {}", plural(1, "track", "tracks"));
                Some(Action::None)
            }
            radio::RadioOutcome::ReportIssue { title, body } => {
                self.open_issue_report(&title, &body);
                Some(Action::None)
            }
            radio::RadioOutcome::Pass => None,
        }
    }

    /// Plays a station now: the status line names it before the player has
    /// answered, the station joins the queue, and the play action goes to
    /// the shell, which builds the player the way `enter` on a queue row
    /// does (T6c). The queue edit is shared with the radio view's queue
    /// verb — and, since issue #33, with the `Recently played` screen.
    ///
    /// `expansion` is the configured station's URL resolution (cliamp S1,
    /// computed by the caller so the tests can inject it): a multi-entry
    /// wrapper opens as the songs it lists — one file row per entry, in the
    /// wrapper's order, each playable and seekable as a track, its identity
    /// the song's `https://…` URL and never a cache path (S3) — a refusal
    /// is one status line naming the station and the reason, with nothing
    /// queued (S5), and anything else plays live exactly as before (S6).
    fn play_station(
        &mut self,
        url: String,
        name: String,
        expansion: Result<crate::radio::stream::Expansion, crate::radio::stream::StreamError>,
    ) -> Action {
        // The stream can take a while to open (S10): the status names the
        // station now, before the player has answered. `start_stream`'s
        // success clears the line and its failure names the error, so a
        // stale "opening" never lingers.
        self.status = format!("velo: opening {name}…");
        match expansion {
            Ok(crate::radio::stream::Expansion::Songs(songs)) => {
                let from = self.queue.len();
                // The titles the wrapper gave the songs are remembered for
                // the session (issue #34): the map is filled here, where the
                // expansion happens, and consulted by [`describe`] alone —
                // no title is threaded through the queue or the audio path.
                for song in &songs {
                    if let Some(title) = &song.title {
                        self.song_titles.insert(song.url.clone(), title.clone());
                    }
                }
                self.edit_queue(|queue| {
                    queue.extend(
                        songs
                            .into_iter()
                            .map(|song| Entry::File(PathBuf::from(song.url))),
                    );
                });
                return Action::PlayQueue { from };
            }
            // A wrapper that cannot be expanded (cliamp S5): one status line
            // naming the station and the reason, and nothing queued — never
            // an empty screen that looks like a station with no songs.
            Err(error) => {
                self.status = format!("velo: {name}: {error}");
                return Action::None;
            }
            Ok(crate::radio::stream::Expansion::Live) => {}
        }
        self.edit_queue(|queue| queue.push(Entry::Station { url, name }));
        let from = self.queue.len() - 1;
        Action::PlayQueue { from }
    }

    /// Appends a station to the queue without starting anything (T6c, issue
    /// #33).
    fn queue_station(&mut self, url: String, name: String) {
        self.edit_queue(|queue| queue.push(Entry::Station { url, name }));
        self.status = format!("velo: added {}", plural(1, "track", "tracks"));
    }

    /// The refusal of the station the radio's cursor rests on, as the status
    /// line says it: `velo: <name> cannot play: <reason>` (S1, issue #19).
    /// `None` when the cursor is not on a catalog station the filter
    /// rejected — favourites and hand-added stations skip the filter by
    /// design, so their playing is never a refusal.
    fn radio_refusal_message(&self) -> Option<String> {
        let view = self.radio.as_ref()?;
        let reason = view.selected_refusal()?;
        let name = view.selection().and_then(|row| match row.kind {
            radio::RowKind::CatalogStation { station, .. } => Some(station.name.clone()),
            _ => None,
        })?;
        // The reason is the sentence the report carries; on a status line that
        // one row wide it would be cut before the key, so the tag goes here and
        // the sentence stays in the report (S1, S3).
        let tag = view
            .selection()
            .and_then(|row| match row.kind {
                radio::RowKind::CatalogStation { station, .. } => {
                    crate::radio::directory::refusal_tag(&station)
                }
                _ => None,
            })
            .unwrap_or(reason);
        Some(format!("velo: {name} cannot play ({tag}) — i reports it"))
    }

    /// Writes the refusal note onto the status line while the radio's cursor
    /// rests on a rejected station and takes it off when the cursor leaves
    /// (S1, issue #19). The note is compared against what was last shown, so
    /// it is written only when it changes, and the line is only cleared when
    /// it still carries the note — a message another writer put up
    /// (`velo: opening …`, a wire error) is never wiped.
    fn radio_refusal_status(&mut self) {
        let message = self.radio_refusal_message();
        if message == self.radio_refusal {
            return;
        }
        match (&message, &self.radio_refusal) {
            // The cursor arrived on a rejected row: the note takes the line.
            (Some(new), _) => self.status = new.clone(),
            // The cursor left: the line is taken back only if it still
            // carries the note.
            (None, Some(shown)) => {
                if self.status == *shown {
                    self.status.clear();
                }
            }
            (None, None) => {}
        }
        self.radio_refusal = message;
    }

    /// Hands a radio report to the browser (S3, issue #19). The URL is
    /// built here — the version and the platform are the shell's to know —
    /// and given to the system opener without waiting on it. velo holds no
    /// token and posts nothing: the browser shows the draft, and the human
    /// reads it and submits. If the opener fails, the URL itself takes the
    /// status line, so the report can still be filed by hand — nothing is
    /// ever lost. The report's own status write does not fight the refusal
    /// note: that note is only rewritten when it changes, and only cleared
    /// when the line still carries it ([`App::radio_refusal_status`]).
    fn open_issue_report(&mut self, title: &str, body: &str) {
        let body = format!(
            "{body}\n\nvelo {} on {} ({})",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH,
        );
        let url = issue_url(title, &body);
        if (self.url_opener)(&url).is_ok() {
            self.status = String::from(
                "velo: the report is open in your browser — reading it and sending it is yours to do",
            );
        } else {
            self.status =
                format!("velo: the browser did not open — the report to file by hand: {url}");
        }
    }

    /// Runs one turn of the radio wiring (T6b): collects finished
    /// requests, starts the ones the view's place needs, saves store
    /// mutations, and reports progress and failures on the status line.
    /// The waiting note `⟳ loading…` is written when a request starts and
    /// cleared when the answer or the error lands (S10), so it never
    /// sticks after the wait is over; any other status write keeps its
    /// place and is never wiped by the clearing.
    /// Never blocks — the request runs on a worker thread and lands
    /// through a channel, the pattern the library index uses.
    fn radio_poll(&mut self) {
        // The playing fact is the shell's to give (S15, issue #32): the view
        // is told, never guesses. Whatever the transport stands on reaches
        // the view: a station by its URL, a song by the path the queue
        // carries — which for a channel's song is the song's URL (issue
        // #59) — so each marks its own row. A local file's path matches no
        // radio row and marks none. radio_poll runs on every tick and after
        // every radio key, so no key can leave the marker stale.
        let playing = self.standing_entry().map(|entry| match entry {
            Entry::Station { url, .. } => url.clone(),
            Entry::File(path) => path.to_string_lossy().into_owned(),
        });
        if let Some(view) = self.radio.as_mut() {
            view.set_playing(playing.map(|identity| (identity, self.state)));
        }
        if let Some((wire, view)) = self.radio_wire.as_mut().zip(self.radio.as_mut()) {
            wire.poll(view);
            if let Some(message) = wire.take_message() {
                self.status = message;
            } else if wire.loading() {
                self.status = String::from("⟳ loading…");
            } else if self.status == "⟳ loading…" {
                self.status.clear();
            }
        }
        // The refusal note follows the cursor (S1, issue #19): said while it
        // rests on a station the filter rejected, gone when it leaves. It is
        // written after the wiring's own messages, and only when it changes,
        // so no tick rewrites the line and no other writer's message is
        // disturbed.
        self.radio_refusal_status();
    }

    /// The fetching fact on the status line (issue #60): while the queue is
    /// fetching a remote entry it has not cached yet, the line names it —
    /// `⟳ fetching <name>…` — and clears when the entry starts producing
    /// sound. A cache hit is instant and says nothing; a failed fetch keeps
    /// its own report, because the clearing only ever removes the fetching
    /// line itself, never another writer's message.
    fn fetch_status(&mut self) {
        // The wait a start handed to its worker (issue #60's second half):
        // the line names the entry for as long as the resolve runs, the way
        // the producer's fetching cell names one later in the queue. The
        // start itself, when the worker answers, decides what stands
        // afterwards; a tick never clears the line from under the wait.
        if let Some(pending) = self.pending_resolve.as_ref() {
            let line = format!("⟳ fetching {}…", self.fetching_name(&pending.path));
            if self.status != line {
                self.status = line;
            }
            return;
        }
        let fetching = self.fetching_entry().map(|entry| {
            let path = entry.as_path().unwrap_or(Path::new(""));
            format!("⟳ fetching {}…", self.fetching_name(path))
        });
        if let Some(line) = fetching {
            if self.status != line {
                self.status = line;
            }
            return;
        }
        if !self.status.starts_with("⟳ fetching ") {
            return;
        }
        // The fetch is over; what may be left is the wait for sound: a
        // track still in its preroll keeps the line until the first samples
        // are heard. Stopped, there is nothing to wait for.
        let sounding = match (&self.player, self.transport_fixture.as_ref()) {
            (Some(player), _) => player.position() > Duration::ZERO,
            (None, Some(fixture)) => fixture.position > Duration::ZERO,
            (None, None) => true,
        };
        if self.state == PlayState::Stopped || sounding {
            self.status.clear();
        }
    }

    /// The entry the queue is fetching right now, when one is: the
    /// transport's fetching cell names the entry it is resolving — the real
    /// player's cell ([`Player::fetching_index`]), the fixture's under test —
    /// and the resolver's own [`PathResolver::cached`], the seam and never a
    /// re-derivation of its rules, says whether the queue actually waits for
    /// it. Only a remote entry can be uncached: a local file is already on
    /// disk.
    fn fetching_entry(&self) -> Option<&Entry> {
        if self.state == PlayState::Stopped {
            return None;
        }
        let index = match &self.player {
            Some(player) => player.fetching_index()?,
            None => self.transport_fixture.as_ref()?.fetching?,
        };
        let entry = self.queue.get(index)?;
        let path = entry.as_path()?;
        if !self.is_uncached_remote(path) {
            return None;
        }
        Some(entry)
    }

    /// Whether reaching `path` would block this thread on a download (issue
    /// #60): a remote entry — a `subsonic://` row or a channel song's URL —
    /// that the resolver has not cached yet, its own
    /// [`PathResolver::cached`] seam answering and never a re-derivation of
    /// its rules. A local file is already on disk, and without a resolver
    /// nothing waits (today's behaviour). The one question that decides
    /// where a first entry is resolved: on this thread, or on a worker the
    /// tick polls.
    fn is_uncached_remote(&self, path: &Path) -> bool {
        if !is_pseudo_path(path) && !is_remote_url(path) {
            return false;
        }
        self.resolver
            .as_ref()
            .is_some_and(|resolver| !resolver.cached(path))
    }

    /// The name a fetching line carries: the title the wrapper gave the song
    /// this session (`song_titles`, issue #34) when one is known, otherwise
    /// the file's own name — exactly what a failure report names.
    fn fetching_name(&self, path: &Path) -> String {
        if let Some(title) = self.song_titles.get(path.to_string_lossy().as_ref()) {
            return title.clone();
        }
        path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
    }

    /// Says the fetching fact for the entry a stream is about to resolve at
    /// selection time (issue #60): the first entry of a fresh stream, a skip
    /// or a seek is resolved on this thread before the player exists, and an
    /// uncached remote one there is a download the user waits for — so the
    /// status line carries `⟳ fetching <name>…` before the wait begins, the
    /// way the opening note does. A cache hit is instant and says nothing.
    /// The line clears when the track starts sounding ([`Self::fetch_status`]),
    /// and a failure's own report replaces it.
    fn note_fetching(&mut self, entry: Option<&Entry>) {
        let Some(Entry::File(path)) = entry else {
            return;
        };
        if !self.is_uncached_remote(path) {
            return;
        }
        let name = self.fetching_name(path);
        self.status = format!("⟳ fetching {name}…");
    }

    /// Hands the first entry's resolve to a worker (issue #60's second
    /// half): the resolve is a download the event loop must never wait for,
    /// so it runs here, the status line says `⟳ fetching <name>…` — the
    /// caller's gate (`is_uncached_remote`) has just said so — and `apply`
    /// goes on, the loop painting while the worker fetches. One slot: the
    /// caller ahead of this one replaces whatever waited, and its answer is
    /// dropped unread — a download it started lands in the cache and is
    /// simply reused. Polled by [`Self::poll_pending_resolve`].
    fn start_as_pending(&mut self, path: PathBuf, start: PendingStart) {
        let name = self.fetching_name(&path);
        self.status = format!("⟳ fetching {name}…");
        let seam = self.resolver.clone();
        let queued = path.clone();
        let (send, answer) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let resolved = match seam {
                Some(seam) => seam.resolve(&queued),
                None => Ok(queued),
            };
            // A dropped receiver — a superseded resolve — answers into
            // nothing and is gone; the download, if it ran, already landed
            // in the cache.
            let _ = send.send(resolved.map_err(|error| error.to_string()));
        });
        self.pending_resolve = Some(PendingResolve {
            path,
            answer,
            start,
        });
    }

    /// Polls the pending resolve (issue #60's second half), once per tick:
    /// still running keeps the line; a file on disk runs the same start the
    /// request named, the resolver now finding a cache hit, exactly as the
    /// synchronous start always opened it; a failure is one line naming the
    /// reason and no stream — the queue stands where it was, never advanced
    /// behind the user's back, and the state goes stopped the way a start
    /// failing on this thread always has. A worker that died before it
    /// answered — a panic inside a resolver — is a failed fetch with its own
    /// sentence, never an eternal wait.
    fn poll_pending_resolve(&mut self) {
        let Some(pending) = self.pending_resolve.as_ref() else {
            return;
        };
        let answer = match pending.answer.try_recv() {
            // The download runs on; the line stays.
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err(String::from("the fetch was interrupted")),
            Ok(answer) => answer,
        };
        let pending = self.pending_resolve.take().expect("checked above");
        match answer {
            Ok(_) => match pending.start {
                PendingStart::Stream { paths, playback } => self.open_stream(paths, playback),
                PendingStart::TrackAt { track, offset } => self.open_track_at(track, offset),
            },
            Err(reason) => {
                self.pending_resolve = None;
                self.status = format!("velo: {reason}");
                self.player = None;
                self.playing = None;
                self.state = PlayState::Stopped;
            }
        }
    }

    /// Carries an [`Action`] into the audio or library half. Errors land in
    /// the status message instead of leaving the interface.
    pub fn apply(&mut self, action: Action) {
        match action {
            Action::None => {}
            Action::SetVolume(percent) => {
                self.set_volume(percent);
                // Saved when it leaves what the file says (S2): `+` at the
                // launch volume writes nothing.
                if percent != self.saved_volume {
                    self.saved_volume = percent;
                    self.remember(Some("state"), "volume", &percent.to_string());
                    // The first `[state] volume` moves the legacy top-level
                    // line out of the file (S2.5); afterwards this removes
                    // nothing and writes nothing.
                    self.forget(None, "volume");
                }
            }
            Action::Quit => self.shutdown(),
            Action::Play { from } if self.search.is_some() => self.play_search_from(from),
            Action::Play { from } => self.play_from(from),
            Action::PlayQueue { from } => self.play_queue_from(from),
            Action::Descend => self.descend(),
            Action::Ascend => self.ascend(),
            Action::TogglePause => self.toggle_pause(),
            Action::Seek { delta_secs } => self.seek_by(delta_secs),
            Action::SeekTo { secs } => self.seek_to(Duration::from_secs(secs)),
            Action::Next => self.skip(true),
            Action::Previous => self.skip(false),
            Action::ToggleShuffle => self.toggle_shuffle(),
            Action::ToggleMono => self.toggle_mono(),
            // Low power (issue #11, S1): the screen keeps its layout with
            // the spectrum row gone, playback and every other key unchanged.
            Action::ToggleLowPower => self.toggle_low_power(),
            Action::SetRepeat(mode) => {
                if let Some(player) = &self.player {
                    player.set_repeat(mode);
                }
                if mode != self.saved.repeat {
                    self.saved.repeat = mode;
                    self.remember(Some("state"), "repeat", &Self::toml_string(mode.label()));
                }
            }
            Action::SetLibrary => self.accept_library(),
            Action::CompleteLibraryPath => self.complete_library_path(),
            Action::Browse(by) => {
                self.browse_view(by);
                // A number key from the queue tab brings the library tab
                // back with the view. The view is written inside
                // `browse_view`; the tab itself is no longer written (S14).
            }
            // Handed to the shell, which owns the thread.
            Action::Reindex => self.library_root.clone_from(&self.refresh.root),
        }
    }

    /// What `Enter` asks for in the library view: play a track list from the
    /// selection, or open the selected artist, album or genre.
    fn enter(&mut self) -> Action {
        let play = Action::Play {
            from: self.selected,
        };
        match &self.position {
            // A station row plays live through the same route the radio view
            // uses (issue #33); a file row plays from it like any other
            // track list.
            Position::Recent => {
                let Some(entry) = self.recent_rows.get(self.selected).cloned() else {
                    return Action::None;
                };
                if entry.is_station() {
                    // A station in the history plays live (S6): the
                    // resolution that opens a channel's songs belongs to the
                    // radio view's configured rows.
                    self.play_station(
                        entry.path,
                        entry.title.unwrap_or_default(),
                        Ok(crate::radio::stream::Expansion::Live),
                    )
                } else {
                    play
                }
            }
            Position::Files if !self.argv.is_empty() => play,
            Position::Tracks { .. } | Position::AllTracks | Position::Genre { .. }
                if !self.tracks.is_empty() =>
            {
                play
            }
            Position::Favourites if !self.favourite_rows.is_empty() => play,
            Position::Playlists if !self.playlist_rows.is_empty() => Action::Descend,
            Position::Playlist { .. } if !self.open_playlist_tracks().is_empty() => play,
            Position::Artists if !self.artists.is_empty() => Action::Descend,
            Position::Albums { .. } | Position::AllAlbums if !self.albums.is_empty() => {
                Action::Descend
            }
            Position::Genres if !self.genres.is_empty() => Action::Descend,
            _ => Action::None,
        }
    }

    /// The keys while the library prompt is open: they edit the path, `Enter`
    /// asks to accept it, `Esc` closes the prompt.
    fn on_prompt_key(&mut self, code: ratatui::crossterm::event::KeyCode) -> Action {
        use ratatui::crossterm::event::KeyCode;

        let Some(setup) = &mut self.library_setup else {
            return Action::None;
        };
        let Some(prompt) = &mut setup.prompt else {
            return Action::None;
        };
        match code {
            KeyCode::Esc => {
                setup.prompt = None;
                Action::None
            }
            KeyCode::Enter => Action::SetLibrary,
            KeyCode::Tab => Action::CompleteLibraryPath,
            KeyCode::Backspace => {
                // `~/` stays: the library is always a folder under home.
                if prompt.input.len() > complete::PROMPT_PREFIX.len() {
                    prompt.input.pop();
                }
                prompt.error = None;
                prompt.candidates.clear();
                Action::None
            }
            KeyCode::Char(character) => {
                prompt.input.push(character);
                prompt.error = None;
                prompt.candidates.clear();
                Action::None
            }
            _ => Action::None,
        }
    }

    /// The keys while the seek prompt is open (issue #6): printable keys type
    /// into it, `backspace` deletes, `enter` answers, `esc` closes it. Nothing
    /// reaches the transport underneath, and a refused answer leaves the
    /// prompt open with the reason under what was typed.
    fn on_seek_key(&mut self, code: ratatui::crossterm::event::KeyCode) -> Action {
        use ratatui::crossterm::event::KeyCode;

        match code {
            KeyCode::Esc => {
                self.seek_prompt = None;
                Action::None
            }
            KeyCode::Backspace => {
                if let Some(prompt) = self.seek_prompt.as_mut() {
                    prompt.input.pop();
                    prompt.error = None;
                }
                Action::None
            }
            KeyCode::Char(character) => {
                if let Some(prompt) = self.seek_prompt.as_mut() {
                    prompt.input.push(character);
                    prompt.error = None;
                }
                Action::None
            }
            KeyCode::Enter => self.answer_seek_prompt(),
            _ => Action::None,
        }
    }

    /// Answers the seek prompt: a position on the clock becomes the jump, and a
    /// percentage is turned into one here, where the track's length is known
    /// (issue #6). A refused answer stays in the card with its reason.
    fn answer_seek_prompt(&mut self) -> Action {
        let Some(typed) = self.seek_prompt.as_ref().map(|prompt| prompt.input.clone()) else {
            return Action::None;
        };
        let answer = match parse_seek(&typed) {
            Ok(SeekPosition::Secs(secs)) => Ok(secs),
            Ok(SeekPosition::Percent(percent)) => self
                .current_track_duration()
                .map(|duration| duration.mul_f64(f64::from(percent) / 100.0).as_secs())
                .ok_or("a percentage needs a track that knows its length"),
            Err(reason) => Err(reason),
        };
        match answer {
            Ok(secs) => {
                self.seek_prompt = None;
                Action::SeekTo { secs }
            }
            Err(reason) => {
                if let Some(prompt) = self.seek_prompt.as_mut() {
                    prompt.error = Some(reason.to_owned());
                }
                Action::None
            }
        }
    }

    /// The keys while the URL prompt is open (issue #42): printable keys type
    /// into it, `backspace` deletes, `enter` answers, `esc` closes it. The
    /// same shape the seek prompt has (issue #6).
    fn on_url_key(&mut self, code: ratatui::crossterm::event::KeyCode) -> Action {
        use ratatui::crossterm::event::KeyCode;

        match code {
            KeyCode::Esc => {
                self.url_prompt = None;
                Action::None
            }
            KeyCode::Backspace => {
                if let Some(prompt) = self.url_prompt.as_mut() {
                    prompt.input.pop();
                    prompt.error = None;
                }
                Action::None
            }
            KeyCode::Char(character) => {
                if let Some(prompt) = self.url_prompt.as_mut() {
                    prompt.input.push(character);
                    prompt.error = None;
                }
                Action::None
            }
            KeyCode::Enter => self.answer_url_prompt(),
            _ => Action::None,
        }
    }

    /// Answers the URL prompt (issue #42): the URL is resolved exactly the
    /// way a configured station is — [`App::play_station`], cliamp S1 — so
    /// a plain stream, or a wrapper with a single entry, plays live as an
    /// `Entry::Station`, and a wrapper with several entries opens as the
    /// finite list of its songs, one `Entry::File` per song whose path is
    /// the URL the cache resolver fetches. Nothing is persisted. An answer
    /// that is not an `http`/`https` URL stays in the card with its reason;
    /// a URL the resolution refuses — HLS, an undecodable body, an
    /// unreachable server — comes back as [`App::play_station`]'s one
    /// refusal line and the prompt closes, `Action::None`, nothing queued.
    fn answer_url_prompt(&mut self) -> Action {
        let Some(typed) = self
            .url_prompt
            .as_ref()
            .map(|prompt| prompt.input.trim().to_owned())
        else {
            return Action::None;
        };
        if !typed.starts_with("http://") && !typed.starts_with("https://") {
            if let Some(prompt) = self.url_prompt.as_mut() {
                prompt.error = Some(String::from("the URL must start with http:// or https://"));
            }
            return Action::None;
        }
        let expansion =
            crate::radio::stream::expand(&typed, crate::radio::stream::Timeouts::default());
        self.url_prompt = None;
        self.play_station(typed.clone(), typed, expansion)
    }

    /// The keys while the save prompt is open (issue #45): printable keys
    /// type into it, `backspace` deletes, `enter` answers, `esc` closes it.
    /// The same shape the URL prompt has (issue #42).
    fn on_save_key(&mut self, code: ratatui::crossterm::event::KeyCode) -> Action {
        use ratatui::crossterm::event::KeyCode;

        match code {
            KeyCode::Esc => {
                self.save_prompt = None;
                Action::None
            }
            KeyCode::Backspace => {
                if let Some(prompt) = self.save_prompt.as_mut() {
                    prompt.input.pop();
                    prompt.error = None;
                }
                Action::None
            }
            KeyCode::Char(character) => {
                if let Some(prompt) = self.save_prompt.as_mut() {
                    prompt.input.push(character);
                    prompt.error = None;
                }
                Action::None
            }
            KeyCode::Enter => self.answer_save_prompt(),
            _ => Action::None,
        }
    }

    /// Answers the save prompt (issue #45): the destination is resolved the
    /// way the library prompt resolves its path — `~` against the home, a
    /// path that is neither absolute nor `~`ed against `/` — and refused in
    /// the card when a file is already there: the name is kept and the user
    /// picks another, velo never overwrites (S3). An accepted answer closes
    /// the prompt and starts the copy on its own thread (S2).
    fn answer_save_prompt(&mut self) -> Action {
        let Some(prompt) = self.save_prompt.as_ref() else {
            return Action::None;
        };
        let typed = prompt.input.trim().to_owned();
        let source = prompt.source.clone();
        let name = prompt.name.clone();
        if typed.is_empty() {
            if let Some(prompt) = self.save_prompt.as_mut() {
                prompt.error = Some(String::from("the destination is empty — name the file"));
            }
            return Action::None;
        }
        let home = std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let destination =
            crate::config::expand_path(Path::new(&typed), home.as_deref(), Path::new("/"));
        if destination.exists() {
            if let Some(prompt) = self.save_prompt.as_mut() {
                prompt.error = Some(format!(
                    "{} is already there — velo never overwrites: pick another name",
                    prompt.name
                ));
            }
            return Action::None;
        }
        self.save_prompt = None;
        self.start_save(source, name, destination);
        Action::None
    }

    /// Opens the save prompt (issue #45, S1) for the entry the transport or
    /// the cursor stands on — the same standing rule `ctrl+i` describes
    /// with. A screen with no entry, a local file (already on disk: copying
    /// it is `cp`) and a live station (a stream does not end, so there is
    /// nothing to save) are refused with their own lines, and a save already
    /// running is never joined by a second one (S2).
    fn ask_to_save_remote(&mut self) {
        if let Some(run) = self.pending_save.as_ref() {
            self.status = format!("velo: already saving {} — wait for it to end", run.name);
            return;
        }
        let Some(entry) = self
            .playing_entry()
            .cloned()
            .or_else(|| self.cursor_entry())
        else {
            "velo: nothing to save here".clone_into(&mut self.status);
            return;
        };
        match &entry {
            Entry::Station { .. } => {
                self.status = format!(
                    "velo: {} is a live stream, and a stream does not end — there is nothing \
                     to save",
                    entry.label()
                );
                return;
            }
            Entry::File(path) if !is_pseudo_path(path) && !is_remote_url(path) => {
                self.status = format!(
                    "velo: {} is already a file on disk — copying it is cp",
                    entry.label()
                );
                return;
            }
            Entry::File(_) => {}
        }
        let source = entry
            .as_path()
            .map_or_else(String::new, |path| path.to_string_lossy().into_owned());
        let name = self.save_source_name(&source);
        self.save_prompt = Some(SavePrompt {
            input: format!("~/Downloads/{name}"),
            error: None,
            source,
            name,
        });
    }

    /// The file's own name, as the prefill sees it (issue #45): the title
    /// the index or a wrapper carries when one is known, else the entry's
    /// own last path segment without any query — a channel song's URL names
    /// its file. `track` when nothing names it.
    fn save_source_name(&self, source: &str) -> String {
        let path = Path::new(source);
        if let Some(entry) = self.known_tracks().find(|entry| entry.path == path)
            && !entry.title.is_empty()
        {
            return saved_file_name(&entry.title);
        }
        if let Some(title) = self.song_titles.get(source)
            && !title.is_empty()
        {
            return saved_file_name(title);
        }
        let segment = source.rsplit('/').next().unwrap_or(source);
        let segment = segment.split(['?', '#']).next().unwrap_or(segment);
        if segment.is_empty() {
            return String::from("track");
        }
        saved_file_name(segment)
    }

    /// Starts the saving copy (issue #45, S2): the worker resolves the
    /// entry through the resolver — a cache hit calls no network, a miss
    /// downloads once, the cache's ordinary job — and copies the bytes out
    /// to the destination, off the event loop. The resolver answers with the
    /// cached file itself, so nothing here duplicates the download and the
    /// cache is never moved nor deleted: a copy out, not a move (S4).
    fn start_save(&mut self, source: String, name: String, destination: PathBuf) {
        let Some(resolver) = self.resolver.clone() else {
            self.status = String::from(
                "velo: nothing is configured to fetch remote tracks — no server, no channels",
            );
            return;
        };
        let receiver = spawn_save_worker(resolver, source, destination);
        self.status = format!("⟳ saving {name}…");
        self.pending_save = Some(SaveRun { name, receiver });
    }

    /// Collects what the saving copy's worker has said since the last turn
    /// (issue #45): the newest progress goes to the status line while the
    /// copy runs, and the one last word — where the file landed, or why it
    /// could not — ends it. A worker that ended without a word is reported
    /// rather than left hanging. Never blocks: the copy runs on its own
    /// thread, the pattern the cover fetch and the radio wiring use.
    fn poll_save(&mut self) {
        let Some(run) = self.pending_save.as_ref() else {
            return;
        };
        let mut finished: Option<String> = None;
        let mut progress: Option<String> = None;
        loop {
            match run.receiver.try_recv() {
                Ok(SaveEvent::Progress { done, total }) => {
                    progress = Some(save_progress_line(&run.name, done, total));
                }
                Ok(SaveEvent::Saved { destination }) => {
                    finished = Some(format!(
                        "velo: {} saved to {}",
                        run.name,
                        destination.display()
                    ));
                }
                Ok(SaveEvent::Failed(reason)) => {
                    finished = Some(format!("velo: {} could not be saved: {reason}", run.name));
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if finished.is_none() {
                        finished = Some(format!(
                            "velo: {} could not be saved: the copy ended without a report",
                            run.name
                        ));
                    }
                    break;
                }
            }
        }
        if finished.is_some() {
            self.pending_save = None;
        }
        if let Some(line) = progress {
            self.status = line;
        }
        if let Some(line) = finished {
            self.status = line;
        }
    }

    /// The keys while the help is open (S1): `j`/`k`/`↓`/`↑` scroll it,
    /// `g`/`G` jump to its ends, `/` starts a filter that printable keys type
    /// into until `Enter`; `Esc` clears a filter before it closes the help,
    /// `?` closes it at once. Nothing reaches the list underneath.
    fn on_help_key(&mut self, code: ratatui::crossterm::event::KeyCode) -> Action {
        use ratatui::crossterm::event::KeyCode;

        let area = self.seen_area.get();
        let Some(help) = &mut self.help else {
            return Action::None;
        };
        let last_top = help.last_top(area);
        match code {
            KeyCode::Esc if help.filtering() => {
                help.filter.clear();
                help.typing = false;
                help.top = 0;
            }
            KeyCode::Enter if help.typing => help.typing = false,
            KeyCode::Backspace if help.typing => {
                help.filter.pop();
                help.top = 0;
            }
            KeyCode::Char(character) if help.typing => {
                help.filter.push(character);
                help.top = 0;
            }
            // While typing, `j`, `k` and `?` were typed above.
            KeyCode::Char('j') | KeyCode::Down => help.top = (help.top + 1).min(last_top),
            KeyCode::Char('k') | KeyCode::Up => help.top = help.top.saturating_sub(1),
            // The paging keys (issue #38): `home`/`end` beside `g`/`G`, a
            // page of the card's own rows at a time.
            KeyCode::Home | KeyCode::Char('g') => help.top = 0,
            KeyCode::End | KeyCode::Char('G') => help.top = last_top,
            KeyCode::PageDown => help.top = (help.top + Help::card_rows(area)).min(last_top),
            KeyCode::PageUp => help.top = help.top.saturating_sub(Help::card_rows(area)),
            KeyCode::Char('/') => help.typing = true,
            KeyCode::Char('?') | KeyCode::Esc => self.help = None,
            _ => {}
        }
        Action::None
    }

    /// The keys while the settings card is open (S6): `j`/`k`/`↓`/`↑` move
    /// between the settings, the section headers skipped; `g`/`G` jump to
    /// the ends; `enter` and `→` step the focused setting forward, `←` back;
    /// `esc` closes it. Everything else — `q` and `space` included — is
    /// inert here, and `?` is not a settings key.
    fn on_settings_key(&mut self, code: ratatui::crossterm::event::KeyCode) -> Action {
        use ratatui::crossterm::event::KeyCode;

        let area = self.seen_area.get();
        let Some(card) = self.settings.as_mut() else {
            return Action::None;
        };
        match code {
            KeyCode::Esc => {
                // The card holds no preview of its own (issue #37): a theme
                // is chosen in the picker, which restores on its own `esc`.
                self.settings = None;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                card.move_down();
                card.keep_visible(SettingsCard::card_rows(area));
            }
            KeyCode::Char('k') | KeyCode::Up => {
                card.move_up();
                card.keep_visible(SettingsCard::card_rows(area));
            }
            // The paging keys (issue #38): `home`/`end` beside `g`/`G`, a
            // page of the card's own rows at a time.
            KeyCode::Home | KeyCode::Char('g') => {
                card.cursor = 0;
                card.top = 0;
            }
            KeyCode::End | KeyCode::Char('G') => {
                card.cursor = SettingsCard::setting_count() - 1;
                card.keep_visible(SettingsCard::card_rows(area));
            }
            KeyCode::PageDown => {
                for _ in 0..SettingsCard::card_rows(area) {
                    card.move_down();
                }
                card.keep_visible(SettingsCard::card_rows(area));
            }
            KeyCode::PageUp => {
                for _ in 0..SettingsCard::card_rows(area) {
                    card.move_up();
                }
                card.keep_visible(SettingsCard::card_rows(area));
            }
            KeyCode::Enter => {
                let key = card.focused();
                if key == SettingsKey::Theme {
                    // The theme is chosen in the picker (issue #37), never
                    // stepped here.
                    self.open_theme_picker();
                } else if key == SettingsKey::Spectrum {
                    // The visualizer is chosen in its own picker too (issue
                    // #40): the row's `enter` opens it, the arrows step it.
                    self.open_visualizer_picker();
                } else {
                    self.change_setting(key, true);
                }
            }
            KeyCode::Right => {
                let key = card.focused();
                self.change_setting(key, true);
            }
            KeyCode::Left => {
                let key = card.focused();
                self.change_setting(key, false);
            }
            _ => {}
        }
        Action::None
    }

    /// The keys while the info card is open (issue #43): `esc` closes it and
    /// every other key is inert — the card holds no state of its own to
    /// change. Nothing reaches the lists or the transport while it is up,
    /// `q` and `space` included.
    fn on_info_key(&mut self, code: ratatui::crossterm::event::KeyCode) -> Action {
        if code == ratatui::crossterm::event::KeyCode::Esc {
            self.info = None;
        }
        Action::None
    }

    /// Opens the info card (issue #43): the track it describes is the one
    /// the player stands on, or — when nothing plays — the row the cursor
    /// marks in a list that holds tracks. A screen with no track under the
    /// cursor opens nothing and says so in one status line. Reading the
    /// card's facts is local and cheap: the format header and the tags, a
    /// decode of no block but the one a container hides its format in.
    fn open_info_card(&mut self) {
        let Some(entry) = self
            .playing_entry()
            .cloned()
            .or_else(|| self.cursor_entry())
        else {
            "velo: nothing to describe here".clone_into(&mut self.status);
            return;
        };
        let index = entry.as_path().and_then(|path| {
            self.known_tracks()
                .find(|track| track.path == path)
                .cloned()
        });
        self.info = Some(InfoCard::for_entry(&entry, index.as_ref()));
    }

    /// The row the cursor marks, as the entry a card would describe: the
    /// queue's when the queue is on screen, the row's own where the list
    /// under it holds tracks. A screen whose rows are names — the sources,
    /// the artists, the albums, the genres, the playlist names — holds no
    /// track: `None`.
    fn cursor_entry(&self) -> Option<Entry> {
        if self.radio_on_screen() {
            // The radio's rows are stations, not files: its own keys own
            // that screen and `ctrl+i` there describes what plays.
            return None;
        }
        match self.view {
            View::Queue => self.queue.get(self.queue_selected).cloned(),
            View::Sources | View::Library => match &self.position {
                Position::Files => self.argv.get(self.selected).cloned().map(Entry::from),
                Position::Tracks { .. } | Position::AllTracks | Position::Genre { .. } => self
                    .tracks
                    .get(self.selected)
                    .map(|entry| Entry::from(entry.path.clone())),
                Position::Favourites => self
                    .favourite_rows
                    .get(self.selected)
                    .map(|favorite| Entry::from(PathBuf::from(&favorite.path))),
                Position::Playlist { .. } => self
                    .open_playlist_tracks()
                    .get(self.selected)
                    .cloned()
                    .map(|path| Entry::from(PathBuf::from(path))),
                Position::Recent => self.recent_rows.get(self.selected).map(|row| {
                    if row.is_station() {
                        Entry::Station {
                            url: row.path.clone(),
                            name: row.title.clone().unwrap_or_default(),
                        }
                    } else {
                        Entry::from(PathBuf::from(&row.path))
                    }
                }),
                // The names lists have no track a card could read.
                Position::Artists
                | Position::Albums { .. }
                | Position::AllAlbums
                | Position::Genres
                | Position::Playlists => None,
            },
        }
    }

    /// Steps the focused setting one step forward or back (S7.1): every
    /// change applies at once through the same hooks the keys use, and is
    /// written to the file by those hooks, never from here. A step always
    /// changes the value — a toggle flips it and a cycle moves along the
    /// presets — so each write is a real change.
    fn change_setting(&mut self, key: SettingsKey, forward: bool) {
        match key {
            SettingsKey::ReplayGain => {
                let next = match (self.replay_gain, forward) {
                    (ReplayGainMode::Track, true) | (ReplayGainMode::Off, false) => {
                        ReplayGainMode::Album
                    }
                    (ReplayGainMode::Album, true) | (ReplayGainMode::Track, false) => {
                        ReplayGainMode::Off
                    }
                    (ReplayGainMode::Off, true) | (ReplayGainMode::Album, false) => {
                        ReplayGainMode::Track
                    }
                };
                self.set_replay_gain(next);
                self.remember(
                    None,
                    "replay_gain",
                    &Self::toml_string(replay_gain_label(next)),
                );
            }
            // The theme row is never stepped through `change_setting`
            // (issue #37): its `enter` opens the picker. The version row is
            // a fact, not a choice (issue #69): the arrows and `enter`
            // change nothing on it either.
            SettingsKey::Theme | SettingsKey::Version => {}
            // The transparent-background switch (issue #61): a press flips
            // it, and the palette is re-resolved through the one function
            // the launch and the picker's preview resolve through
            // (`theme_colors_of`, ending in `resolved_over`), so the change
            // is on screen at once — the card is over the interface. A
            // press that changed nothing would write nothing; a two-state
            // switch always changes.
            SettingsKey::TransparentBackground => {
                let next = !self.background_transparent();
                self.theme_overrides.transparent_background = Some(next);
                let name = self.theme_name.clone();
                self.set_theme(self.theme_colors_of(&name));
                // A return to off writes `false` explicitly, the way the
                // `[ui]` switches write both of their values; the surgical
                // `set_value` keeps the key held once, in place.
                self.remember(
                    Some("theme"),
                    "transparent_background",
                    if next { "true" } else { "false" },
                );
            }
            // The visualizer row (issue #40): the arrows step it through the
            // nine shapes, each step written like any other `[ui]` switch.
            SettingsKey::Spectrum => {
                let next = if forward {
                    self.ui.visualizer.next()
                } else {
                    self.ui.visualizer.previous()
                };
                self.set_visualizer(next);
            }
            SettingsKey::Repeat => {
                let next = match (self.repeat, forward) {
                    (Repeat::Off, true) | (Repeat::One, false) => Repeat::All,
                    (Repeat::All, true) | (Repeat::Off, false) => Repeat::One,
                    (Repeat::One, true) | (Repeat::All, false) => Repeat::Off,
                };
                self.repeat = next;
                self.apply(Action::SetRepeat(next));
            }
            SettingsKey::Shuffle => self.apply(Action::ToggleShuffle),
            SettingsKey::CoverArt => self.toggle_ui("cover_art", |ui| &mut ui.cover_art),
            SettingsKey::Hints => self.toggle_ui("hints", |ui| &mut ui.hints),
            SettingsKey::TopBar => self.toggle_ui("top_bar", |ui| &mut ui.top_bar),
            SettingsKey::Advisories => self.toggle_ui("advisories", |ui| &mut ui.advisories),
            SettingsKey::ViewNumbers => {
                self.toggle_ui("view_numbers", |ui| &mut ui.view_numbers);
            }
            SettingsKey::Folder => {
                // The prompt owns the keys from here (S7.2): its typing, its
                // completion and its saving stay exactly as they are.
                self.settings = None;
                self.ask_for_library(self.config_file.clone());
            }
        }
    }

    /// Opens the theme picker on the theme row's `enter` (issue #37): the
    /// cursor starts on the theme applied right now, and the palette in
    /// force — `[theme]` overrides included — is remembered for `esc`. The
    /// card gives every key to the picker.
    fn open_theme_picker(&mut self) {
        if self.theme_files.is_empty() {
            return;
        }
        let cursor = self
            .theme_files
            .iter()
            .position(|theme| theme.name == self.theme_name)
            .unwrap_or(0);
        self.theme_picker = Some(ThemePicker {
            cursor,
            snapshot: self.colors,
        });
        self.settings = None;
    }

    /// The keys while the theme picker is open (issue #37): `j`/`k` and the
    /// arrows move and wrap at the ends, and every move APPLIES the focused
    /// theme — the preview is the interface. `enter` keeps it and writes
    /// `[theme] name` only when it changed; `esc` closes and restores the
    /// palette the picker opened with. Every other key waits.
    fn on_theme_picker_key(&mut self, code: ratatui::crossterm::event::KeyCode) -> Action {
        use ratatui::crossterm::event::KeyCode;

        let count = self.theme_files.len();
        match code {
            KeyCode::Char('j') | KeyCode::Down => {
                if let Some(picker) = self.theme_picker.as_mut()
                    && count > 0
                {
                    picker.cursor = (picker.cursor + 1) % count;
                    self.apply_picker_theme();
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Some(picker) = self.theme_picker.as_mut()
                    && count > 0
                {
                    picker.cursor = (picker.cursor + count - 1) % count;
                    self.apply_picker_theme();
                }
            }
            KeyCode::Enter => {
                if let Some(picker) = self.theme_picker.take()
                    && let Some(theme) = self.theme_files.get(picker.cursor)
                {
                    let name = theme.name.clone();
                    let colors = self.theme_colors_of(&name);
                    self.set_theme(colors);
                    if name != self.theme_name {
                        // The `s`/shuffle write contract: the file gets the
                        // name only when it changed.
                        self.remember(Some("theme"), "name", &Self::toml_string(&name));
                        self.theme_name = name;
                    }
                }
            }
            KeyCode::Esc => {
                if let Some(picker) = self.theme_picker.take() {
                    // Exactly the palette in force when the picker opened
                    // (S4): overrides included, nothing of the preview left.
                    self.set_theme(picker.snapshot);
                }
            }
            _ => {}
        }
        Action::None
    }

    /// Puts the focused theme's palette on screen (issue #37, S4): the move
    /// preview IS the interface, with the colour keys that win over the
    /// theme file on top (S2). A name that is not loaded falls back to the
    /// built-in (S3).
    fn apply_picker_theme(&mut self) {
        let cursor = match self.theme_picker.as_ref() {
            Some(picker) => picker.cursor,
            None => return,
        };
        if let Some(theme) = self.theme_files.get(cursor) {
            let name = theme.name.clone();
            let colors = self.theme_colors_of(&name);
            self.set_theme(colors);
        }
    }

    /// Opens the visualizer picker (issue #40, S3): the cursor starts on
    /// the visualizer in force, and the choice in force is remembered for
    /// `esc`. The card that opens it hands the keys over, the way it does
    /// for the theme picker.
    fn open_visualizer_picker(&mut self) {
        let cursor = Visualizer::ALL
            .iter()
            .position(|visualizer| *visualizer == self.ui.visualizer)
            .unwrap_or(0);
        self.visualizer_picker = Some(VisualizerPicker {
            cursor,
            snapshot: self.ui.visualizer,
        });
        self.settings = None;
    }

    /// The keys while the visualizer picker is open (issue #40, S3):
    /// `j`/`k` and the arrows move and wrap at the ends, and every move
    /// APPLIES the focused visualizer — the preview is the interface.
    /// `enter` keeps it and writes `[ui] visualizer` only when it changed;
    /// `esc` closes and restores the visualizer the picker opened with.
    /// Every other key waits.
    fn on_visualizer_picker_key(&mut self, code: ratatui::crossterm::event::KeyCode) -> Action {
        use ratatui::crossterm::event::KeyCode;

        let count = Visualizer::ALL.len();
        match code {
            KeyCode::Char('j') | KeyCode::Down => {
                let next = self
                    .visualizer_picker
                    .as_ref()
                    .map(|picker| (picker.cursor + 1) % count);
                if let Some(cursor) = next {
                    if let Some(picker) = self.visualizer_picker.as_mut() {
                        picker.cursor = cursor;
                    }
                    self.preview_visualizer(Visualizer::ALL[cursor]);
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                let next = self
                    .visualizer_picker
                    .as_ref()
                    .map(|picker| (picker.cursor + count - 1) % count);
                if let Some(cursor) = next {
                    if let Some(picker) = self.visualizer_picker.as_mut() {
                        picker.cursor = cursor;
                    }
                    self.preview_visualizer(Visualizer::ALL[cursor]);
                }
            }
            KeyCode::Enter => {
                if let Some(picker) = self.visualizer_picker.take() {
                    self.set_visualizer(Visualizer::ALL[picker.cursor]);
                }
            }
            KeyCode::Esc => {
                if let Some(picker) = self.visualizer_picker.take() {
                    // Exactly the visualizer in force when the picker opened
                    // (S3): nothing of the preview left.
                    self.preview_visualizer(picker.snapshot);
                }
            }
            _ => {}
        }
        Action::None
    }

    /// Puts the focused visualizer on screen (issue #40, S3): the move
    /// preview IS the interface, and a preview never writes.
    fn preview_visualizer(&mut self, visualizer: Visualizer) {
        self.ui.visualizer = visualizer;
    }

    /// Adopts a visualizer for good (issue #40, S5): the shapes on screen
    /// change at once and the file gets `[ui] visualizer` — only when the
    /// choice changed, the way every remembered value is written.
    fn set_visualizer(&mut self, visualizer: Visualizer) {
        if visualizer != self.saved_visualizer {
            self.saved_visualizer = visualizer;
            self.ui.visualizer = visualizer;
            self.remember(
                Some("ui"),
                "visualizer",
                &Self::toml_string(visualizer.name()),
            );
        }
    }

    /// Opens the device picker (issue #44, S1): the machine's outputs, one
    /// row each — a machine with one device still lists it — with the
    /// cursor starting on the one in use. A machine with no output says so
    /// instead of opening an empty card.
    fn open_device_picker(&mut self) {
        let list = (self.device_lister)();
        if list.names.is_empty() {
            self.note("velo: no audio output device");
            return;
        }
        let in_use = self.device_in_use(&list);
        let cursor = list
            .names
            .iter()
            .position(|name| *name == in_use)
            .unwrap_or(0);
        self.device_picker = Some(DevicePicker {
            names: list.names,
            in_use,
            cursor,
        });
    }

    /// The device in use: the player's own while one runs — a switch may
    /// have chosen a device the config does not carry — else the session's
    /// choice, else the system default.
    fn device_in_use(&self, list: &DeviceList) -> String {
        if let Some(player) = &self.player {
            return player.device_name().to_owned();
        }
        self.audio_device
            .clone()
            .or_else(|| list.default.clone())
            .unwrap_or_else(|| list.names.first().cloned().unwrap_or_default())
    }

    /// The keys while the device picker is open (issue #44, S1): `j`/`k`
    /// and the arrows move and wrap at the ends; `enter` moves the playback
    /// to the focused device; `esc` closes without changing anything —
    /// there is no preview to restore, a device is heard, not seen. Every
    /// other key waits.
    fn on_device_picker_key(&mut self, code: ratatui::crossterm::event::KeyCode) -> Action {
        use ratatui::crossterm::event::KeyCode;

        let count = self
            .device_picker
            .as_ref()
            .map_or(0, |picker| picker.names.len());
        match code {
            KeyCode::Char('j') | KeyCode::Down => {
                if let Some(picker) = self.device_picker.as_mut()
                    && count > 0
                {
                    picker.cursor = (picker.cursor + 1) % count;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Some(picker) = self.device_picker.as_mut()
                    && count > 0
                {
                    picker.cursor = (picker.cursor + count - 1) % count;
                }
            }
            KeyCode::Enter => {
                if let Some(picker) = self.device_picker.take()
                    && let Some(name) = picker.names.get(picker.cursor)
                {
                    self.switch_audio_device(name);
                }
            }
            KeyCode::Esc => {
                self.device_picker = None;
            }
            _ => {}
        }
        Action::None
    }

    /// Moves the playback to `name` (issue #44, S2–S4), or only remembers
    /// the choice when nothing is playing. A playing stream is rebuilt at
    /// the position it reached — the same rebuild a seek does — so a file
    /// keeps its place in the current track and a live station reopens from
    /// the current moment, never from its start. The new player is opened
    /// *before* the old one is stopped: a device that cannot be opened
    /// leaves the current output playing and says why in one status line
    /// (S4). Switching to the device already in use is a close, not a
    /// rebuild — a gap for nothing is not a switch.
    fn switch_audio_device(&mut self, name: &str) {
        let in_use = self.device_in_use(&(self.device_lister)());
        if name == in_use {
            self.status = format!("output: {name}");
            return;
        }
        let at = self
            .player
            .as_ref()
            .map(|player| (player.current_index(), player.position()));
        let Some((track, offset)) = at else {
            self.adopt_device(name);
            return;
        };
        if self.queue.is_empty() || track >= self.queue.len() {
            self.adopt_device(name);
            return;
        }
        let queue = Arc::new(Mutex::new(self.queue.clone()));
        // A handover still falling dies with the switch: the new player is
        // the only stream this start keeps (issue #65).
        self.drop_crossfade();
        // The probe: the new player is opened while the old one still
        // plays, so a failure here costs a status line, never the music (S4).
        match Player::play_gapless_at_full(
            Arc::clone(&queue),
            track,
            offset,
            self.replay_gain,
            self.repeat,
            self.mono,
            self.bit_perfect,
            Some(name),
            self.resolver.clone(),
        ) {
            Ok(player) => {
                if let Some(old) = self.player.take() {
                    old.stop();
                }
                // A rebuilt player starts at its own default level; without
                // this a switch would silently jump the volume back to it.
                player.set_volume(self.volume);
                self.shared = Some(queue);
                self.player = Some(player);
                self.state = PlayState::Playing;
                self.adopt_device(name);
            }
            Err(error) => {
                // The old player was never touched: the music keeps playing
                // on the device it was on (S4).
                self.status = format!("velo: {error}");
            }
        }
    }

    /// Adopts the chosen device for the rest of the session and remembers
    /// it in the config file, written only when it changed (issue #44, S3)
    /// — the way `z` writes shuffle and the visualizer writes its own key.
    fn adopt_device(&mut self, name: &str) {
        self.audio_device = Some(name.to_owned());
        if self.saved_audio_device.as_deref() != Some(name) {
            self.saved_audio_device = Some(name.to_owned());
            self.remember(Some("audio"), "device", &Self::toml_string(name));
        }
        self.status = format!("output: {name}");
    }

    /// Takes the visualizer to the whole body (issue #40, S4), remembering
    /// the screen it came from: `V` and `esc` land back there exactly.
    fn open_visualizer_fullscreen(&mut self) {
        if self.visualizer_fullscreen.is_none() {
            self.visualizer_fullscreen = Some(self.view);
        }
    }

    /// Leaves the full screen (issue #40, S4): the view it came from comes
    /// back, whatever the keys pressed while it was up did underneath.
    fn close_visualizer_fullscreen(&mut self) {
        if let Some(from) = self.visualizer_fullscreen.take() {
            self.view = from;
        }
    }

    /// The palette the theme named `name` gives, with the colour keys that
    /// win over it on top (S2): what the picker previews and applies. The
    /// invisible alias `default` keeps the old layering, defaults and all
    /// (issue #54, S2/S5) — it is how existing installations select the
    /// factory palette. A name that is not loaded falls back to the
    /// built-in (S3). `transparent_background` is applied last, inside the
    /// same `resolved_over` the launch resolves through, so a preview never
    /// lies about the switch (issue #57, S17).
    fn theme_colors_of(&self, name: &str) -> ThemeColors {
        match self
            .theme_files
            .iter()
            .find(|theme| theme.name == name)
            .map(|theme| theme.colors)
        {
            // The launch's rule, so the preview cannot lie (issue #41): under
            // a named theme, a `[theme]` key that merely restates the built-in
            // default — or the factory value before the palette had its own
            // colours (issue #62) — is documentation, not a choice, and the
            // generated config carries all seven of them. Without this, every
            // preview is masked by the config's own defaults and nothing
            // seems to change.
            // Every name, the invisible alias `default` included, drops the
            // values velo itself wrote (issue #62, L10): the preview shows
            // what the launch will show.
            Some(colors) => self
                .theme_overrides
                .non_default_choices()
                .resolved_over(colors),
            None => self.theme_overrides.resolved(),
        }
    }

    /// Flips one `[ui]` switch (S7): the parts the layout read change at
    /// once through [`App::set_ui`], and the file gets the switch's new
    /// value. A flip only ever happens from the card, so it is always a
    /// change worth writing.
    fn toggle_ui(&mut self, key: &str, part: fn(&mut UiParts) -> &mut bool) {
        let mut ui = self.ui;
        *part(&mut ui) = !*part(&mut ui);
        self.set_ui(ui);
        let shown = *part(&mut self.ui);
        self.remember(Some("ui"), key, if shown { "true" } else { "false" });
    }

    /// The value each setting shows, in the order of the settings (S6.2).
    fn settings_values(&self) -> Vec<String> {
        SETTINGS_ROWS
            .iter()
            .filter_map(|row| match row {
                SettingsRow::Header(_) => None,
                SettingsRow::Setting(key) => Some(self.setting_value(*key)),
            })
            .collect()
    }

    /// What one setting shows on its row's right (S6.2).
    fn setting_value(&self, key: SettingsKey) -> String {
        match key {
            SettingsKey::ReplayGain => replay_gain_label(self.replay_gain).to_owned(),
            SettingsKey::Repeat => self.repeat.label().to_owned(),
            SettingsKey::Shuffle => if self.shuffle { "on" } else { "off" }.to_owned(),
            // The spectrum row shows the visualizer in force (issue #40).
            SettingsKey::Theme => self.theme_name.clone(),
            SettingsKey::TransparentBackground => if self.background_transparent() {
                "on"
            } else {
                "off"
            }
            .to_owned(),
            SettingsKey::Spectrum => self.ui.visualizer.name().to_owned(),
            SettingsKey::CoverArt
            | SettingsKey::Hints
            | SettingsKey::TopBar
            | SettingsKey::Advisories
            | SettingsKey::ViewNumbers => {
                let drawn = match key {
                    SettingsKey::CoverArt => self.ui.cover_art,
                    SettingsKey::Hints => self.ui.hints,
                    SettingsKey::Advisories => self.ui.advisories,
                    SettingsKey::ViewNumbers => self.ui.view_numbers,
                    _ => self.ui.top_bar,
                };
                if drawn { "shown" } else { "hidden" }.to_owned()
            }
            // The version row shows the version in force, and a newer one
            // when a check found it (issue #69).
            SettingsKey::Version => self.version_setting_value(),
            SettingsKey::Folder => self
                .refresh
                .root
                .as_ref()
                .map_or_else(|| "not set".to_owned(), |root| root.display().to_string()),
        }
    }

    /// Accepts the typed library path when it names a directory: saves it to
    /// the config file as typed, closes the prompt and hands the expanded
    /// root to the shell. Otherwise the prompt stays open and says why.
    fn accept_library(&mut self) {
        let Some(setup) = self.library_setup.as_mut() else {
            return;
        };
        let home = setup.home.clone();
        let Some(prompt) = setup.prompt.as_mut() else {
            return;
        };
        let typed = prompt.input.trim().to_owned();
        if typed.is_empty() {
            prompt.error = Some("type the folder that holds your music".to_owned());
            return;
        }
        // The input always starts with `~/`, so the working directory never
        // decides where the library is.
        let root = crate::config::expand_path(Path::new(&typed), home.as_deref(), Path::new("/"));
        if !root.is_dir() {
            prompt.error = Some(format!("{} is not a directory", root.display()));
            return;
        }
        let config_file = self
            .library_setup
            .take()
            .and_then(|setup| setup.config_file);
        if let Some(path) = &config_file {
            let saved = crate::config::library_to_save(&typed, &root);
            match crate::config::set_library(path, &saved) {
                Ok(()) => self.status = format!("library saved to {}", path.display()),
                Err(error) => {
                    self.status = format!("velo: cannot save {}: {error}", path.display());
                }
            }
        }
        self.refresh.root = Some(root.clone());
        self.library_root = Some(root);
    }

    /// Renders the interface. Reads state, changes nothing.
    pub fn draw(&self, frame: &mut Frame) {
        let area = frame.area();
        self.seen_area.set(area);
        // Set again by `draw_visualizer` only when this frame has room for it.
        self.spectrum_bands.set(0);
        frame.render_widget(
            Block::new().style(Style::new().bg(self.theme.background)),
            area,
        );

        // The window: a rounded frame around the title bar, the content and
        // the footer. The top bar hidden gives its row to the content (S5);
        // the hints hidden leave the footer the status row alone. The art
        // threshold below is still measured on the whole terminal, so
        // `artwork::hidden_reason` keeps telling the truth.
        let window = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(self.theme.border))
            .style(Style::new().bg(self.theme.background));
        let inner = window.inner(area);
        frame.render_widget(window, area);

        let footer_rows = if self.ui.hints { 2 } else { 1 };
        let [content, footer] = if self.ui.top_bar {
            let [title_bar, content, footer] = Layout::vertical([
                Constraint::Length(1),
                Constraint::Min(0),
                Constraint::Length(footer_rows),
            ])
            .areas(inner);
            self.draw_title_bar(frame, title_bar);
            [content, footer]
        } else {
            Layout::vertical([Constraint::Min(0), Constraint::Length(footer_rows)]).areas(inner)
        };
        let content = pad(content);
        let footer = pad(footer);
        // Cover art (T8): when a protocol was detected, the art is set to draw
        // and the terminal meets the threshold, it takes the rightmost quarter
        // of the content and the rest makes way. Otherwise the window is
        // exactly what it is without art — nothing to fake, nothing to move.
        // The full-screen visualizer (issue #40, S4) takes the whole body:
        // no now-playing block, but the header and the footer stay.
        let now_rows = if self.visualizer_fullscreen.is_some() {
            0
        } else {
            self.now_playing_rows(content.height)
        };
        let [now, header, body] = Layout::vertical([
            Constraint::Length(now_rows),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .areas(content);
        // The cover sits at the left of the now-playing block, the way an
        // album sleeve sits beside its title, and the block's text moves over.
        // The window-size threshold is the one `hidden_reason` explains. The
        // switch decides first (S5): a cover hidden on purpose takes no slot
        // even when the terminal could draw one.
        let slot = self
            .picker
            .filter(|_| self.ui.cover_art)
            .filter(|_| self.artwork.is_some())
            .and_then(|_| artwork::pane(area))
            .and_then(|_| cover_slot(now));
        let (pane, now) = slot.map_or((None, now), |(art, rest)| (Some(art), rest));
        self.draw_now_playing(frame, now);
        self.draw_header(frame, header);
        // The full-screen visualizer (issue #40, S4) owns the body; the
        // radio and the lists keep out of its way while it is up, and come
        // back exactly where they were when it leaves.
        if self.visualizer_fullscreen.is_some() {
            self.draw_visualizer(frame, body);
        } else {
            self.draw_body(frame, body);
            // The radio view (T6a) replaces the list, not the block above it. It
            // never paints over the queue (issue #23): when the round trip put
            // the queue on screen, the queue is what the body draws. And it
            // draws from the row the content starts on, the header row
            // included (issue #50): the screen header stands down while the
            // radio is on screen, so its row is the radio's to draw its own
            // header line on — no empty row is left above it.
            if self.view != View::Queue
                && let Some(view) = &self.radio
            {
                view.render(frame, union(header, body), &self.theme);
            }
        }
        self.draw_footer(frame, footer);
        self.draw_artwork(frame, pane);
        if let Some(help) = &self.help {
            Self::draw_help(frame, area, help, &self.theme);
        }
        if let Some(card) = &self.settings {
            self.draw_settings_card(frame, area, card);
        }
        if let Some(info) = &self.info {
            self.draw_info_card(frame, area, info);
        }
        if let Some(picker) = &self.theme_picker {
            self.draw_theme_picker(frame, area, picker);
        }
        if let Some(picker) = &self.visualizer_picker {
            Self::draw_visualizer_picker(frame, area, picker, &self.theme);
        }
        if let Some(picker) = &self.device_picker {
            self.draw_device_picker(frame, area, picker);
        }
        if let Some(prompt) = self.library_prompt() {
            Self::draw_library_prompt(frame, area, prompt, &self.theme);
        }
        if let Some(prompt) = &self.seek_prompt {
            Self::draw_seek_prompt(frame, area, prompt, &self.theme);
        }
        if let Some(prompt) = &self.playlist_prompt {
            Self::draw_playlist_prompt(frame, area, prompt, &self.theme);
        }
        if let Some(prompt) = &self.url_prompt {
            Self::draw_url_prompt(frame, area, prompt, &self.theme);
        }
        if let Some(prompt) = &self.save_prompt {
            Self::draw_save_prompt(frame, area, prompt, &self.theme);
        }
        if let Some(picker) = &self.playlist_picker {
            Self::draw_playlist_picker(frame, area, picker, &self.theme);
        }
    }

    /// Renders the cover at the left of the now-playing block, fitting the source image into the
    /// pane. The protocol re-encodes only when its area changes, so a redraw
    /// costs it nothing.
    fn draw_artwork(&self, frame: &mut Frame, pane: Option<Rect>) {
        let (Some(pane), Some(mut picker), Some(artwork)) =
            (pane, self.picker, self.artwork.as_ref())
        else {
            return;
        };
        let mut protocol = self.protocol.borrow_mut();
        if protocol.is_none() {
            *protocol = Some(picker.new_resize_protocol(artwork.image().clone()));
        }
        let Some(state) = protocol.as_mut() else {
            return;
        };
        frame.render_stateful_widget(StatefulImage::new(None), pane, state);
    }

    /// The title bar: the two views as tabs on the left, the one `tab`
    /// switches between, and the letter-spaced wordmark on the right.
    fn draw_title_bar(&self, frame: &mut Frame, area: Rect) {
        frame.render_widget(
            Block::new().style(Style::new().bg(self.theme.background)),
            area,
        );
        // `tab` is one round trip between the sources screen and the queue
        // (issue #26): the strip names both. `sources` is lit anywhere
        // inside it — on the sources screen, inside the local library and
        // under the radio view, which is drawn over both — and the queue is
        // lit only when it is the screen.
        let inside_sources = matches!(self.view, View::Sources | View::Library)
            || (self.radio.is_some() && self.view != View::Queue);
        let tab = |label: &'static str, lit: bool| {
            if lit {
                Span::styled(label, Style::new().fg(self.theme.accent).bold())
            } else {
                Span::styled(label, Style::new().fg(self.theme.dim))
            }
        };
        let mut tabs = Line::from(vec![
            Span::raw(" "),
            tab("sources", inside_sources),
            Span::raw("  "),
            tab("queue", self.view == View::Queue),
        ]);
        // A background index is announced where it is always in view, and
        // goes away when it ends (library-refresh T3).
        if self.refresh.running.is_some() {
            tabs.spans.push(Span::raw("   "));
            tabs.spans.push(Span::styled(
                "⟳ indexing…",
                Style::new().fg(self.theme.accent),
            ));
        }
        frame.render_widget(Paragraph::new(tabs), area);
        frame.render_widget(
            Paragraph::new(Span::styled(
                WORDMARK,
                Style::new().fg(self.theme.accent).bold(),
            ))
            .alignment(Alignment::Right),
            area,
        );
    }

    /// The track that is playing, as large as the window allows: the full
    /// block on a tall window, three rows on a short one, one row on a tiny
    /// one — the list below keeps its rows before the block keeps its air.
    fn draw_now_playing(&self, frame: &mut Frame, area: Rect) {
        // A station has no path to describe: its ICY title is the title when
        // the stream announces one, its own name otherwise, and its progress
        // is the `live` footer (S3, T6c).
        let current = self.playing_entry();
        let (title, subtitle) = match current {
            Some(Entry::File(path)) => describe(path, self.known_tracks(), &self.song_titles),
            Some(Entry::Station { name, .. }) => (self.station_title(name), String::new()),
            None => (
                "nothing playing".to_owned(),
                "enter plays the selection".to_owned(),
            ),
        };
        let title = Line::from(vec![
            Span::styled("♫ ", Style::new().fg(self.theme.accent)),
            Span::styled(title, Style::new().fg(self.theme.text).bold()),
        ]);
        let volume = self.player.as_ref().map_or(self.volume, Player::volume);

        match area.height {
            0 => {}
            1 => {
                let pill = self.state_pill();
                let right = vec![pill, Span::raw("  ")]
                    .into_iter()
                    .chain(volume_slider(volume, 0, &self.theme))
                    .collect::<Vec<_>>();
                split_row(frame, area, title, Line::from(right));
            }
            2..=6 => {
                let [top, middle, bottom] = rows3(area);
                let mut title = title;
                title.spans.push(Span::styled(
                    format!("  {subtitle}"),
                    Style::new().fg(self.theme.dim),
                ));
                split_row(frame, top, title, Line::from(self.state_pill()));
                self.draw_clock_and_progress(frame, middle);
                self.draw_transport(frame, bottom, volume, true);
            }
            _ => {
                // The spectrum sits between the clock and the progress bar
                // when the block was given room for it.
                let bars = area.height.saturating_sub(7);
                let [
                    title_row,
                    subtitle_row,
                    _,
                    clock_row,
                    spectrum_area,
                    progress_row,
                    transport_row,
                    _,
                ] = Layout::vertical([
                    Constraint::Length(1),
                    Constraint::Length(1),
                    Constraint::Length(1),
                    Constraint::Length(1),
                    Constraint::Length(bars),
                    Constraint::Length(1),
                    Constraint::Length(1),
                    Constraint::Length(1),
                ])
                .areas(area);
                self.draw_visualizer(frame, spectrum_area);
                frame.render_widget(Paragraph::new(title), title_row);
                frame.render_widget(
                    Paragraph::new(Line::styled(subtitle, Style::new().fg(self.theme.dim))),
                    subtitle_row,
                );
                let mut clock = self.clock_spans();
                clock.push(Span::raw("  "));
                clock.push(self.state_pill());
                let position = self.player.as_ref().map(|player| {
                    format!(
                        "track {} of {}",
                        player.current_index() + 1,
                        self.queue.len()
                    )
                });
                split_row(
                    frame,
                    clock_row,
                    Line::from(clock),
                    Line::styled(
                        position.unwrap_or_default(),
                        Style::new().fg(self.theme.dim),
                    ),
                );
                frame.render_widget(
                    Paragraph::new(Line::from(progress_bar(
                        self.progress(),
                        progress_row.width,
                        &self.theme,
                    ))),
                    progress_row,
                );
                self.draw_transport(frame, transport_row, volume, false);
            }
        }
    }

    /// `01:23 / 04:56` and the bar after it, on one row of the compact block.
    fn draw_clock_and_progress(&self, frame: &mut Frame, area: Rect) {
        let mut spans = self.clock_spans();
        let used = Line::from(spans.clone()).width();
        let room = usize::from(area.width).saturating_sub(used + 2);
        spans.push(Span::raw("  "));
        spans.extend(progress_bar(
            self.progress(),
            u16::try_from(room).unwrap_or(0),
            &self.theme,
        ));
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    /// The buttons on the left, the volume on the right, and the ladder that
    /// decides what else fits. The play button and the volume's number are the
    /// row; as the width runs out the gauge goes first, then the ReplayGain
    /// mode, then shuffle and repeat, then the two skip arrows — RG outranks
    /// nothing here, which is what it used to do while shuffle left. `dense` is
    /// the short block, where RG, shuffle and repeat are not offered at all.
    fn draw_transport(&self, frame: &mut Frame, area: Rect, volume: u8, dense: bool) {
        let button = Style::new().fg(self.theme.secondary);
        let lit = |on: bool| {
            if on {
                self.theme.accent
            } else {
                self.theme.dim
            }
        };
        let width = usize::from(area.width);
        let measure = |spans: &[Span<'static>]| Line::from(spans.to_vec()).width();

        let primary = vec![Span::styled(
            if self.state == PlayState::Playing {
                " ❚❚ pause "
            } else {
                " ▶ play "
            },
            Style::new().bg(self.theme.accent).fg(self.theme.ink).bold(),
        )];
        let previous = vec![Span::styled(" ◂◂ ", button), Span::raw(" ")];
        let next = vec![Span::raw(" "), Span::styled(" ▸▸ ", button)];
        // Shuffle and repeat: dim while off, the accent while on.
        let extras = vec![
            Span::raw("   "),
            Span::styled("shuffle", Style::new().fg(lit(self.shuffle))),
            Span::styled("  repeat ", Style::new().fg(self.theme.dim)),
            Span::styled(
                self.repeat.label(),
                Style::new().fg(lit(self.repeat != Repeat::Off)),
            ),
        ];
        // The ReplayGain mode in the config's own words, padded to the longest
        // so the slider never moves with it (ui-redesign T10).
        let mode = match self.replay_gain {
            ReplayGainMode::Track => "track",
            ReplayGainMode::Album => "album",
            ReplayGainMode::Off => "off",
        };
        let gain = vec![
            Span::styled("RG ", Style::new().fg(self.theme.dim)),
            Span::styled(format!("{mode:<5}"), Style::new().fg(self.theme.secondary)),
            Span::raw("   "),
        ];

        let mut gauge = VOLUME_CELLS;
        let mut arrows = true;
        let mut with_extras = !dense;
        let mut with_gain = !dense;
        // Give pieces up one at a time, in the ladder's order, until the row
        // fits. The play button and `VOL` plus its number always fit: a window
        // narrower than that clips, it does not lose the transport.
        for _ in 0..u16::MAX {
            let left = measure(&primary)
                + if arrows {
                    measure(&previous) + measure(&next)
                } else {
                    0
                }
                + if with_extras { measure(&extras) } else { 0 };
            let right = measure(&volume_slider(volume, gauge, &self.theme))
                + if with_gain { measure(&gain) } else { 0 };
            if left + right + 1 <= width {
                break;
            }
            if gauge > 0 {
                gauge -= 1;
            } else if with_gain {
                with_gain = false;
            } else if with_extras {
                with_extras = false;
            } else if arrows {
                arrows = false;
            } else {
                break;
            }
        }

        let mut left = Vec::new();
        if arrows {
            left.extend(previous);
        }
        left.extend(primary);
        if arrows {
            left.extend(next);
        }
        if with_extras {
            left.extend(extras);
        }
        let mut right = Vec::new();
        if with_gain {
            right.extend(gain);
        }
        right.extend(volume_slider(volume, gauge, &self.theme));
        split_row(frame, area, Line::from(left), Line::from(right));
    }

    /// The position, and the length when it is known; a dim placeholder
    /// when nothing plays. A live station has neither — and the transport
    /// pill on the same row already says `● live` (issue #53) — so this slot
    /// stays empty instead of saying it twice.
    fn clock_spans(&self) -> Vec<Span<'static>> {
        if matches!(self.playing_entry(), Some(Entry::Station { .. })) {
            return Vec::new();
        }
        let (position, total) = self.transport_time();
        let Some(position) = position else {
            return vec![Span::styled("--:--", Style::new().fg(self.theme.dim))];
        };
        let mut clock = mmss(position);
        if let Some(total) = total {
            clock.push_str(" / ");
            clock.push_str(&mmss(total));
        }
        vec![Span::styled(clock, Style::new().fg(self.theme.accent))]
    }

    /// The position and total the clock shows, as the player reports them.
    /// The tests' fixture answers where no player exists to answer; it is
    /// never set in production, so the player stays the only source.
    fn transport_time(&self) -> (Option<Duration>, Option<Duration>) {
        if let Some(player) = &self.player {
            return (Some(player.position()), self.current_track_duration());
        }
        self.transport_fixture
            .as_ref()
            .map_or((None, None), |fixture| {
                (Some(fixture.position), fixture.duration)
            })
    }

    /// How far into the track playback is, `0.0..=1.0`; `None` when nothing
    /// plays or the length is unknown, so the bar never claims a fraction.
    fn progress(&self) -> Option<f64> {
        let position = self.player.as_ref()?.position();
        let total = self.current_track_duration()?;
        if total.is_zero() {
            return None;
        }
        Some((position.as_secs_f64() / total.as_secs_f64()).clamp(0.0, 1.0))
    }

    /// The visualizer (issue #40): the chosen shape over the analyzer's
    /// bands, as many as the row holds at two columns each, with the band
    /// count remembered so the next tick analyzes exactly that many. `none`
    /// draws nothing — the low-power rule keeps its rows out either way
    /// (S6).
    fn draw_visualizer(&self, frame: &mut Frame, area: Rect) {
        if area.height == 0 {
            return;
        }
        let visualizer = self.ui.visualizer;
        if visualizer == Visualizer::None {
            return;
        }
        let bands = usize::from(area.width).div_ceil(2);
        self.spectrum_bands.set(bands);
        let mut levels = self.spectrum.levels().to_vec();
        levels.resize(bands, 0.0);
        frame.render_widget(
            Paragraph::new(visualizer.draw(
                &levels,
                &self.waveform,
                self.started.elapsed(),
                area,
                &self.theme,
            )),
            area,
        );
    }

    /// The play state as a pill: `▶ playing` in the accent, `● live` for a
    /// station — a live stream is not "playing" in a track's sense, and it
    /// cannot pause (issue #53) — the others dim.
    fn state_pill(&self) -> Span<'static> {
        let live = self.state == PlayState::Playing
            && matches!(self.standing_entry(), Some(Entry::Station { .. }));
        let (glyph, color) = match self.state {
            PlayState::Playing => ("▶", self.theme.accent),
            PlayState::Paused => ("❚❚", self.theme.secondary),
            PlayState::Stopped => ("■", self.theme.dim),
        };
        let (glyph, label) = if live {
            ("●", "live")
        } else {
            (glyph, self.state.label())
        };
        Span::styled(format!(" {glyph} {label} "), Style::new().fg(color))
    }

    /// The line over the list: what it lists and how many of them, as in
    /// `▸─ Queue  [12 tracks]`. Over the library the four views come first,
    /// the active one lit, as in
    /// `▸─ artists  albums  tracks  genres   Tide › Harbor  [12 tracks]`.
    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        // The radio view owns the body while it is on screen (issue #35) and
        // draws its own header line inside it, so this header must not title
        // the screen over it: the default theme's background is the
        // terminal's own, and a background that is the terminal's own covers
        // nothing — the line drawn here would stay visible above the radio.
        if self.radio_on_screen() {
            return;
        }
        if self.search.is_none() && self.view == View::Library && self.in_library() {
            frame.render_widget(Paragraph::new(self.views_header(area.width)), area);
            return;
        }
        let (label, count) = if let Some(search) = &self.search {
            (
                format!("Search /{}", search.query()),
                plural(self.search_results.len(), "match", "matches"),
            )
        } else if self.view == View::Sources {
            (
                "Sources".to_owned(),
                plural(sources::Source::ALL.len(), "source", "sources"),
            )
        } else if self.view == View::Queue {
            (
                "Queue".to_owned(),
                plural(self.queue.len(), "track", "tracks"),
            )
        } else {
            (
                "Files".to_owned(),
                plural(self.argv.len(), "track", "tracks"),
            )
        };
        let line = Line::from(vec![
            Span::styled("▸─ ", Style::new().fg(self.theme.border)),
            Span::styled(label, Style::new().fg(self.theme.text).bold()),
            Span::styled(format!("  [{count}]"), Style::new().fg(self.theme.dim)),
        ]);
        frame.render_widget(Paragraph::new(line), area);
    }

    /// The library's header line: the four views, each numbered when
    /// `[ui] view_numbers` is on (S5.5), the active one in the accent and
    /// the others dim, then where inside the view the list stands, then how
    /// many rows it has. On a row too narrow for all of it the count goes
    /// first, then the place is cut at the edge; the views stay.
    fn views_header(&self, width: u16) -> Line<'static> {
        let mut spans = vec![Span::styled("▸─ ", Style::new().fg(self.theme.border))];
        for (index, by) in BrowseBy::ALL.into_iter().enumerate() {
            if index > 0 {
                spans.push(Span::raw("  "));
            }
            let style = if by == self.browse_by {
                Style::new().fg(self.theme.accent).bold()
            } else {
                Style::new().fg(self.theme.dim)
            };
            // The number is the key that switches to the view (S5.5); it
            // takes part in the width fitting like every other span, so a
            // narrow header still drops the count rather than the views.
            if self.ui.view_numbers {
                spans.push(Span::styled(format!("{} ", index + 1), style));
            }
            spans.push(Span::styled(by.label(), style));
        }
        let place = match &self.position {
            Position::Albums { artist } => artist.clone(),
            Position::Tracks { artist, album } => format!("{artist} › {album}"),
            Position::Genre { genre } => genre.clone(),
            _ => String::new(),
        };
        if !place.is_empty() {
            spans.push(Span::styled(
                format!("   {place}"),
                Style::new().fg(self.theme.text).bold(),
            ));
        }
        let count = Span::styled(
            format!(
                "{}[{}]",
                if place.is_empty() { "   " } else { "  " },
                self.level_count()
            ),
            Style::new().fg(self.theme.dim),
        );
        if Line::from(spans.clone()).width() + count.width() <= usize::from(width) {
            spans.push(count);
        }
        Line::from(spans)
    }

    /// How many rows the library level holds, in words: `12 albums`.
    fn level_count(&self) -> String {
        let (one, many) = match &self.position {
            Position::Files
            | Position::Tracks { .. }
            | Position::AllTracks
            | Position::Genre { .. }
            | Position::Recent
            | Position::Favourites
            | Position::Playlist { .. } => ("track", "tracks"),
            Position::Playlists => ("playlist", "playlists"),
            Position::Artists => ("artist", "artists"),
            Position::Albums { .. } | Position::AllAlbums => ("album", "albums"),
            Position::Genres => ("genre", "genres"),
        };
        plural(self.row_count(), one, many)
    }

    /// The list the keys act on — the search results, the queue or the
    /// library level — or the sentence an empty one says about itself.
    fn draw_body(&self, frame: &mut Frame, area: Rect) {
        // The radio view owns the body while it is on screen (issue #35): it
        // renders over this exact area, and the default theme's background is
        // the terminal's own — a background that is the terminal's own covers
        // nothing — so the list drawn here would keep showing through around
        // the radio's rows.
        if self.radio_on_screen() {
            return;
        }
        let width = area.width;
        let (rows, selected) = if self.search.is_some() {
            if self.search_results.is_empty() {
                draw_empty(frame, area, "no tracks match", self.theme.dim);
                return;
            }
            (self.search_rows(width), self.search_selected)
        } else if self.view == View::Sources {
            (self.source_rows(width), self.sources.selected())
        } else if self.view == View::Queue {
            if self.queue.is_empty() {
                draw_empty(
                    frame,
                    area,
                    "nothing queued — press a on a track to add it",
                    self.theme.dim,
                );
                return;
            }
            (self.queue_rows(width), self.queue_selected)
        } else {
            if let Some(message) = self.empty_message() {
                draw_empty(frame, area, message, self.theme.dim);
                return;
            }
            (self.rows(width), self.selected)
        };
        // Only the background changes on the selected row, so its dim notes
        // stay dim and the playing row keeps its accent.
        let list = List::new(rows)
            .style(Style::new().bg(self.theme.background))
            .highlight_style(Style::new().bg(self.theme.accent).fg(self.theme.ink));
        let mut state = ListState::default().with_selected(Some(selected));
        frame.render_stateful_widget(list, area, &mut state);
    }

    /// The rows of the sources screen (issue #3): one per source, in the
    /// order it shows them.
    /// The source what plays came from, for the marker on the first screen: a
    /// station is radio, a file the index knows is the local library, a file the
    /// store marks is a favourite. Anything else says nothing — a playlist keeps
    /// no origin for its tracks, and a guess would be worse than a bare row.
    fn playing_source(&self) -> Option<sources::Source> {
        match self.playing_entry()? {
            Entry::Station { .. } => Some(sources::Source::Radio),
            Entry::File(path) => {
                let path = path.to_string_lossy();
                if self
                    .tracks
                    .iter()
                    .any(|entry| entry.path.to_string_lossy() == path)
                {
                    Some(sources::Source::Library)
                } else if self.favourites.is_favorite(&path) {
                    Some(sources::Source::Favourites)
                } else {
                    None
                }
            }
        }
    }

    fn source_rows(&self, width: u16) -> Vec<ListItem<'static>> {
        let digits = digits(sources::Source::ALL.len());
        let playing = self.playing_source();
        sources::Source::ALL
            .into_iter()
            .enumerate()
            .map(|(index, source)| {
                ListItem::new(list_row(
                    ListRow {
                        mark: if playing == Some(source) {
                            RowMark::Playing
                        } else {
                            RowMark::None
                        },
                        number: Some(index + 1),
                        digits,
                        name: source.label().to_owned(),
                        note: String::new(),
                        fact: String::new(),
                    },
                    width,
                    &self.theme,
                ))
            })
            .collect()
    }

    /// The rows of the current level, as they are on screen.
    fn rows(&self, width: u16) -> Vec<ListItem<'static>> {
        let digits = digits(self.row_count());
        let standing = self.playing_row();
        (0..self.row_count())
            .map(|index| {
                let (name, note, fact) = self.row_text(index);
                // The recent list's station rows (issue #33) follow the
                // station rule (issue #53): a stopped stream still marks
                // its station, a track it does not.
                let station = matches!(&self.position, Position::Recent)
                    && self
                        .recent_rows
                        .get(index)
                        .is_some_and(|row| row.kind == crate::history::Kind::Station);
                ListItem::new(list_row(
                    ListRow {
                        mark: self.row_mark(standing == Some(index), station),
                        number: Some(index + 1),
                        digits,
                        name,
                        note,
                        fact,
                    },
                    width,
                    &self.theme,
                ))
            })
            .collect()
    }

    /// The row's name with the saved store's `★` before it when the row is
    /// marked (issue #7) — the way the radio view stars its favourites, in
    /// the label itself. The marker is state, read from the store every
    /// frame, never a copy made when the list was built. Only whole-track
    /// lists carry marks; every row of the favourites list is marked.
    fn starred_name(&self, index: usize, name: String) -> String {
        let starred_mark = match &self.position {
            Position::Tracks { .. } | Position::AllTracks | Position::Genre { .. } => self
                .tracks
                .get(index)
                .is_some_and(|entry| self.favourites.is_favorite(&entry.path.to_string_lossy())),
            Position::Favourites => true,
            // A playlist's rows are track rows too (issue #4): a track that
            // is also a favourite keeps its star there.
            Position::Playlist { .. } => self
                .open_playlist_tracks()
                .get(index)
                .is_some_and(|path| self.favourites.is_favorite(path)),
            _ => false,
        };
        if starred_mark {
            rowtext::starred(&name)
        } else {
            name
        }
    }

    /// What row `index` of the current level says: its name, the note
    /// dimmed after it and the fact against the right edge.
    fn row_text(&self, index: usize) -> (String, String, String) {
        let count = |count: u64, one: &str, many: &str| {
            plural(usize::try_from(count).unwrap_or(usize::MAX), one, many)
        };
        match &self.position {
            Position::Files => {
                let (name, folder) = describe(&self.argv[index], [], &self.song_titles);
                (name, folder, String::new())
            }
            Position::Artists => {
                let entry = &self.artists[index];
                let counts = format!(
                    "{} · {}",
                    count(entry.albums, "album", "albums"),
                    count(entry.tracks, "track", "tracks")
                );
                (entry.name.clone(), String::new(), counts)
            }
            Position::Albums { .. } => {
                let entry = &self.albums[index];
                let year = entry.year.map_or_else(String::new, |year| year.to_string());
                (entry.title.clone(), String::new(), year)
            }
            // Every album says whose it is beside its year.
            Position::AllAlbums => {
                let entry = &self.albums[index];
                let fact = entry.year.map_or_else(
                    || entry.artist.clone(),
                    |year| format!("{} · {year}", entry.artist),
                );
                (entry.title.clone(), String::new(), fact)
            }
            Position::Tracks { .. } | Position::AllTracks | Position::Genre { .. } => {
                let entry = &self.tracks[index];
                // Rows from many albums say whose and which album they are.
                let note = if matches!(self.position, Position::Tracks { .. }) {
                    String::new()
                } else {
                    rowtext::note(&[&entry.artist, &entry.album])
                };
                (
                    self.starred_name(index, entry.title.clone()),
                    note,
                    Self::duration(entry.duration_ms),
                )
            }
            Position::Recent => {
                let entry = &self.recent_rows[index];
                // The library's row convention (S4): the title, with
                // `artist · album` dimmed after it; a station shows its own
                // name with the note `radio` (issue #33); an entry with no
                // title keeps the file's own name over its folder.
                match entry.kind {
                    crate::history::Kind::Station => (
                        entry.title.clone().unwrap_or_default(),
                        String::from("radio"),
                        String::new(),
                    ),
                    crate::history::Kind::File => file_entry_text(
                        &entry.path,
                        entry.artist.as_deref(),
                        entry.album.as_deref(),
                        entry.title.as_deref(),
                        &self.song_titles,
                    ),
                }
            }
            Position::Favourites => {
                let favorite = &self.favourite_rows[index];
                // The same row convention as the recent list (issue #7):
                // what the store knew about the track names it, and a path
                // the index never described keeps its file name.
                let (name, note, fact) = file_entry_text(
                    &favorite.path,
                    favorite.artist.as_deref(),
                    favorite.album.as_deref(),
                    favorite.title.as_deref(),
                    &self.song_titles,
                );
                (self.starred_name(index, name), note, fact)
            }
            // The names list (issue #4): the name, and its track count
            // against the right edge.
            Position::Playlists => {
                let entry = &self.playlist_rows[index];
                (
                    entry.name.clone(),
                    String::new(),
                    plural(entry.track.len(), "track", "tracks"),
                )
            }
            // A playlist's track list (issue #4): the same row convention as
            // the recent list, over a path the store holds with no metadata —
            // the file's own name names it.
            Position::Playlist { .. } => {
                let path = self
                    .open_playlist_tracks()
                    .get(index)
                    .map(String::as_str)
                    .unwrap_or_default();
                let (name, note, fact) = file_entry_text(path, None, None, None, &self.song_titles);
                (self.starred_name(index, name), note, fact)
            }
            Position::Genres => {
                let entry = &self.genres[index];
                (
                    entry.name.clone(),
                    String::new(),
                    count(entry.tracks, "track", "tracks"),
                )
            }
        }
    }

    /// The queue, in play order, named the way the now-playing block names
    /// a file and marked the way every other list marks the row the
    /// transport stands on (issue #53).
    fn queue_rows(&self, width: u16) -> Vec<ListItem<'static>> {
        let digits = digits(self.queue.len());
        let standing = self.playing_queue_index();
        self.queue
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                // A station is named for itself, never resolved to a path:
                // its own name is what the row shows.
                let (name, note) = match entry {
                    Entry::File(path) => describe(path, self.known_tracks(), &self.song_titles),
                    Entry::Station { name, .. } => (name.clone(), String::new()),
                };
                let row = ListRow {
                    mark: self.row_mark(
                        standing == Some(index),
                        matches!(entry, Entry::Station { .. }),
                    ),
                    number: Some(index + 1),
                    digits,
                    name,
                    note,
                    fact: String::new(),
                };
                // A station reads as live (issue #17): its row shows `● live`
                // where a track shows its position and total — the same fact
                // the transport shows for a live stream.
                ListItem::new(match entry {
                    Entry::Station { .. } => live_row(row, width, &self.theme),
                    Entry::File(_) => list_row(row, width, &self.theme),
                })
            })
            .collect()
    }

    /// The search results: a title with its artist and album dimmed after
    /// it and its length at the right edge.
    fn search_rows(&self, width: u16) -> Vec<ListItem<'static>> {
        let digits = digits(self.search_results.len());
        let standing = self.playing_path();
        self.search_results
            .iter()
            .filter_map(|&index| self.search_pool.get(index))
            .enumerate()
            .map(|(row, entry)| {
                ListItem::new(list_row(
                    ListRow {
                        mark: self.row_mark(standing == Some(entry.path.as_path()), false),
                        number: Some(row + 1),
                        digits,
                        name: entry.title.clone(),
                        note: rowtext::note(&[&entry.artist, &entry.album]),
                        fact: Self::duration(entry.duration_ms),
                    },
                    width,
                    &self.theme,
                ))
            })
            .collect()
    }

    /// A duration in `m:ss`, empty for tracks with no known length.
    fn duration(duration_ms: i64) -> String {
        if duration_ms <= 0 {
            return String::new();
        }
        let seconds = u64::try_from(duration_ms / 1000).unwrap_or(0);
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }

    fn draw_footer(&self, frame: &mut Frame, area: Rect) {
        let [status_row, hints_row] = Layout::vertical([
            Constraint::Length(1),
            // Without the hints the footer is the status row alone (S5): no
            // empty line is left where they were.
            Constraint::Length(u16::from(self.ui.hints)),
        ])
        .areas(area);

        let status = if self.search.is_some() {
            // The query itself, over the transport line, so the user sees what
            // they are typing while the body lists the results.
            Line::styled(
                format!("/{}", self.search_query()),
                Style::new().fg(self.theme.accent),
            )
        } else if self.status.is_empty() {
            // The clock and the volume live in the now-playing block.
            Line::default()
        } else {
            Line::styled(self.status.clone(), Style::new().fg(self.theme.accent))
        };
        if !status.spans.is_empty() {
            frame.render_widget(Paragraph::new(status), status_row);
        }

        let browsing = !matches!(self.position, Position::Files);
        let hints: &[(&str, &str)] = if self.search.is_some() {
            &[
                ("type", "to search"),
                ("j/k", "move"),
                ("enter", "play"),
                ("backspace", "delete"),
                ("esc", "cancel"),
            ]
        } else if browsing {
            &[
                ("j/k", "move"),
                ("enter", "play"),
                ("space", "pause"),
                ("←/→", "seek"),
                ("/", "search"),
                // Dropped before search on a narrow row: the header already
                // names the views and the help says which key is which.
                ("1-4", "views"),
                ("n", "next"),
                ("tab", "queue"),
                ("?", "help"),
                ("q", "quit"),
            ]
        } else {
            &[
                ("j/k", "move"),
                ("enter", "play"),
                ("space", "pause"),
                ("←/→", "seek"),
                ("/", "search"),
                // Dropped first on a narrow row: the transport buttons and
                // the help already show how to skip and to see the queue.
                ("n", "next"),
                ("tab", "queue"),
                ("?", "help"),
                ("q", "quit"),
            ]
        };
        if self.ui.hints {
            frame.render_widget(
                Paragraph::new(hint_line(hints, hints_row.width, &self.theme)),
                hints_row,
            );
        }
    }

    /// Every library row the interface holds that may name a playing file:
    /// the list on screen, the search pool and the rows the stream started
    /// from.
    fn known_tracks(&self) -> impl Iterator<Item = &TrackEntry> {
        self.tracks
            .iter()
            .chain(&self.search_pool)
            .chain(&self.played)
    }

    /// The path of the file whose samples are being produced, if any: the
    /// marker has to find the playing row wherever a list shows whole tracks.
    fn playing_path(&self) -> Option<&std::path::Path> {
        let player = self.player.as_ref()?;
        self.queue
            .get(player.current_index())
            .and_then(Entry::as_path)
    }

    /// The queue entry the transport and the now-playing block read: the one
    /// the player stands on (T6c). Nothing plays while the state is stopped,
    /// and in production the running state and the player stand and fall
    /// together — the fixture path (no player, state running) exists only
    /// under [`App::show_playing_for_tests`], where the queue's head is the
    /// entry.
    fn playing_entry(&self) -> Option<&Entry> {
        if self.state == PlayState::Stopped {
            return None;
        }
        let index = self.player.as_ref().map_or(0, Player::current_index);
        self.queue.get(index)
    }

    /// The queue entry the transport stands on, whether or not it still
    /// plays (issue #53): a stopped stream keeps its queue, so the stopped
    /// station can still be marked. `None` before the first stream starts —
    /// the queue the user lined up without playing is nobody's current
    /// entry.
    fn standing_entry(&self) -> Option<&Entry> {
        self.queue.get(self.playing_queue_index()?)
    }

    /// The mark a row carries (issue #53), given that it is the row the
    /// transport stands on and whether it names a station: the transport's
    /// state — except that a stopped stream marks only a station, a stopped
    /// track being nothing a list needs to point at, and a live stream never
    /// pauses.
    fn row_mark(&self, standing: bool, station: bool) -> RowMark {
        if !standing {
            return RowMark::None;
        }
        match self.state {
            PlayState::Playing => RowMark::Playing,
            PlayState::Paused if !station => RowMark::Paused,
            PlayState::Stopped if station => RowMark::Stopped,
            _ => RowMark::None,
        }
    }

    /// The title the playing station shows (S4): the ICY title the stream is
    /// announcing when there is one, the station's own name otherwise. The
    /// player publishes the title; the tests' fixture answers where no
    /// player exists to answer (T6c) and is never set in production.
    fn station_title(&self, name: &str) -> String {
        if let Some(player) = &self.player {
            return player.current_title().unwrap_or_else(|| name.to_owned());
        }
        self.transport_fixture
            .as_ref()
            .and_then(|fixture| fixture.title.clone())
            .unwrap_or_else(|| name.to_owned())
    }

    /// The length of the track that is playing: from the interface's own track
    /// row when the queue came from the library, otherwise from the decoder,
    /// which has always known it. Without the fallback a command-line queue
    /// showed a bare clock that a seek could push past the end of the file.
    fn current_track_duration(&self) -> Option<Duration> {
        let player = self.player.as_ref()?;
        // The row is found by the file that plays, so a shuffled queue reads
        // the length of the track that is actually playing.
        let from_row = self.playing_path().and_then(|path| {
            let entry = self.known_tracks().find(|entry| entry.path == path)?;
            let millis = u64::try_from(entry.duration_ms).ok()?;
            (millis > 0).then(|| Duration::from_millis(millis))
        });
        from_row.or_else(|| player.current_duration())
    }

    /// The help card (S1): every key, as tall as the window allows but never
    /// shorter, scrolled to `help.top`, with the filter under the title while
    /// there is one and where it is (`12–20 of 26`) on its bottom edge when it
    /// does not fit.
    fn draw_help(frame: &mut Frame, area: Rect, help: &Help, theme: &Theme) {
        let key_width = HELP_ROWS
            .iter()
            .map(|(key, _)| key.chars().count())
            .max()
            .unwrap_or(0);
        let label_width = HELP_ROWS
            .iter()
            .map(|(_, label)| label.chars().count())
            .max()
            .unwrap_or(0);
        // Both measured before the filter, so that narrowing the contents
        // never makes the card itself jump: the whole table decides its size.
        // Two border columns, one of air per side, three between the columns.
        let width = u16::try_from(key_width + 3 + label_width + 4).unwrap_or(area.width);
        let rows = help.rows();
        let shown = help.shown_rows(area);
        let top = help.top.min(help.last_top(area));
        let body = Help::card_rows(area);
        let height = u16::try_from(body + 2).unwrap_or(area.height);
        let box_area = centered(area, width, height);

        let mut text: Vec<Line> = Vec::new();
        if help.filtering() {
            let mut filter = vec![
                Span::styled("/ ", Style::new().fg(theme.secondary)),
                Span::styled(help.filter.clone(), Style::new().fg(theme.accent)),
            ];
            if help.typing {
                filter.push(Span::styled("▏", Style::new().fg(theme.secondary)));
            }
            text.push(Line::from(filter));
        }
        if rows.is_empty() {
            text.push(Line::styled(
                format!("no key matches “{}”", help.filter),
                Style::new().fg(theme.dim),
            ));
        }
        text.extend(rows.iter().skip(top).take(shown).map(|(key, label)| {
            Line::from(vec![
                Span::styled(
                    format!("{key:<key_width$}   "),
                    Style::new().fg(theme.secondary),
                ),
                Span::styled((*label).to_owned(), Style::new().fg(theme.text)),
            ])
        }));

        let mut card = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(theme.accent))
            .title(Span::styled(" keys ", Style::new().fg(theme.accent).bold()))
            .padding(ratatui::widgets::Padding::horizontal(1))
            .style(Style::new().bg(theme.background));
        if !help.filtering() {
            card = card.title_bottom(Line::styled(" / filter ", Style::new().fg(theme.dim)));
        }
        if shown < rows.len() {
            let position = format!(" {}–{} of {} ", top + 1, top + shown, rows.len());
            card = card
                .title_bottom(Line::styled(position, Style::new().fg(theme.dim)).right_aligned());
        }
        frame.render_widget(Clear, box_area);
        frame.render_widget(Paragraph::new(text).block(card), box_area);
    }

    /// The settings card (S6): a rounded, accent-bordered, centred block
    /// titled ` settings `, every setting with its value on the right, the
    /// section headers dim and the focused row in the accent. Its size comes
    /// from the whole table, so it never changes as the cursor moves, and it
    /// scrolls only as far as the focused row needs.
    fn draw_settings_card(&self, frame: &mut Frame, area: Rect, card: &SettingsCard) {
        let values = self.settings_values();
        let label_width = SETTINGS_ROWS
            .iter()
            .map(|row| match row {
                SettingsRow::Header(name) => name.chars().count(),
                SettingsRow::Setting(key) => key.label().chars().count(),
            })
            .max()
            .unwrap_or(0);
        let value_width = values
            .iter()
            .map(|value| value.chars().count())
            .max()
            .unwrap_or(0);
        // Two border columns, one of air per side, two between the columns.
        let width = u16::try_from(label_width + 2 + value_width + 4).unwrap_or(area.width);
        let shown = SettingsCard::card_rows(area);
        let top = card.top.min(SettingsCard::last_top(shown));
        let height = u16::try_from(shown + 2).unwrap_or(area.height);
        let box_area = centered(area, width, height);

        let mut setting = 0;
        let lines: Vec<Line> = SETTINGS_ROWS
            .iter()
            .map(|row| match row {
                SettingsRow::Header(name) => {
                    Line::styled((*name).to_owned(), Style::new().fg(self.theme.dim))
                }
                SettingsRow::Setting(key) => {
                    let focused = setting == card.cursor;
                    let value = values.get(setting).map(String::as_str).unwrap_or_default();
                    setting += 1;
                    let label = if focused {
                        Style::new().fg(self.theme.accent)
                    } else {
                        Style::new().fg(self.theme.text)
                    };
                    let value_style = if focused {
                        Style::new().fg(self.theme.accent)
                    } else {
                        Style::new().fg(self.theme.secondary)
                    };
                    Line::from(vec![
                        Span::styled(
                            format!("{key_label:<label_width$}  ", key_label = key.label()),
                            label,
                        ),
                        Span::styled(value.to_owned(), value_style),
                    ])
                }
            })
            .collect();

        frame.render_widget(Clear, box_area);
        frame.render_widget(
            Paragraph::new(lines.into_iter().skip(top).take(shown).collect::<Vec<_>>()).block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().fg(self.theme.accent))
                    .title(Span::styled(
                        " settings ",
                        Style::new().fg(self.theme.accent).bold(),
                    ))
                    .padding(ratatui::widgets::Padding::horizontal(1))
                    .style(Style::new().bg(self.theme.background)),
            ),
            box_area,
        );
    }

    /// The info card (issue #43): a rounded, accent-bordered, centred block
    /// titled ` info `, one row per fact the track has: the path or the
    /// remote URI verbatim, the file's header facts, what the index knew and
    /// the `ReplayGain` tags the file carries. A field the sources do not
    /// hold has no row; the card itself reads nothing.
    fn draw_info_card(&self, frame: &mut Frame, area: Rect, card: &InfoCard) {
        let label_width = card
            .rows
            .iter()
            .map(|(field, _)| field.chars().count())
            .max()
            .unwrap_or(0);
        let value_width = card
            .rows
            .iter()
            .map(|(_field, value)| value.chars().count())
            .max()
            .unwrap_or(0);
        // Two border columns, one of air per side, two between the columns.
        let width = u16::try_from(label_width + 2 + value_width + 4).unwrap_or(area.width);
        let height = u16::try_from(card.rows.len() + 2).unwrap_or(area.height);
        let box_area = centered(area, width, height);

        let lines: Vec<Line> = card
            .rows
            .iter()
            .map(|(field, value)| {
                Line::from(vec![
                    Span::styled(
                        format!("{field:<label_width$}  "),
                        Style::new().fg(self.theme.dim),
                    ),
                    Span::styled(value.clone(), Style::new().fg(self.theme.text)),
                ])
            })
            .collect();

        frame.render_widget(Clear, box_area);
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().fg(self.theme.accent))
                    .title(Span::styled(
                        " info ",
                        Style::new().fg(self.theme.accent).bold(),
                    ))
                    .padding(ratatui::widgets::Padding::horizontal(1))
                    .style(Style::new().bg(self.theme.background)),
            ),
            box_area,
        );
    }

    /// The theme picker (issue #37): a small rounded card, centred like the
    /// order card, one row per theme, the focused row in the accent.
    fn draw_theme_picker(&self, frame: &mut Frame, area: Rect, picker: &ThemePicker) {
        let names = self.theme_picker_names();
        let width = u16::try_from(
            names
                .iter()
                .map(|name| name.chars().count())
                .max()
                .unwrap_or(0)
                + 4,
        )
        .unwrap_or(area.width);
        let height = u16::try_from(names.len() + 2).unwrap_or(area.height);
        let box_area = centered(area, width, height);
        let lines = names
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let style = if index == picker.cursor {
                    Style::new().fg(self.theme.accent)
                } else {
                    Style::new().fg(self.theme.text)
                };
                Line::styled(name.clone(), style)
            })
            .collect::<Vec<_>>();
        frame.render_widget(Clear, box_area);
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().fg(self.theme.accent))
                    .title(Span::styled(
                        " theme ",
                        Style::new().fg(self.theme.accent).bold(),
                    ))
                    .padding(ratatui::widgets::Padding::horizontal(1))
                    .style(Style::new().bg(self.theme.background)),
            ),
            box_area,
        );
    }

    /// The visualizer picker (issue #40): the theme picker's card, one row
    /// per shape, the focused row in the accent, titled ` visualizer `.
    fn draw_visualizer_picker(
        frame: &mut Frame,
        area: Rect,
        picker: &VisualizerPicker,
        theme: &Theme,
    ) {
        let names: Vec<&str> = Visualizer::ALL
            .iter()
            .map(|visualizer| visualizer.name())
            .collect();
        let width = u16::try_from(
            names
                .iter()
                .map(|name| name.chars().count())
                .max()
                .unwrap_or(0)
                + 4,
        )
        .unwrap_or(area.width);
        let height = u16::try_from(names.len() + 2).unwrap_or(area.height);
        let box_area = centered(area, width, height);
        let lines = names
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let style = if index == picker.cursor {
                    Style::new().fg(theme.accent)
                } else {
                    Style::new().fg(theme.text)
                };
                Line::styled((*name).to_owned(), style)
            })
            .collect::<Vec<_>>();
        frame.render_widget(Clear, box_area);
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().fg(theme.accent))
                    .title(Span::styled(
                        " visualizer ",
                        Style::new().fg(theme.accent).bold(),
                    ))
                    .padding(ratatui::widgets::Padding::horizontal(1))
                    .style(Style::new().bg(theme.background)),
            ),
            box_area,
        );
    }

    /// The device picker (issue #44): the theme picker's card, one row per
    /// output, the focused row in the accent and the one in use marked with
    /// the live dot, titled ` output `.
    fn draw_device_picker(&self, frame: &mut Frame, area: Rect, picker: &DevicePicker) {
        let width = u16::try_from(
            picker
                .names
                .iter()
                .map(|name| name.chars().count() + 2)
                .max()
                .unwrap_or(0)
                + 4,
        )
        .unwrap_or(area.width);
        let height = u16::try_from(picker.names.len() + 2).unwrap_or(area.height);
        let box_area = centered(area, width, height);
        let lines = picker
            .names
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let style = if index == picker.cursor {
                    Style::new().fg(self.theme.accent)
                } else {
                    Style::new().fg(self.theme.text)
                };
                // The one in use carries the live dot the radio's footer
                // uses; the rest are indented to keep the rows straight.
                let label = if *name == picker.in_use {
                    format!("\u{25cf} {name}")
                } else {
                    format!("  {name}")
                };
                Line::styled(label, style)
            })
            .collect::<Vec<_>>();
        frame.render_widget(Clear, box_area);
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().fg(self.theme.accent))
                    .title(Span::styled(
                        " output ",
                        Style::new().fg(self.theme.accent).bold(),
                    ))
                    .padding(ratatui::widgets::Padding::horizontal(1))
                    .style(Style::new().bg(self.theme.background)),
            ),
            box_area,
        );
    }

    /// The library prompt: a rounded card, centred like the help, with the
    /// path being typed and, under it, why the last one was refused.
    fn draw_library_prompt(frame: &mut Frame, area: Rect, prompt: &LibraryPrompt, theme: &Theme) {
        let width = area.width.saturating_sub(8).clamp(20, 72);
        let box_area = centered(area, width, 9);
        let field_width = usize::from(width.saturating_sub(6));
        // Keep the end of a long path, where the cursor is, in view.
        let shown: String = {
            let chars: Vec<char> = prompt.input.chars().collect();
            let skip = chars.len().saturating_sub(field_width);
            chars[skip..].iter().collect()
        };
        let text = vec![
            Line::styled("Where is your music?", Style::new().fg(theme.text)),
            Line::styled(
                "a folder in your home — tab completes it",
                Style::new().fg(theme.dim),
            ),
            Line::from(vec![
                Span::styled(shown, Style::new().fg(theme.accent)),
                Span::styled("▏", Style::new().fg(theme.secondary)),
            ]),
            // The folders the last tab could not choose between.
            Line::styled(
                prompt.candidates.join("  "),
                Style::new().fg(theme.secondary),
            ),
            prompt.error.as_ref().map_or_else(
                || Line::styled("enter save · esc skip", Style::new().fg(theme.dim)),
                |error| Line::styled(error.clone(), Style::new().fg(theme.secondary)),
            ),
        ];
        frame.render_widget(Clear, box_area);
        frame.render_widget(
            Paragraph::new(text).block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().fg(theme.accent))
                    .title(Span::styled(
                        " library ",
                        Style::new().fg(theme.accent).bold(),
                    ))
                    .padding(ratatui::widgets::Padding::new(1, 1, 1, 0))
                    .style(Style::new().bg(theme.background)),
            ),
            box_area,
        );
    }

    /// The seek prompt (issue #6): a card like the library's, with the position
    /// being typed, the forms it accepts, and why the last one was refused.
    fn draw_seek_prompt(frame: &mut Frame, area: Rect, prompt: &SeekPrompt, theme: &Theme) {
        let width = area.width.saturating_sub(8).clamp(20, 48);
        let box_area = centered(area, width, 7);
        let text = vec![
            Line::styled("Jump to where?", Style::new().fg(theme.text)),
            Line::styled("70% · 1:20 · 1:20:30 · 90", Style::new().fg(theme.dim)),
            Line::from(vec![
                Span::styled(prompt.input.clone(), Style::new().fg(theme.accent)),
                Span::styled("▏", Style::new().fg(theme.secondary)),
            ]),
            prompt.error.as_ref().map_or_else(
                || Line::styled("enter jump · esc cancel", Style::new().fg(theme.dim)),
                |error| Line::styled(error.clone(), Style::new().fg(theme.secondary)),
            ),
        ];
        frame.render_widget(Clear, box_area);
        frame.render_widget(
            Paragraph::new(text).block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().fg(theme.accent))
                    .title(Span::styled(" seek ", Style::new().fg(theme.accent).bold()))
                    .padding(ratatui::widgets::Padding::new(1, 1, 1, 0))
                    .style(Style::new().bg(theme.background)),
            ),
            box_area,
        );
    }

    /// The playlist-name prompt (issue #4), drawn in the seek prompt's
    /// style: a question, the typed name, and `enter save · esc cancel` — or
    /// the reason the last answer was refused under what was typed.
    fn draw_playlist_prompt(frame: &mut Frame, area: Rect, prompt: &PlaylistPrompt, theme: &Theme) {
        let width = area.width.saturating_sub(8).clamp(20, 48);
        let box_area = centered(area, width, 7);
        let question = if prompt.rename.is_some() {
            "Rename the playlist"
        } else {
            "Save the queue as a new playlist"
        };
        let text = vec![
            Line::styled(question, Style::new().fg(theme.text)),
            Line::styled(
                "a name is the playlist's identity",
                Style::new().fg(theme.dim),
            ),
            Line::from(vec![
                Span::styled(prompt.input.clone(), Style::new().fg(theme.accent)),
                Span::styled("▏", Style::new().fg(theme.secondary)),
            ]),
            prompt.error.as_ref().map_or_else(
                || Line::styled("enter save · esc cancel", Style::new().fg(theme.dim)),
                |error| Line::styled(error.clone(), Style::new().fg(theme.secondary)),
            ),
        ];
        frame.render_widget(Clear, box_area);
        frame.render_widget(
            Paragraph::new(text).block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().fg(theme.accent))
                    .title(Span::styled(
                        " playlist ",
                        Style::new().fg(theme.accent).bold(),
                    ))
                    .padding(ratatui::widgets::Padding::new(1, 1, 1, 0))
                    .style(Style::new().bg(theme.background)),
            ),
            box_area,
        );
    }

    /// The URL prompt (issue #42), drawn in the seek prompt's style: a
    /// question, the typed URL, and `enter play · esc cancel` — or the
    /// reason the last answer was refused under what was typed.
    fn draw_url_prompt(frame: &mut Frame, area: Rect, prompt: &UrlPrompt, theme: &Theme) {
        let width = area.width.saturating_sub(8).clamp(20, 48);
        let box_area = centered(area, width, 7);
        let text = vec![
            Line::styled("Play what URL?", Style::new().fg(theme.text)),
            Line::styled(
                "a live stream — or the songs a playlist lists",
                Style::new().fg(theme.dim),
            ),
            Line::from(vec![
                Span::styled(prompt.input.clone(), Style::new().fg(theme.accent)),
                Span::styled("▏", Style::new().fg(theme.secondary)),
            ]),
            prompt.error.as_ref().map_or_else(
                || Line::styled("enter play · esc cancel", Style::new().fg(theme.dim)),
                |error| Line::styled(error.clone(), Style::new().fg(theme.secondary)),
            ),
        ];
        frame.render_widget(Clear, box_area);
        frame.render_widget(
            Paragraph::new(text).block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().fg(theme.accent))
                    .title(Span::styled(" url ", Style::new().fg(theme.accent).bold()))
                    .padding(ratatui::widgets::Padding::new(1, 1, 1, 0))
                    .style(Style::new().bg(theme.background)),
            ),
            box_area,
        );
    }

    /// The save prompt (issue #45), drawn in the seek prompt's style: a
    /// question, the destination being typed, and `enter save · esc cancel`
    /// — or the reason the last answer was refused under what was typed.
    fn draw_save_prompt(frame: &mut Frame, area: Rect, prompt: &SavePrompt, theme: &Theme) {
        let width = area.width.saturating_sub(8).clamp(24, 56);
        let box_area = centered(area, width, 7);
        let text = vec![
            Line::styled("Save the track where?", Style::new().fg(theme.text)),
            Line::styled(
                "a copy out — the library and the cache are untouched",
                Style::new().fg(theme.dim),
            ),
            Line::from(vec![
                Span::styled(prompt.input.clone(), Style::new().fg(theme.accent)),
                Span::styled("▏", Style::new().fg(theme.secondary)),
            ]),
            prompt.error.as_ref().map_or_else(
                || Line::styled("enter save · esc cancel", Style::new().fg(theme.dim)),
                |error| Line::styled(error.clone(), Style::new().fg(theme.secondary)),
            ),
        ];
        frame.render_widget(Clear, box_area);
        frame.render_widget(
            Paragraph::new(text).block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().fg(theme.accent))
                    .title(Span::styled(" save ", Style::new().fg(theme.accent).bold()))
                    .padding(ratatui::widgets::Padding::new(1, 1, 1, 0))
                    .style(Style::new().bg(theme.background)),
            ),
            box_area,
        );
    }

    /// The append overlay (issue #4): a small card listing the saved names,
    /// the cursor highlighted, `enter add · esc cancel` at its foot.
    fn draw_playlist_picker(frame: &mut Frame, area: Rect, picker: &PlaylistPicker, theme: &Theme) {
        let height = (u16::try_from(picker.names.len()).unwrap_or(12) + 4).min(12);
        let width = area.width.saturating_sub(8).clamp(24, 40);
        let box_area = centered(area, width, height);
        let mut text = vec![Line::styled(
            "Add to which playlist?",
            Style::new().fg(theme.text),
        )];
        text.extend(picker.names.iter().enumerate().map(|(index, name)| {
            if index == picker.selected {
                Line::styled(name.clone(), Style::new().fg(theme.accent).bold())
            } else {
                Line::styled(name.clone(), Style::new().fg(theme.text))
            }
        }));
        text.push(Line::styled(
            "enter add · esc cancel",
            Style::new().fg(theme.dim),
        ));
        frame.render_widget(Clear, box_area);
        frame.render_widget(
            Paragraph::new(text).block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().fg(theme.accent))
                    .title(Span::styled(
                        " add to playlist ",
                        Style::new().fg(theme.accent).bold(),
                    ))
                    .padding(ratatui::widgets::Padding::new(1, 1, 1, 0))
                    .style(Style::new().bg(theme.background)),
            ),
            box_area,
        );
    }

    /// How many rows the current level has.
    fn row_count(&self) -> usize {
        match &self.position {
            Position::Files => self.argv.len(),
            Position::Artists => self.artists.len(),
            Position::Albums { .. } | Position::AllAlbums => self.albums.len(),
            Position::Tracks { .. } | Position::AllTracks | Position::Genre { .. } => {
                self.tracks.len()
            }
            Position::Recent => self.recent_rows.len(),
            Position::Favourites => self.favourite_rows.len(),
            Position::Playlists => self.playlist_rows.len(),
            Position::Playlist { .. } => self.open_playlist_tracks().len(),
            Position::Genres => self.genres.len(),
        }
    }

    /// What an empty level says about itself, if it is empty.
    fn empty_message(&self) -> Option<&'static str> {
        match &self.position {
            Position::Files if self.argv.is_empty() => {
                Some("no tracks — pass files on the command line")
            }
            _ if self.row_count() > 0 => None,
            Position::Recent => Some("nothing played yet"),
            Position::Favourites => Some("nothing favourited yet"),
            Position::Playlists => Some("no playlists yet — save the queue with S"),
            Position::Playlist { .. } => Some("the playlist is empty"),
            Position::Artists | Position::AllAlbums | Position::AllTracks | Position::Genres
                if self.library_setup.is_some() =>
            {
                Some(
                    "no library set — add library = \"~/Music\" to ~/.config/velo/config.toml, or run velo --library <PATH>",
                )
            }
            Position::Artists => Some("no artists in the index yet"),
            Position::AllAlbums => Some("no albums in the index yet"),
            Position::AllTracks => Some("no tracks in the index yet"),
            Position::Genres => Some("no genres in the index yet"),
            Position::Albums { .. } => Some("no albums for this artist"),
            Position::Tracks { .. } => Some("no tracks in this album"),
            Position::Genre { .. } => Some("no tracks in this genre"),
            Position::Files => None,
        }
    }

    /// The list position of the row the transport stands on — whether or
    /// not it still plays (issue #53) — so the marker finds the row wherever
    /// a list shows whole tracks. Found by path, not by where the stream
    /// started plus the producer's index, because a shuffled queue no longer
    /// follows the list's order.
    fn playing_row(&self) -> Option<usize> {
        match &self.position {
            Position::Files => {
                let path = self.playing_path()?;
                self.argv.iter().position(|row| row == path)
            }
            // The recent list holds the file's entries (issue #33): a
            // station row is marked when its station key is the transport's
            // station's; a file row compares paths as everywhere else. The
            // standing entry is read fresh every frame (issue #53), so a
            // stopped stream still finds its station here.
            Position::Recent => {
                let standing = self.standing_entry()?;
                self.recent_rows
                    .iter()
                    .position(|row| match (&row.kind, standing) {
                        (crate::history::Kind::Station, Entry::Station { url, .. }) => {
                            crate::radio::store::station_key(&row.path)
                                == crate::radio::store::station_key(url)
                        }
                        (crate::history::Kind::File, Entry::File(path)) => {
                            row.path == path.to_string_lossy()
                        }
                        _ => false,
                    })
            }
            Position::Tracks { .. } | Position::AllTracks | Position::Genre { .. } => {
                let path = self.playing_path()?;
                self.tracks.iter().position(|row| row.path == path)
            }
            Position::Favourites => {
                let path = self.playing_path()?;
                self.favourite_rows
                    .iter()
                    .position(|favorite| favorite.path == path.to_string_lossy())
            }
            Position::Playlist { .. } => {
                let path = self.playing_path()?;
                self.open_playlist_tracks()
                    .iter()
                    .position(|track| *track == path.to_string_lossy())
            }
            Position::Artists
            | Position::Albums { .. }
            | Position::AllAlbums
            | Position::Genres
            | Position::Playlists => None,
        }
    }

    /// The search keys while a search is open: they type into the query and
    /// move through the results, and never reach the browse state below.
    fn on_search_key(&mut self, code: ratatui::crossterm::event::KeyCode) -> Action {
        use ratatui::crossterm::event::KeyCode;

        match code {
            KeyCode::Esc => {
                // Cancel. The interface never moved while the search was open,
                // so the position and selection it had are still there.
                self.close_search();
                Action::None
            }
            KeyCode::Backspace => {
                self.backspace_search();
                Action::None
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.move_search_selection(true);
                Action::None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.move_search_selection(false);
                Action::None
            }
            // The paging keys (issue #38): the result list is a list like
            // any other.
            KeyCode::Home => {
                self.search_selected = 0;
                Action::None
            }
            KeyCode::End => {
                self.search_selected = self.search_results.len().saturating_sub(1);
                Action::None
            }
            KeyCode::PageDown => {
                self.move_search_page(true);
                Action::None
            }
            KeyCode::PageUp => {
                self.move_search_page(false);
                Action::None
            }
            KeyCode::Enter if self.search_results.is_empty() => Action::None,
            KeyCode::Enter => Action::Play {
                from: self.search_selected,
            },
            KeyCode::Char(character) => {
                if let Some(search) = &mut self.search {
                    search.push(character);
                }
                self.rerank();
                Action::None
            }
            _ => Action::None,
        }
    }

    /// Reads every track of the library once and opens the search over it.
    fn open_search(&mut self) {
        let Some(library) = self.library else {
            return;
        };
        match library.all_tracks() {
            Ok(pool) => {
                self.set_search_pool(pool);
                self.search_results = (0..self.search_pool.len()).collect();
                self.search_selected = 0;
                self.search = Some(Search::new());
            }
            Err(error) => self.status = format!("velo: {error}"),
        }
    }

    /// The tracks the search ranks, with the text each one is matched on.
    fn set_search_pool(&mut self, pool: Vec<TrackEntry>) {
        self.search_haystacks = pool
            .iter()
            .map(|entry| format!("{} {} {}", entry.artist, entry.album, entry.title))
            .collect();
        self.search_pool = pool;
    }

    /// After an index, an open search reads the pool again and ranks the
    /// query over it; the selected result stays selected while its track is
    /// still there. A pool that cannot be read keeps the last one.
    fn reload_search(&mut self) {
        let (Some(library), Some(_)) = (self.library, &self.search) else {
            return;
        };
        let kept = self
            .search_results
            .get(self.search_selected)
            .and_then(|&index| self.search_pool.get(index))
            .map(|entry| entry.path.clone());
        match library.all_tracks() {
            Ok(pool) => self.set_search_pool(pool),
            Err(error) => {
                self.status = format!("velo: {error}");
                return;
            }
        }
        self.rerank();
        if let Some(found) = kept.and_then(|path| {
            self.search_results
                .iter()
                .position(|&index| self.search_pool[index].path == path)
        }) {
            self.search_selected = found;
        }
    }

    /// Ranks the pool again after the query changed, keeping the selection on
    /// the screen.
    fn rerank(&mut self) {
        let Some(search) = &self.search else {
            return;
        };
        self.search_results = search.rank(&self.search_haystacks);
        self.search_selected = self
            .search_selected
            .min(self.search_results.len().saturating_sub(1));
    }

    fn backspace_search(&mut self) {
        let Some(search) = &mut self.search else {
            return;
        };
        if search.pop().is_some() {
            self.rerank();
        }
    }

    fn move_search_selection(&mut self, down: bool) {
        self.search_selected = if down {
            (self.search_selected + 1).min(self.search_results.len().saturating_sub(1))
        } else {
            self.search_selected.saturating_sub(1)
        };
    }

    /// Moves the search's selection a page of rows down or up (issue #38),
    /// stopping at the ends of the result list.
    fn move_search_page(&mut self, down: bool) {
        let len = self.search_results.len();
        if len == 0 {
            return;
        }
        let step = self.page_rows();
        self.search_selected = if down {
            (self.search_selected + step).min(len - 1)
        } else {
            self.search_selected.saturating_sub(step)
        };
    }

    /// Closes the search, dropping the pool with it; the next `/` re-reads
    /// the library, which may have been re-indexed in the meantime.
    fn close_search(&mut self) {
        self.search = None;
        self.search_pool = Vec::new();
        self.search_haystacks = Vec::new();
        self.search_results = Vec::new();
        self.search_selected = 0;
    }

    /// Plays the search results from the selection onwards: the queue becomes
    /// every result from that one to the end. The search closes, so the
    /// interface returns to exactly where it was. A remote result plays like
    /// a local one: the producer resolves it through the cache when the
    /// stream reaches it (T12c).
    fn play_search_from(&mut self, from: usize) {
        let paths: Vec<PathBuf> = self
            .search_results
            .get(from..)
            .map_or(Vec::new(), |picked| {
                picked
                    .iter()
                    .filter_map(|&index| self.search_pool.get(index))
                    .map(|entry| entry.path.clone())
                    .collect()
            });
        if paths.is_empty() {
            return;
        }
        self.close_search();
        self.start_stream(
            paths.into_iter().map(Entry::from).collect(),
            Playback::Queue,
        );
    }

    /// How many rows the view the keys act on holds.
    fn view_len(&self) -> usize {
        match self.view {
            // The sources screen's list is its own; the keys that reach here
            // never do while it is up (issue #3), so it follows the library.
            View::Library | View::Sources => self.row_count(),
            View::Queue => self.queue.len(),
        }
    }

    /// The selected row of the view the keys act on.
    const fn view_selection(&self) -> usize {
        match self.view {
            // Unreachable while the sources screen is up (issue #3).
            View::Library | View::Sources => self.selected,
            View::Queue => self.queue_selected,
        }
    }

    fn set_view_selection(&mut self, index: usize) {
        match self.view {
            // Unreachable while the sources screen is up (issue #3).
            View::Library | View::Sources => self.selected = index,
            View::Queue => self.queue_selected = index,
        }
    }

    /// Whether the list the keys act on wraps at its ends (issue #36):
    /// every navigation list does — the sources, the library and its
    /// source lists, the radio — while the queue keeps stopping at its
    /// ends, because its `j`/`k` move over entries that get removed and
    /// reordered, and a jump from the last entry to the first reads as a
    /// mistake.
    const fn navigation_wraps(&self) -> bool {
        !matches!(self.view, View::Queue)
    }

    fn select_next(&mut self) {
        let len = self.view_len();
        if len == 0 {
            return;
        }
        let index = self.view_selection();
        let next = if self.navigation_wraps() {
            (index + 1) % len
        } else {
            (index + 1).min(len - 1)
        };
        self.set_view_selection(next);
    }

    fn select_previous(&mut self) {
        let len = self.view_len();
        if len == 0 {
            return;
        }
        let index = self.view_selection();
        let previous = if self.navigation_wraps() {
            (index + len - 1) % len
        } else {
            index.saturating_sub(1)
        };
        self.set_view_selection(previous);
    }

    fn select_first(&mut self) {
        self.set_view_selection(0);
    }

    fn select_last(&mut self) {
        self.set_view_selection(self.view_len().saturating_sub(1));
    }

    /// Moves the selection a page of rows down or up (issue #38), stopping
    /// at the ends. Before the first draw the page is one row and the keys
    /// step like `j`/`k` — never a jump into the dark.
    fn select_page(&mut self, down: bool) {
        let len = self.view_len();
        if len == 0 {
            return;
        }
        let step = self.page_rows();
        let index = self.view_selection();
        let target = if down {
            index.saturating_add(step).min(len - 1)
        } else {
            index.saturating_sub(step)
        };
        self.set_view_selection(target);
    }

    /// How many rows the body showed the last time it was drawn (issue #38):
    /// the list's own page, the same arithmetic `draw` runs — the window's
    /// borders, the bars the switches take and give back, and the header.
    /// Nothing drawn yet, one row at a time.
    fn page_rows(&self) -> usize {
        let area = self.seen_area.get();
        if area.height == 0 {
            return 1;
        }
        let footer = u16::from(self.ui.hints) + 1;
        let top = u16::from(self.ui.top_bar);
        let content = area.height.saturating_sub(2 + footer + top);
        let now = self.now_playing_rows(content);
        usize::from(content.saturating_sub(now + 1).max(1))
    }

    /// Moves the selected entry down or up (issue #38): `J`/`K` and their
    /// `shift+arrow` aliases. The queue's arms own the queue view; a
    /// playlist's track list is the library view's rows.
    fn move_selection(&mut self, delta: i64) {
        if self.view == View::Queue {
            self.move_in_queue(delta);
        } else if matches!(self.position, Position::Playlist { .. }) {
            self.move_in_playlist(delta);
        }
    }

    /// The tab round trip (issue #23): from the queue, back to the screen
    /// `tab` was pressed on; from anywhere else — the sources screen, the
    /// library at any level, the radio at any depth — to the queue. The
    /// status line names the queue when it arrives and is cleared when it
    /// leaves.
    fn toggle_view(&mut self) {
        if self.view == View::Queue {
            self.view = self.tab_origin;
            self.status.clear();
        } else {
            self.open_queue();
        }
        // The tab is a transient round trip, not a setting (S14): moving it
        // writes nothing.
    }

    /// Opens the queue view (issue #38): `p`, the way `tab`'s first half
    /// does — the screen it leaves is remembered for the way back, and the
    /// status line names the queue. Already there, it stays.
    fn open_queue(&mut self) {
        if self.view == View::Queue {
            return;
        }
        self.tab_origin = self.view;
        self.view = View::Queue;
        self.queue_selected = self.queue_selected.min(self.queue.len().saturating_sub(1));
        self.status = format!(
            "velo: queue, {}",
            plural(self.queue.len(), "track", "tracks")
        );
    }

    /// The queue index that is playing, when anything is: the player counts
    /// from the list it was given, which is this queue. Production sets
    /// `playing` and the player together; the tests' fixture (no player) reads
    /// the queue's head, the same entry [`Self::playing_entry`] names.
    fn playing_queue_index(&self) -> Option<usize> {
        self.playing.as_ref()?;
        Some(self.player.as_ref().map_or(0, Player::current_index))
    }

    /// Applies an edit to the queue and to the handle the player reads, in that
    /// order and in one place: two call sites that had to agree would drift.
    fn edit_queue(&mut self, edit: impl FnOnce(&mut Vec<Entry>)) {
        edit(&mut self.queue);
        if let Some(shared) = &self.shared {
            match shared.lock() {
                Ok(mut queue) => queue.clone_from(&self.queue),
                Err(poisoned) => poisoned.into_inner().clone_from(&self.queue),
            }
        }
    }

    /// The queue index an edit may touch, or why it may not. Only entries after
    /// the one playing are editable: that keeps the producer's index valid
    /// without renumbering, and the entries before it are history anyway.
    fn editable_queue_index(&self, index: usize) -> Result<usize, String> {
        Self::editable_queue_index_among(self.queue.len(), self.playing_queue_index(), index)
    }

    /// The edit rule on its own terms, so the playing-entry refusals are
    /// testable without opening a sound device: given a queue of `len`
    /// entries with entry `playing` being played, `index` may be edited only
    /// when it is neither the playing entry nor anything before it.
    fn editable_queue_index_among(
        len: usize,
        playing: Option<usize>,
        index: usize,
    ) -> Result<usize, String> {
        if index >= len {
            return Err("velo: nothing selected in the queue".to_owned());
        }
        match playing {
            Some(playing) if index == playing => {
                Err("velo: cannot remove the track that is playing".to_owned())
            }
            Some(playing) if index < playing => Err("velo: that track already played".to_owned()),
            _ => Ok(index),
        }
    }

    fn remove_from_queue(&mut self) {
        let Ok(index) = self.editable_queue_index(self.queue_selected) else {
            self.status = self.editable_queue_index(self.queue_selected).unwrap_err();
            return;
        };
        let name = self.queue[index].label().into_owned();
        self.edit_queue(|queue| {
            queue.remove(index);
        });
        self.queue_selected = self.queue_selected.min(self.queue.len().saturating_sub(1));
        self.status = format!("velo: removed {name}");
    }

    fn move_in_queue(&mut self, delta: i64) {
        let Ok(index) = self.editable_queue_index(self.queue_selected) else {
            self.status = self.editable_queue_index(self.queue_selected).unwrap_err();
            return;
        };
        let Some(target) =
            Self::queue_move_target(self.queue.len(), self.playing_queue_index(), index, delta)
        else {
            "velo: cannot move past the track that is playing".clone_into(&mut self.status);
            return;
        };
        self.edit_queue(|queue| queue.swap(index, target));
        self.queue_selected = target;
    }

    /// The move rule on its own terms: the index `J`/`K` may move entry
    /// `index` to, or `None` when the move is refused — landing outside the
    /// queue, or on, or across, the entry that is playing.
    fn queue_move_target(
        len: usize,
        playing: Option<usize>,
        index: usize,
        delta: i64,
    ) -> Option<usize> {
        let target = i64::try_from(index).ok()?.checked_add(delta)?;
        let target = usize::try_from(target).ok()?;
        let reachable = target < len && playing.is_none_or(|playing| target > playing);
        reachable.then_some(target)
    }

    /// Appends the current selection to the queue without interrupting what is
    /// playing. A track appends itself; an album appends its tracks. A remote
    /// row appends itself too (T12c): the queue holds the `subsonic://…`
    /// entry, and the producer resolves it through the cache when it is
    /// reached — appending never starts a download.
    fn append_selection(&mut self) {
        // A playlist's track list (issue #4) is its own list, not `tracks`:
        // a row appends the store's path verbatim, like a favourite's.
        if matches!(self.position, Position::Playlist { .. }) {
            let Some(path) = self.open_playlist_tracks().get(self.selected) else {
                return;
            };
            let path = PathBuf::from(path);
            self.edit_queue(|queue| queue.push(Entry::from(path)));
            self.status = format!("velo: added {}", plural(1, "track", "tracks"));
            return;
        }
        // The favourites list (issue #7) is its own list, not `tracks`: a
        // row appends the store's path verbatim.
        if matches!(self.position, Position::Favourites) {
            let Some(favorite) = self.favourite_rows.get(self.selected) else {
                return;
            };
            let path = PathBuf::from(&favorite.path);
            self.edit_queue(|queue| queue.push(Entry::from(path)));
            self.status = format!("velo: added {}", plural(1, "track", "tracks"));
            return;
        }
        // The recent list holds the file's entries (issue #33): a station
        // appends itself through the radio's own queue verb, a file appends
        // its path verbatim.
        if matches!(self.position, Position::Recent) {
            let Some(entry) = self.recent_rows.get(self.selected).cloned() else {
                return;
            };
            if entry.is_station() {
                self.queue_station(entry.path, entry.title.unwrap_or_default());
            } else {
                let path = PathBuf::from(&entry.path);
                self.edit_queue(|queue| queue.push(Entry::from(path)));
                self.status = format!("velo: added {}", plural(1, "track", "tracks"));
            }
            return;
        }
        let paths: Vec<PathBuf> = match (&self.position, self.view) {
            (Position::Tracks { .. } | Position::AllTracks | Position::Genre { .. }, _) => {
                match self.tracks.get(self.selected) {
                    Some(entry) => vec![entry.path.clone()],
                    None => return,
                }
            }
            (Position::Files, _) => self
                .argv
                .get(self.selected)
                .cloned()
                .map(|path| vec![path])
                .unwrap_or_default(),
            (Position::Albums { artist }, _) => {
                let Some(album) = self.albums.get(self.selected) else {
                    return;
                };
                let Some(library) = self.library else {
                    return;
                };
                match library.tracks_of(artist, &album.title) {
                    Ok(tracks) => tracks.into_iter().map(|entry| entry.path).collect(),
                    Err(error) => {
                        self.status = format!("velo: {error}");
                        return;
                    }
                }
            }
            (Position::AllAlbums | Position::Genres, _) => {
                let Some(library) = self.library else {
                    return;
                };
                let found = if let Position::AllAlbums = self.position {
                    let Some(album) = self.albums.get(self.selected) else {
                        return;
                    };
                    library.tracks_of(&album.artist, &album.title)
                } else {
                    let Some(genre) = self.genres.get(self.selected) else {
                        return;
                    };
                    library.tracks_of_genre(&genre.name)
                };
                match found {
                    Ok(tracks) => tracks.into_iter().map(|entry| entry.path).collect(),
                    Err(error) => {
                        self.status = format!("velo: {error}");
                        return;
                    }
                }
            }
            (Position::Artists, _) => {
                "velo: open an album to add its tracks".clone_into(&mut self.status);
                return;
            }
            // The recent and favourites lists are answered above: their rows
            // are the store's entries, not `tracks` rows (issue #33, #7). The
            // playlist's track list is its own too (issue #4). The arms below
            // stand only for exhaustiveness; none of them can be reached.
            (Position::Recent | Position::Favourites | Position::Playlist { .. }, _) => return,
            // The names list has no track to add — it says so instead of
            // doing nothing (issue #38, S4).
            (Position::Playlists, _) => {
                "velo: nothing to queue here".clone_into(&mut self.status);
                return;
            }
        };
        if paths.is_empty() {
            return;
        }
        let count = paths.len();
        self.edit_queue(|queue| queue.extend(paths.into_iter().map(Entry::from)));
        self.status = format!("velo: added {}", plural(count, "track", "tracks"));
    }

    /// Moves the volume by `percent`, clamped to 0..=100.
    fn change_volume(&mut self, percent: i16) -> u8 {
        let moved = i16::from(self.volume) + percent;
        self.volume = u8::try_from(moved.clamp(0, 100)).unwrap_or(0);
        self.volume
    }

    /// Goes one level deeper, re-reading the child snapshot from the index.
    /// Query errors are reported instead of leaving the level unchanged.
    fn descend(&mut self) {
        let Some(library) = self.library else {
            return;
        };
        match &self.position {
            Position::Artists => {
                let Some(entry) = self.artists.get(self.selected) else {
                    return;
                };
                let artist = entry.name.clone();
                match library.albums_of(&artist) {
                    Ok(albums) => {
                        self.position = Position::Albums { artist };
                        self.albums = albums;
                        self.selected = 0;
                    }
                    Err(error) => self.status = format!("velo: {error}"),
                }
            }
            Position::Albums { artist } => {
                let Some(entry) = self.albums.get(self.selected) else {
                    return;
                };
                let album = entry.title.clone();
                match library.tracks_of(artist, &album) {
                    Ok(tracks) => {
                        self.position = Position::Tracks {
                            artist: artist.clone(),
                            album,
                        };
                        self.tracks = tracks;
                        self.selected = 0;
                    }
                    Err(error) => self.status = format!("velo: {error}"),
                }
            }
            Position::AllAlbums => {
                let Some(entry) = self.albums.get(self.selected) else {
                    return;
                };
                let (artist, album) = (entry.artist.clone(), entry.title.clone());
                match library.tracks_of(&artist, &album) {
                    Ok(tracks) => {
                        self.position = Position::Tracks { artist, album };
                        self.tracks = tracks;
                        self.selected = 0;
                    }
                    Err(error) => self.status = format!("velo: {error}"),
                }
            }
            Position::Genres => {
                let Some(entry) = self.genres.get(self.selected) else {
                    return;
                };
                let genre = entry.name.clone();
                match library.tracks_of_genre(&genre) {
                    Ok(tracks) => {
                        self.position = Position::Genre { genre };
                        self.tracks = tracks;
                        self.selected = 0;
                    }
                    Err(error) => self.status = format!("velo: {error}"),
                }
            }
            // The names list (issue #4) descends into the chosen playlist's
            // track list — no index query, the rows are the store's.
            Position::Playlists => {
                let Some(entry) = self.playlist_rows.get(self.selected) else {
                    return;
                };
                let name = entry.name.clone();
                self.open_playlist(name);
                return;
            }
            // `Files` does not descend; track lists are the deepest levels
            // and answer with `Action::Play` instead.
            Position::Files
            | Position::Tracks { .. }
            | Position::AllTracks
            | Position::Genre { .. }
            | Position::Recent
            | Position::Favourites
            | Position::Playlist { .. } => {}
        }
        // The open node is persisted (S3), but only when it changed.
        self.remember_node();
    }

    /// Goes one level up. The parent's row snapshot is kept as it was when
    /// the level was entered — no query runs on ascent.
    fn ascend(&mut self) {
        match &self.position {
            Position::Files
            | Position::Artists
            | Position::AllAlbums
            | Position::AllTracks
            | Position::Genres => {
                // Already at the top of the library: one step back is the
                // sources screen (S6). The flat command-line list has none.
                if self.view == View::Library && !matches!(self.position, Position::Files) {
                    self.view = View::Sources;
                }
            }
            Position::Albums { .. } => {
                self.position = Position::Artists;
                self.selected = 0;
            }
            // An album opened from every album goes back to every album.
            Position::Tracks { .. } if self.browse_by == BrowseBy::Albums => {
                self.position = Position::AllAlbums;
                self.selected = 0;
            }
            Position::Tracks { artist, .. } => {
                self.position = Position::Albums {
                    artist: artist.clone(),
                };
                self.selected = 0;
            }
            Position::Genre { .. } => {
                self.position = Position::Genres;
                self.selected = 0;
            }
            // A playlist's track list climbs to the names list (issue #4):
            // the names are their own rows, still held from when the source
            // opened — a playlist is one level above the favourites' walk.
            Position::Playlist { .. } => {
                self.position = Position::Playlists;
                self.selected = 0;
            }
            // The recent list (issue #5), the favourites list (issue #7) and
            // the playlists names list (issue #4) are their own lists, not
            // levels of the library: there is no parent snapshot to keep —
            // `open_recent`/`open_favourites`/`open_playlists` replaced the
            // tracks — so ascending lands on `Sources` over the top of the
            // library view, read again from the index, the way the top of
            // the library reaches `Sources` (S6). The saved node was never
            // these lists' to walk out of, so nothing is persisted.
            Position::Recent | Position::Favourites | Position::Playlists => {
                self.position = match self.browse_by {
                    BrowseBy::Artists => Position::Artists,
                    BrowseBy::Albums => Position::AllAlbums,
                    BrowseBy::Tracks => Position::AllTracks,
                    BrowseBy::Genres => Position::Genres,
                };
                self.selected = 0;
                self.load_top();
                if self.view == View::Library {
                    self.view = View::Sources;
                }
                return;
            }
        }
        // The open node is persisted (S3): walking out of a level removes
        // its keys, so the file never keeps a node the user has left.
        self.remember_node();
    }

    /// Starts a gapless stream of [`App::playlist_from`]: on a track list
    /// that is the rest of the album, of the genre or of every track, from the
    /// selection onwards; a track that cannot play is reported in the status
    /// line instead of exiting. A remote row plays like a local one (T12c):
    /// the producer resolves it through the cache when the stream reaches it.
    fn play_from(&mut self, from: usize) {
        let paths = self.playlist_from(from);
        if paths.is_empty() {
            return;
        }
        let playback = match self.position {
            Position::Files => Playback::Files,
            Position::Tracks { .. } => Playback::Album,
            _ => Playback::List,
        };
        // The recent list is not `tracks` (issue #33): its rows are the
        // history's entries and the library snapshot is somebody else's, so
        // the describing pool keeps what it had. A playlist's track list is
        // the same kind of guest (issue #4): its rows are the store's paths.
        if !matches!(
            self.position,
            Position::Files | Position::Recent | Position::Playlist { .. }
        ) {
            self.played = self.tracks[from..].to_vec();
        }
        self.start_stream(paths.into_iter().map(Entry::from).collect(), playback);
    }

    /// Plays the queue itself from `from`, rotating it so that entry is first:
    /// the player counts from the list it is given, and every marker in the
    /// interface counts from the same place.
    fn play_queue_from(&mut self, from: usize) {
        if from >= self.queue.len() {
            return;
        }
        let paths = self.queue[from..].to_vec();
        self.start_stream(paths, Playback::Queue);
    }

    /// Whether starting `first` over the running stream is a crossfade
    /// handover (issue #65): the seam is a station — its open (connect,
    /// headers, first bytes) is the wait that used to be a cut plus a second
    /// of silence — taken over a stream that is sounding. A file start keeps
    /// the plain switch (it opens in milliseconds, and the gapless
    /// track-to-track advance inside one player is a different seam
    /// entirely), a stream that is not sounding has nothing to hand over
    /// from, and bit-perfect refuses the fade: a fade multiplies samples,
    /// and bit-perfect means the decoder's samples reach the device
    /// unmodified.
    fn crossfade_wanted(&self, first: Option<&Entry>) -> bool {
        matches!(first, Some(Entry::Station { .. }))
            && !self.bit_perfect
            && self.state == PlayState::Playing
    }

    /// Drives [`Self::crossfade_wanted`] for the tests: no test can hold a
    /// `Player` — building one opens an audio device — so the decision's own
    /// shape is reached through this hook; production reaches it through the
    /// starts. Hidden from the docs.
    #[doc(hidden)]
    pub fn crossfade_wanted_for_tests(&self, first: Option<&Entry>) -> bool {
        self.crossfade_wanted(first)
    }

    /// Ends a handover in flight: the previous stream — already falling or
    /// fallen — is stopped outright, because a new start replaces it. Stopping
    /// a stream whose fade had not begun is the accepted cost of starting
    /// something else over an unfinished handover.
    fn drop_crossfade(&mut self) {
        if let Some(old) = self.crossfading.take() {
            old.player.stop();
        }
    }

    /// The handover's clock (issue #65), run once per tick: the old stream's
    /// fade-out is armed when the new one has started sounding — the overlap
    /// must be two sources sounding, never two fades of silence — and the
    /// old player stops once its fade has fully fallen, exact silence
    /// reached, never a fader stuck half-way.
    fn poll_crossfade(&mut self) {
        let started = self.player.as_ref().is_some_and(Player::stream_started);
        if let Some(handover) = self.crossfading.as_mut()
            && !handover.armed
            && started
        {
            handover.player.begin_crossfade_out();
            handover.armed = true;
        }
        if self
            .crossfading
            .as_ref()
            .is_some_and(|handover| handover.armed && handover.player.crossfade_out_finished())
        {
            let old = self.crossfading.take().expect("checked above").player;
            old.stop();
        }
    }

    /// Opens a device for these entries and makes them the queue. The first
    /// entry decides where the turn waits: an uncached remote one is a
    /// download the event loop must never sit in (issue #60), so its resolve
    /// is handed to a worker and the same start runs again when the file is
    /// on disk; everything else opens here, on this thread, as it always
    /// has.
    fn start_stream(&mut self, paths: Vec<Entry>, playback: Playback) {
        // A handover still falling is replaced outright: the new start is
        // what the user asked for.
        self.drop_crossfade();
        // The old stream dies here — before the new one exists — only when
        // this start is not a station handover (issue #65): a handover keeps
        // it sounding while the new source opens, which is the point.
        if !self.crossfade_wanted(paths.first())
            && let Some(old) = self.player.take()
        {
            old.stop();
        }
        // Under shuffle the first track is the one asked for and the rest
        // follow mixed; the order they came in is kept to put them back.
        let mut paths = paths;
        self.unshuffled.clone_from(&paths);
        if self.shuffle {
            shuffle::shuffle_after(&mut paths, 1, shuffle::seed());
        }
        let first = paths.first().cloned();
        let waits = first
            .as_ref()
            .and_then(Entry::as_path)
            .is_some_and(|path| self.is_uncached_remote(path));
        if waits && let Some(Entry::File(path)) = first {
            self.start_as_pending(path, PendingStart::Stream { paths, playback });
            return;
        }
        self.open_stream(paths, playback);
    }

    /// The synchronous half of a fresh start: local files and already-cached
    /// entries open the player on this thread, exactly as they always have
    /// (issue #60) — no new state, no extra tick — and the start clears
    /// whatever a superseded fetch left waiting in the slot, since this start
    /// is the one the user asked for. Production reaches this through
    /// [`Self::start_stream`] and, once a fetch has landed, through
    /// [`Self::poll_pending_resolve`].
    fn open_stream(&mut self, paths: Vec<Entry>, playback: Playback) {
        // A finished fetch replays here with no player running — the wait
        // outlived the last one — but a start of any kind retires the slot
        // all the same, so a stale wait can never start behind it.
        self.pending_resolve = None;
        // The handover decision is remade here, at the open: a start that
        // arrives through a finished fetch is always a file start, which is
        // never a handover, and asking the same question once more is
        // cheaper than threading the answer through (issue #65).
        let crossfade = self.crossfade_wanted(paths.first());
        // The player reads the queue it is given, so the interface keeps the
        // handle and edits it in place (T18).
        let queue = Arc::new(Mutex::new(paths.clone()));
        // The first entry is resolved at selection time, on this thread
        // (T12c): an uncached remote one is a download the user waits for,
        // so the status line says so before the wait begins (issue #60).
        self.note_fetching(paths.first());
        match Player::play_gapless_at_full(
            Arc::clone(&queue),
            0,
            Duration::ZERO,
            self.replay_gain,
            self.repeat,
            self.mono,
            self.bit_perfect,
            // The session's device (issue #44, S3): the config's choice at
            // launch, the picker's after a switch.
            self.audio_device.as_deref(),
            self.resolver.clone(),
        ) {
            Ok(player) => {
                player.set_volume(self.volume);
                if crossfade && let Some(old) = self.player.take() {
                    // The handover (issue #65): the new stream rises from
                    // exact silence over its window while the old one keeps
                    // sounding — it was never stopped. Its fade-out is armed
                    // once the new stream has started (the tick's
                    // [`Self::poll_crossfade`]), and the old player stops
                    // when the fade has fully fallen.
                    player.begin_crossfade_in();
                    self.crossfading = Some(Crossfading {
                        player: old,
                        armed: false,
                    });
                }
                self.queue = paths;
                self.shared = Some(queue);
                self.player = Some(player);
                self.playing = Some(playback);
                self.state = PlayState::Playing;
                // A fetching line outlives the start (issue #60): the wait it
                // names is only beginning, and `fetch_status` clears it when
                // the track sounds. Every other message has had its moment.
                if !self.status.starts_with("⟳ fetching ") {
                    self.status.clear();
                }
            }
            Err(error) => {
                if self.player.is_some() {
                    // A failed open under a handover never touched the old
                    // stream: it keeps playing, and the status line says both
                    // facts (issue #65) — never a fader stuck mid-fade with
                    // silence behind it.
                    self.status = format!("velo: {error} — the stream that was playing goes on");
                } else {
                    self.queue.clear();
                    self.shared = None;
                    self.player = None;
                    self.playing = None;
                    self.state = PlayState::Stopped;
                    self.status = format!("velo: {error}");
                }
            }
        }
    }

    /// Moves the position by `delta_secs`: the current position plus the
    /// delta, clamped to the track, then the player rebuilt from that offset
    /// (T6). A few milliseconds of silence are expected; a click is not.
    fn seek_by(&mut self, delta_secs: i64) {
        if self.refuse_live_seek() {
            return;
        }
        let Some(player) = &self.player else {
            return;
        };
        let position = player.position();
        let duration = self.current_track_duration();
        let target = seek_target(position, delta_secs, duration);
        self.start_at(target);
    }

    /// Jumps to an absolute position (issue #6), clamped to the end of the
    /// track, and refused by a live station with the sentence the relative
    /// seek already says.
    fn seek_to(&mut self, target: Duration) {
        if self.refuse_live_seek() {
            return;
        }
        if self.player.is_none() {
            return;
        }
        let duration = self.current_track_duration();
        self.start_at(duration.map_or(target, |duration| target.min(duration)));
    }

    /// Whether a live station refused the seek (S3): it has no position to seek
    /// within, and saying so is cheaper than rebuilding the player — which
    /// would refetch the stream. One answer, both gears.
    fn refuse_live_seek(&mut self) -> bool {
        if matches!(self.playing_entry(), Some(Entry::Station { .. })) {
            self.note("velo: nothing to seek in a live station");
            return true;
        }
        false
    }

    /// Low power (issue #11, S1–S2): the redraw cadence drops to a quarter
    /// of its ordinary rate and the spectrum is left out entirely;
    /// playback, the clock, the volume and every other key are untouched.
    /// The key writes `[ui] low_power` the way `z` writes `[state] shuffle`
    /// — once per change against what the file already said.
    fn toggle_low_power(&mut self) {
        self.ui.low_power = !self.ui.low_power;
        if self.ui.low_power != self.saved_low_power {
            self.saved_low_power = self.ui.low_power;
            self.remember(
                Some("ui"),
                "low_power",
                if self.ui.low_power { "true" } else { "false" },
            );
        }
    }

    /// Mixes what is left of the queue, or puts it back in the order it came
    /// in. What played and what plays now never move, and the change goes
    /// through the one queue edit, so the player follows it at its next track.
    fn toggle_shuffle(&mut self) {
        let keep = self.playing_queue_index().map_or(0, |index| index + 1);
        self.shuffle = !self.shuffle;
        if self.shuffle {
            self.unshuffled.clone_from(&self.queue);
            let seed = shuffle::seed();
            self.edit_queue(|queue| shuffle::shuffle_after(queue, keep, seed));
        } else {
            let original = std::mem::take(&mut self.unshuffled);
            self.edit_queue(|queue| shuffle::restore_order(queue, keep, &original));
        }
        // The shuffle state is persisted (S2), but only when it changed.
        if self.shuffle != self.saved.shuffle {
            self.saved.shuffle = self.shuffle;
            self.remember(
                Some("state"),
                "shuffle",
                if self.shuffle { "true" } else { "false" },
            );
        }
    }

    /// The queue index `next` goes to while a station plays (issue #17): the
    /// FIRST track of the queue — the human's rule, "ir a la primera pista de
    /// la cola" — not the entry after the station, so an evening of stations
    /// lined up behind it is left whole. `Err` is the one status line the
    /// refusal says; the station keeps playing. Only consulted when the entry
    /// that plays is a station; a queue of tracks alone answers from
    /// [`skip_target`] as it always has.
    fn station_next_target(queue: &[Entry]) -> Result<usize, &'static str> {
        queue
            .iter()
            .position(|entry| matches!(entry, Entry::File(_)))
            .ok_or("velo: no track in the queue — the station keeps playing")
    }

    /// Moves to the next track, or back one (see [`skip_target`]), by
    /// rebuilding the player at that track the way a seek rebuilds it at an
    /// offset. Past the last track it says so instead of stopping the music.
    fn skip(&mut self, forward: bool) {
        let Some(player) = &self.player else {
            return;
        };
        let current = player.current_index();
        // A station playing takes `next` by the human's rule (issue #17), not
        // by position: the first track of the queue, or one refusal that
        // leaves the station playing. `previous` keeps its own rule below.
        if forward && matches!(self.queue.get(current), Some(Entry::Station { .. })) {
            match Self::station_next_target(&self.queue) {
                Ok(track) => return self.start_track_at(track, Duration::ZERO),
                Err(message) => return self.note(message),
            }
        }
        match skip_target(current, self.queue.len(), player.position(), forward) {
            Some(track) => self.start_track_at(track, Duration::ZERO),
            None => self.note("velo: this is the last track"),
        }
    }

    /// Closes the player and starts it again at the same track, `offset` into
    /// it. The queue and the marker stay as they were.
    fn start_at(&mut self, offset: Duration) {
        let Some(track) = self.player.as_ref().map(Player::current_index) else {
            return;
        };
        self.start_track_at(track, offset);
    }

    /// Closes the player and starts the same queue at `track`, `offset` into
    /// it: the one rebuild behind a seek and a skip, so both keep the volume
    /// and the `ReplayGain` mode.
    fn start_track_at(&mut self, track: usize, offset: Duration) {
        // A handover still falling is replaced outright: the new start is
        // what the user asked for.
        self.drop_crossfade();

        // Whatever ran before is closed here — whatever this rebuild goes on
        // to do — unless this start is a station handover (issue #65), which
        // keeps the old stream sounding until the new source can take over.
        // Both callers — skip and seek — hold a player, so this sees `None`
        // only under the tests' hook.
        if self.queue.is_empty() || track >= self.queue.len() {
            if let Some(player) = self.player.take() {
                player.stop();
            }
            self.playing = None;
            self.state = PlayState::Stopped;
            return;
        }
        if !self.crossfade_wanted(self.queue.get(track))
            && let Some(player) = self.player.take()
        {
            player.stop();
        }
        // Same rule as a fresh start (issue #60): the entry the rebuild
        // would open first, being remote and uncached, is a download the
        // event loop never waits for — its resolve goes to a worker, the
        // line says so, and the same rebuild runs when the file is on disk.
        // The queue stands where it was; the rebuild neither advances nor
        // clears it.
        let first = self.queue.get(track).cloned();
        let waits = first
            .as_ref()
            .and_then(Entry::as_path)
            .is_some_and(|path| self.is_uncached_remote(path));
        if waits && let Some(Entry::File(path)) = first {
            self.start_as_pending(path, PendingStart::TrackAt { track, offset });
            return;
        }
        self.open_track_at(track, offset);
    }

    /// The synchronous half of a rebuild: local files and already-cached
    /// entries open the player on this thread, exactly as they always have
    /// (issue #60), and the rebuild clears whatever a superseded fetch left
    /// waiting in the slot. Production reaches this through
    /// [`Self::start_track_at`] and, once a fetch has landed, through
    /// [`Self::poll_pending_resolve`].
    fn open_track_at(&mut self, track: usize, offset: Duration) {
        // A finished fetch replays here with no player running — the wait
        // outlived the last one — but a rebuild of any kind retires the slot
        // all the same, so a stale wait can never start behind it.
        self.pending_resolve = None;
        // The handover decision, remade at the open (issue #65) — a rebuild
        // that arrives through a finished fetch is a file start, which is
        // never a handover.
        let crossfade = self.crossfade_wanted(self.queue.get(track));
        let queue = Arc::new(Mutex::new(self.queue.clone()));
        self.shared = Some(Arc::clone(&queue));
        // A skip or a seek reaches its entry at selection time too (T12c):
        // an uncached remote one is a download, so the line says so before
        // the wait begins (issue #60).
        let fetching = self.queue.get(track).cloned();
        self.note_fetching(fetching.as_ref());
        match Player::play_gapless_at_full(
            queue,
            track,
            offset,
            self.replay_gain,
            self.repeat,
            self.mono,
            self.bit_perfect,
            // The session's device (issue #44, S3): a seek and a skip stay
            // on the device the session chose, exactly as they keep the
            // volume and the gain mode.
            self.audio_device.as_deref(),
            self.resolver.clone(),
        ) {
            Ok(player) => {
                // A rebuilt player starts at its own default level; without this
                // a seek would silently jump the volume back to it.
                player.set_volume(self.volume);
                if crossfade && let Some(old) = self.player.take() {
                    // The handover (issue #65), the same one a fresh start
                    // makes: the new stream rises from exact silence while
                    // the old one keeps sounding, its fade-out armed once the
                    // new stream has started, and it stops when the fade has
                    // fully fallen ([`Self::poll_crossfade`]).
                    player.begin_crossfade_in();
                    self.crossfading = Some(Crossfading {
                        player: old,
                        armed: false,
                    });
                }
                self.player = Some(player);
                self.state = PlayState::Playing;
                // A fetching line outlives the rebuild (issue #60), the same
                // rule a fresh start follows.
                if !self.status.starts_with("⟳ fetching ") {
                    self.status.clear();
                }
            }
            Err(error) => {
                if self.player.is_some() {
                    // A failed open under a handover never touched the old
                    // stream: it keeps playing, and the status line says both
                    // facts (issue #65) — never a fader stuck mid-fade with
                    // silence behind it.
                    self.status = format!("velo: {error} — the stream that was playing goes on");
                } else {
                    self.player = None;
                    self.playing = None;
                    self.state = PlayState::Stopped;
                    self.status = format!("velo: {error}");
                }
            }
        }
    }

    fn toggle_pause(&mut self) {
        match (&self.player, self.state) {
            (Some(player), PlayState::Playing) => {
                player.set_playing(false);
                self.state = PlayState::Paused;
            }
            (Some(player), PlayState::Paused) => {
                player.set_playing(true);
                self.state = PlayState::Playing;
            }
            _ => {}
        }
    }

    /// Toggles the mono downmix for the session (issue #8, S2): both channels
    /// carry the average of the mix until `M` is pressed again. Nothing is
    /// persisted. With bit-perfect on it is refused in the status line and
    /// mono stays off (S3): the downmix scales samples, which is exactly what
    /// bit-perfect refuses.
    fn toggle_mono(&mut self) {
        if self.bit_perfect {
            self.note(MONO_WITH_BIT_PERFECT_REFUSED);
            return;
        }
        self.mono = !self.mono;
        if let Some(player) = &self.player {
            player.set_mono(self.mono);
        }
    }

    fn shutdown(&mut self) {
        if let Some(player) = self.player.take() {
            player.stop();
        }
        // A handover still falling dies with the session too (issue #65).
        self.drop_crossfade();
        // A fetch still running answers into a slot nobody polls (issue
        // #60); its worker — a download it may still hold — dies with the
        // session, and its bytes land in the cache whenever the thread gets
        // there.
        self.pending_resolve = None;
        self.state = PlayState::Stopped;
    }
}

impl Default for App<'_> {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

/// A duration as `mm:ss`.
/// Key hints as `key label` pairs, the key bright and the label dim. When the
/// row is too narrow, pairs are dropped from the middle: the first ones say
/// how to move and the last two how to get help and leave, and those last
/// two are never the ones that go.
fn hint_line(hints: &[(&str, &str)], width: u16, theme: &Theme) -> Line<'static> {
    let fits = |pairs: &[&(&str, &str)]| {
        let used: usize = pairs
            .iter()
            .map(|(key, label)| key.chars().count() + 1 + label.chars().count())
            .sum::<usize>()
            + 2 * pairs.len().saturating_sub(1);
        used <= usize::from(width)
    };
    let mut kept: Vec<&(&str, &str)> = hints.iter().collect();
    while !fits(&kept) && kept.len() > 2 {
        kept.remove(kept.len() - 3);
    }
    let mut spans = Vec::new();
    for (index, (key, label)) in kept.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(
            (*key).to_owned(),
            Style::new().fg(theme.secondary),
        ));
        spans.push(Span::styled(
            format!(" {label}"),
            Style::new().fg(theme.dim),
        ));
    }
    Line::from(spans)
}

/// What the marker column of a row draws (issue #53): the transport's state
/// for the row it stands on, and nothing for the rows beside it.
enum RowMark {
    /// Two spaces: the transport stands on no row here.
    None,
    /// `▶` in the accent: the stream runs on this row.
    Playing,
    /// `■` dimmed: the stream stood on this station and stopped — a live
    /// stream cannot pause, so stopped is the only state it can be left in.
    Stopped,
    /// `⏸` in the accent: the track on this row is paused.
    Paused,
}

/// What one list row says, before it is laid out.
struct ListRow {
    /// The transport's state drawn in the marker column ([`RowMark`]).
    mark: RowMark,
    /// The row's number, when it takes one: `None` for a row that stands
    /// above the list's numbering — the cliamp live row (issue #68) — so
    /// the rows below it keep counting from one.
    number: Option<usize>,
    digits: usize,
    name: String,
    /// Dimmed after the name: a folder, or an artist and album.
    note: String,
    /// Dimmed against the right edge: a length, a year, counts.
    fact: String,
}

/// `▶ 3. Dawn  Tide · Harbor ……… 4:12`: the marker — the transport's state
/// in the row's own column (issue #53): `▶` for the row a track plays on,
/// `⏸` for the row paused, `■` for the station a stopped stream stood on,
/// and two spaces for every row beside it (issue #52) — then the number,
/// the name, the note, and the fact against the right edge when it fits
/// beside the rest; a fact that does not fit is the part left out, never
/// the name.
fn list_row(row: ListRow, width: u16, theme: &Theme) -> Line<'static> {
    let lit = matches!(row.mark, RowMark::Playing | RowMark::Paused);
    let (marker, number, name) = if lit {
        (
            Style::new().fg(theme.accent),
            Style::new().fg(theme.accent),
            Style::new().fg(theme.accent).bold(),
        )
    } else {
        (
            Style::new().fg(theme.border),
            Style::new().fg(theme.dim),
            Style::new().fg(theme.text),
        )
    };
    let digits = row.digits;
    let mut spans = vec![Span::styled(
        match row.mark {
            RowMark::Playing => "▶ ",
            RowMark::Paused => "⏸ ",
            RowMark::Stopped => "■ ",
            RowMark::None => "  ",
        },
        marker,
    )];
    if let Some(row_number) = row.number {
        spans.push(Span::styled(format!("{row_number:>digits$}. "), number));
    }
    spans.push(Span::styled(row.name, name));
    if !row.note.is_empty() {
        spans.push(Span::styled(
            format!("  {}", row.note),
            Style::new().fg(theme.dim),
        ));
    }
    let used = Line::from(spans.clone()).width();
    let fact = row.fact.chars().count();
    let width = usize::from(width);
    if fact > 0 && used + 2 + fact <= width {
        spans.push(Span::raw(" ".repeat(width - used - fact)));
        spans.push(Span::styled(row.fact, Style::new().fg(theme.dim)));
    }
    Line::from(spans)
}

/// The queue row of a live station (issue #17): numbered and marked like
/// every row, so the queue edits keep working on it, and showing `● live`
/// against the right edge where a track would show its length — the same
/// fact, in the accent, the transport shows for a live stream.
fn live_row(row: ListRow, width: u16, theme: &Theme) -> Line<'static> {
    const LIVE: &str = "● live";
    let mut line = list_row(row, width, theme);
    let marker_width = LIVE.chars().count();
    let used = line.width();
    let width = usize::from(width);
    if used + 2 + marker_width <= width {
        line.spans
            .push(Span::raw(" ".repeat(width - used - marker_width)));
        line.spans
            .push(Span::styled(LIVE.to_owned(), Style::new().fg(theme.accent)));
    }
    line
}

/// How many digits the largest row number of a list this long takes.
fn digits(count: usize) -> usize {
    count.max(1).to_string().len()
}

/// The sentence an empty list says about itself, dimmed.
fn draw_empty(frame: &mut Frame, area: Rect, message: &str, dim: ratatui::style::Color) {
    frame.render_widget(
        Paragraph::new(Line::styled(message.to_owned(), Style::new().fg(dim))),
        area,
    );
}

/// How many rows the now-playing block takes out of a content area this tall.
/// The list below always keeps rows of its own: the block carries the
/// spectrum only on a tall window, is seven rows when twelve are left over,
/// three on a short window, one on a tiny one.
const fn now_playing_rows(height: u16) -> u16 {
    match height {
        22.. => 7 + SPECTRUM_ROWS,
        18.. => 7,
        7.. => 3,
        _ => 1,
    }
}

/// How many columns the now-playing text keeps beside a cover; narrower
/// than this, the cover gives way.
const COVER_MIN_TEXT: u16 = 48;

/// Where the cover goes inside the now-playing `block`, and what is left for
/// the block's text: at the left, as tall as the block less its trailing gap
/// row, twice as wide as tall — terminal cells are about twice as tall as
/// wide, so that is close to square — with two columns of air after it.
/// `None` for a compact block, or when the text would be left too narrow.
fn cover_slot(block: Rect) -> Option<(Rect, Rect)> {
    if block.height < 7 {
        return None;
    }
    let height = block.height - 1;
    let width = height * 2;
    let text = block.width.checked_sub(width + 2)?;
    if text < COVER_MIN_TEXT {
        return None;
    }
    let art = Rect {
        width,
        height,
        ..block
    };
    let rest = Rect {
        x: block.x + width + 2,
        width: text,
        ..block
    };
    Some((art, rest))
}

/// How tall the spectrum is, in rows; each row resolves eight steps.
const SPECTRUM_ROWS: u16 = 4;

/// The redraw cadence: about thirty frames a second only while a spectrum is
/// on screen and moving — the music playing, or bars still falling after it
/// stopped. Hidden bars and still ones are drawn at ten. Low power (issue
/// #11) draws at a quarter of whatever the ordinary rate would be.
#[expect(
    clippy::fn_params_excessive_bools,
    reason = "the four facts the cadence reads are independent: visible bars, moving music, falling bars, low power"
)]
const fn cadence(visible: bool, playing: bool, falling: bool, low_power: bool) -> Duration {
    let base = if visible && (playing || falling) {
        33
    } else {
        100
    };
    Duration::from_millis(if low_power { base * 4 } else { base })
}

/// Partial blocks by eighths, empty to full.
pub(crate) const EIGHTHS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// The spectrum as `rows` lines, top first: one column per band and one of
/// air after it, each bar filling from the bottom in eighths of a row and
/// colored by height. A silent band keeps a baseline in the frame's color,
/// so the analyzer reads as there and quiet rather than missing.
/// The spectrum as `rows` lines, top first: one column per band and one of
/// air after it, each bar filling from the bottom in eighths of a row and
/// colored by height. A silent band keeps a baseline in the frame's color,
/// so the analyzer reads as there and quiet rather than missing — but full
/// silence draws nothing at all (issue #40): every visualizer renders
/// nothing when every band is quiet.
pub(crate) fn spectrum_lines(levels: &[f32], rows: u16, theme: &Theme) -> Vec<Line<'static>> {
    let rows = usize::from(rows);
    let steps = rows * 8;
    let quiet = levels.iter().all(|&level| level <= 0.0);
    (0..rows)
        .map(|row| {
            let from_bottom = rows - 1 - row;
            let color = bar_color(from_bottom, rows, theme);
            let spans: Vec<Span<'static>> = levels
                .iter()
                .flat_map(|&level| {
                    #[expect(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        clippy::cast_precision_loss,
                        reason = "a level is 0..=1 and a bar a few dozen steps"
                    )]
                    let filled = (level.clamp(0.0, 1.0) * steps as f32).round() as usize;
                    let here = filled.saturating_sub(from_bottom * 8).min(8);
                    let cell = if here == 0 && from_bottom == 0 && !quiet {
                        Span::styled("▁", Style::new().fg(theme.border))
                    } else {
                        Span::styled(EIGHTHS[here].to_string(), Style::new().fg(color))
                    };
                    [cell, Span::raw(" ")]
                })
                .collect();
            Line::from(spans)
        })
        .collect()
}

/// The bar color `from_bottom` rows up out of `rows`: the accent in the
/// lower half, its bright twin above — the two-step gradient the terminal's
/// own palette can give.
pub(crate) fn bar_color(from_bottom: usize, rows: usize, theme: &Theme) -> ratatui::style::Color {
    if from_bottom * 2 >= rows {
        theme.accent_bright
    } else {
        theme.accent
    }
}

/// One stored entry that names a file — a history entry or a favourite
/// (issue #7) — as one row: the title when the store knew one, with
/// `artist · album` dimmed after it, and the file's own name over its folder
/// when it did not. Nothing is invented for a path the store never described.
fn file_entry_text(
    path: &str,
    artist: Option<&str>,
    album: Option<&str>,
    title: Option<&str>,
    song_titles: &HashMap<String, String>,
) -> (String, String, String) {
    match title {
        Some(title) if !title.is_empty() => {
            let note = rowtext::note(&[artist.unwrap_or(""), album.unwrap_or("")]);
            (title.to_owned(), note, String::new())
        }
        _ => {
            let (name, folder) = describe(Path::new(path), [], song_titles);
            (name, folder, String::new())
        }
    }
}

/// The title and the dim subtitle of a playing file: its library row when
/// one of `known` has its path (`artist · album`), otherwise the title the
/// wrapper gave the song this session (`song_titles`, issue #34 — read only
/// here, so every list that shows the song by path names it alike), and
/// otherwise its file name over its folder. Nothing is invented for a file
/// the index never saw and no wrapper ever named.
fn describe<'a>(
    path: &Path,
    known: impl IntoIterator<Item = &'a TrackEntry>,
    song_titles: &HashMap<String, String>,
) -> (String, String) {
    if let Some(entry) = known.into_iter().find(|entry| entry.path == path) {
        return (
            entry.title.clone(),
            format!("{} · {}", entry.artist, entry.album),
        );
    }
    if let Some(title) = song_titles.get(path.to_string_lossy().as_ref()) {
        return (title.clone(), String::new());
    }
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().to_string(),
    );
    let folder = path
        .parent()
        .map(|parent| parent.display().to_string())
        .unwrap_or_default();
    (name, folder)
}

/// The width of the volume slider's track, in cells.
const VOLUME_CELLS: u16 = 12;

/// `VOL ━━━━━━●───── 90`: the filled part in the accent, the knob, the rest
/// in the border color, then the number. With no cells it is `VOL 90`.
fn volume_slider(volume: u8, cells: u16, theme: &Theme) -> Vec<Span<'static>> {
    let mut spans = vec![Span::styled("VOL ", Style::new().fg(theme.dim))];
    if cells > 0 {
        let cells = usize::from(cells);
        let knob = usize::from(volume.min(100)) * (cells - 1) / 100;
        spans.push(Span::styled(
            "━".repeat(knob),
            Style::new().fg(theme.accent),
        ));
        spans.push(Span::styled("●", Style::new().fg(theme.text)));
        spans.push(Span::styled(
            "─".repeat(cells - 1 - knob),
            Style::new().fg(theme.border),
        ));
        spans.push(Span::raw(" "));
    }
    // Three columns whatever the number, so a slider set against the right
    // edge does not jump when 100 becomes 95 (T7).
    spans.push(Span::styled(
        format!("{volume:>3}"),
        Style::new().fg(theme.text),
    ));
    spans
}

/// A bar `width` cells wide, filled to `ratio` in the accent; with no ratio
/// (nothing plays, or the length is unknown) the whole bar is unfilled.
fn progress_bar(ratio: Option<f64>, width: u16, theme: &Theme) -> Vec<Span<'static>> {
    let width = usize::from(width);
    // Rounded down, so a bar is never shown full before the track ends.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let filled = ratio
        .map_or(0, |ratio| (ratio * width as f64) as usize)
        .min(width);
    vec![
        Span::styled("━".repeat(filled), Style::new().fg(theme.accent)),
        Span::styled("─".repeat(width - filled), Style::new().fg(theme.border)),
    ]
}

/// One row with `left` against the left edge and `right` against the right;
/// the right side is measured first so a long left side is the one cut.
fn split_row(frame: &mut Frame, area: Rect, left: Line<'static>, right: Line<'static>) {
    let right_width = u16::try_from(right.width()).unwrap_or(area.width);
    let [left_area, right_area] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(right_width)]).areas(area);
    frame.render_widget(Paragraph::new(left), left_area);
    frame.render_widget(Paragraph::new(right), right_area);
}

/// Three one-row areas from the top of `area`.
fn rows3(area: Rect) -> [Rect; 3] {
    Layout::vertical([Constraint::Length(1); 3]).areas(area)
}

/// The name in the title bar, spaced out the way a wordmark is set.
const WORDMARK: &str = "V E L O ";

/// The area inside the window less one column of breathing room per side.
fn pad(area: Rect) -> Rect {
    Rect {
        x: area.x.saturating_add(1),
        width: area.width.saturating_sub(2),
        ..area
    }
}

/// The smallest rectangle that covers two areas side by side or one above
/// the other: the header row and the body, for the radio view that draws
/// across both (issue #50).
fn union(a: Rect, b: Rect) -> Rect {
    let right = (a.x + a.width).max(b.x + b.width);
    let bottom = (a.y + a.height).max(b.y + b.height);
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    Rect {
        x,
        y,
        width: right - x,
        height: bottom - y,
    }
}

/// A position typed at the seek prompt (issue #6). A share of the track needs
/// its length to become a position, which only the caller knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeekPosition {
    /// A percentage of the track, `0..=100`.
    Percent(u8),
    /// A position from the start of the track, in seconds.
    Secs(u64),
}

/// Reads what the seek prompt was given (issue #6): `70%`, `1:20`, `1:20:30`,
/// or a bare count of seconds. The refusal says what was expected instead of
/// leaving the card with nothing to show.
///
/// # Errors
///
/// Refuses anything else, saying which of the accepted forms was expected:
/// an empty answer, a percentage outside `0..=100`, a clock field past 59,
/// more than three clock fields, or text that is none of the three forms.
pub fn parse_seek(input: &str) -> Result<SeekPosition, &'static str> {
    const FORMS: &str = "type a position: 70%, 1:20 or 90";

    let typed = input.trim();
    if typed.is_empty() {
        return Err(FORMS);
    }
    if let Some(head) = typed.strip_suffix('%') {
        let percent: u8 = head
            .trim()
            .parse()
            .map_err(|_| "a percentage is a whole number, 0 to 100")?;
        return if percent <= 100 {
            Ok(SeekPosition::Percent(percent))
        } else {
            Err("a percentage is a whole number, 0 to 100")
        };
    }
    if typed.contains(':') {
        const SHAPE: &str = "a clock position looks like 1:20 or 1:20:30";
        let numbers = typed
            .split(':')
            .map(|part| part.trim().parse::<u64>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| SHAPE)?;
        let (hours, minutes, seconds) = match numbers.as_slice() {
            [minutes, seconds] => (0, *minutes, *seconds),
            [hours, minutes, seconds] => (*hours, *minutes, *seconds),
            _ => return Err(SHAPE),
        };
        if minutes > 59 || seconds > 59 {
            return Err("the minutes and the seconds of a clock position stop at 59");
        }
        return hours
            .checked_mul(3600)
            .and_then(|total| total.checked_add(minutes * 60))
            .and_then(|total| total.checked_add(seconds))
            .map(SeekPosition::Secs)
            .ok_or("that position is past the end of any track");
    }
    typed.parse().map(SeekPosition::Secs).map_err(|_| FORMS)
}

fn mmss(duration: Duration) -> String {
    let seconds = duration.as_secs();
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

/// Every row of the help card: the key, then what it does.
const HELP_ROWS: [(&str, &str); 46] = [
    ("j / Down", "move down"),
    ("k / Up", "move up"),
    ("g / home", "first track"),
    ("G / end", "last track"),
    ("pgdn / pgup", "a page at a time; ctrl+d / ctrl+u too"),
    ("enter", "open, or play from the selection"),
    ("esc", "back one level / close help"),
    (
        "/",
        "search the whole library; in radio: the directory, by name",
    ),
    ("space", "pause / resume"),
    ("← / →", "seek 5 s back / forward; shift: 30 s"),
    (": / ctrl+j", "jump to a position: 70%, 1:20 or 90"),
    ("> / . / n", "next track"),
    ("< / ,", "previous track"),
    ("z", "shuffle what is left of the queue"),
    ("r", "repeat: off, all, one"),
    ("m", "mono: both channels carry the average of the mix"),
    ("+ / -", "volume up / down; [ / ] a point at a time"),
    ("? / ctrl+k", "toggle this help"),
    ("ctrl+p", "settings"),
    ("tab", "library / queue"),
    ("p", "the queue, like tab going there"),
    ("1 2 3 4", "artists, albums, tracks, genres"),
    ("o / ctrl+l", "set the library folder"),
    ("L", "the saved playlists"),
    ("I", "refresh the library index"),
    (
        "u",
        "open a URL: a live stream, or the songs its wrapper lists",
    ),
    (
        "",
        "no library set: a prompt asks for its path, tab completes",
    ),
    ("a", "add the selection to the queue"),
    ("A", "add the selection to the queue and play from it"),
    ("f", "on a track: mark it as a favourite"),
    (
        "S",
        "save the queue as a playlist; on a track: add it to one",
    ),
    ("x", "in the queue: remove it; on a playlist: delete it"),
    ("e", "in playlists: rename the one under the cursor"),
    ("J / K", "move it down / up; shift+↓ / shift+↑ too"),
    ("R", "radio: the directory, your favourites, your stations"),
    ("v", "next visualizer"),
    ("ctrl+v", "pick a visualizer"),
    ("V", "the visualizer over the whole window"),
    ("f", "in radio: favourite a station, pin a place"),
    ("F", "in radio: filter what is already loaded"),
    ("N", "in radio: the country list"),
    ("ctrl+r", "in radio: refetch the place you are in"),
    ("i", "in radio: report a station that cannot play"),
    (
        "ctrl+s",
        "save a remote track out of the cache, to a folder you name",
    ),
    // Kept at the end so the rows the first screenful pins stay put: the
    // tests read `I` and `q` on a small window, where a row added above
    // them would silently scroll them off.
    ("P", "low power: a quarter of the redraw rate, no spectrum"),
    ("q / ctrl+c", "quit"),
];

/// The open help card (S1): which row is at its top and what it is
/// filtered by. Nothing here touches the filesystem.
#[derive(Debug, Default)]
struct Help {
    /// The first row in view.
    top: usize,
    /// The text typed after `/`; a row stays when its key or what it does
    /// contains it, ignoring case.
    filter: String,
    /// Whether printable keys type into the filter (between `/` and `Enter`).
    typing: bool,
}

impl Help {
    /// The card opens on the rows that belong to the screen it is over
    /// (issue #31): with the radio view showing, it starts at the radio's own
    /// keys instead of making the person scroll past the library's to find
    /// them. Every other screen starts at the top, and a window too short for
    /// the jump keeps the card scrolled to its last page, exactly what it
    /// would have drawn anyway.
    fn for_radio(radio: bool, area: Rect) -> Self {
        let mut help = Self::default();
        if radio {
            help.top = Self::first_radio_row().min(help.last_top(area));
        }
        help
    }

    /// Where the radio's keys begin, found by their own label: a row added
    /// above them cannot silently move where the card starts.
    fn first_radio_row() -> usize {
        HELP_ROWS
            .iter()
            .position(|(_, label)| label.starts_with("in radio:"))
            .unwrap_or(0)
    }

    /// The rows the filter keeps, in their order.
    fn rows(&self) -> Vec<(&'static str, &'static str)> {
        let needle = self.filter.to_lowercase();
        HELP_ROWS
            .iter()
            .filter(|(key, label)| {
                key.to_lowercase().contains(&needle) || label.to_lowercase().contains(&needle)
            })
            .copied()
            .collect()
    }

    /// Whether a filter is being typed or kept; `Esc` clears it first.
    fn filtering(&self) -> bool {
        self.typing || !self.filter.is_empty()
    }

    /// The lines above the rows: the filter's, while there is one.
    fn header_lines(&self) -> usize {
        usize::from(self.filtering())
    }

    /// The body rows the card keeps whatever the filter keeps: every key, or
    /// as many as fit in `area`. Read before the filter, so that a filter
    /// leaving a single row leaves the card just as tall.
    fn card_rows(area: Rect) -> usize {
        if area.height == 0 {
            return HELP_ROWS.len();
        }
        let room = usize::from(area.height).saturating_sub(2);
        HELP_ROWS.len().min(room)
    }

    /// How many of the rows the filter kept fit on the card.
    fn shown_rows(&self, area: Rect) -> usize {
        let room = Self::card_rows(area).saturating_sub(self.header_lines());
        self.rows().len().min(room)
    }

    /// The row at the top when the last row is at the bottom.
    fn last_top(&self, area: Rect) -> usize {
        self.rows().len() - self.shown_rows(area)
    }
}

/// One setting the card can change (S7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsKey {
    ReplayGain,
    Repeat,
    Shuffle,
    Theme,
    TransparentBackground,
    CoverArt,
    Spectrum,
    Hints,
    TopBar,
    Advisories,
    ViewNumbers,
    Folder,
    Version,
}

impl SettingsKey {
    /// The setting's label on the card's left.
    fn label(self) -> &'static str {
        match self {
            Self::ReplayGain => "replay gain",
            Self::Repeat => "repeat",
            Self::Shuffle => "shuffle",
            Self::Theme => "theme",
            Self::TransparentBackground => "transparent background",
            Self::CoverArt => "cover art",
            Self::Spectrum => "spectrum",
            Self::Hints => "hints",
            Self::TopBar => "top bar",
            Self::Advisories => "advisories",
            Self::ViewNumbers => "view numbers",
            Self::Folder => "folder",
            Self::Version => "version",
        }
    }
}

/// One row of the settings card: a section header, or a setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsRow {
    Header(&'static str),
    Setting(SettingsKey),
}

/// The card's rows, in order (S7): the headers are never focused. The
/// colours are not rows (issue #37): they are the theme's inside, and the
/// theme is chosen by name in the picker; the `[theme]` table keeps every
/// colour editable in the file. The volume is not a row (S7.3): it lives
/// in `[state]` and on `+`/`-`, where it has always been changed from the
/// keyboard.
const SETTINGS_ROWS: [SettingsRow; 18] = [
    SettingsRow::Header("sound"),
    SettingsRow::Setting(SettingsKey::ReplayGain),
    SettingsRow::Setting(SettingsKey::Repeat),
    SettingsRow::Setting(SettingsKey::Shuffle),
    SettingsRow::Header("theme"),
    SettingsRow::Setting(SettingsKey::Theme),
    // The transparent-background switch (issue #61): directly below the
    // theme row, a switch like `shuffle` — never a picker of its own.
    SettingsRow::Setting(SettingsKey::TransparentBackground),
    SettingsRow::Header("screen"),
    SettingsRow::Setting(SettingsKey::CoverArt),
    SettingsRow::Setting(SettingsKey::Spectrum),
    SettingsRow::Setting(SettingsKey::Hints),
    SettingsRow::Setting(SettingsKey::TopBar),
    SettingsRow::Setting(SettingsKey::Advisories),
    SettingsRow::Setting(SettingsKey::ViewNumbers),
    SettingsRow::Header("library"),
    SettingsRow::Setting(SettingsKey::Folder),
    // The version row (issue #69): the running version, and whether a
    // newer one exists — a read-only row, like the headers.
    SettingsRow::Header("about"),
    SettingsRow::Setting(SettingsKey::Version),
];

/// The open visualizer picker (issue #40): the row the cursor is on, and
/// the visualizer in force when it opened, which `esc` restores exactly.
#[derive(Debug, Clone, Copy)]
struct VisualizerPicker {
    /// The focused visualizer, as its index in [`Visualizer::ALL`].
    cursor: usize,
    /// The visualizer the session drew with when the picker opened: what
    /// `esc` puts back.
    snapshot: Visualizer,
}

/// The open device picker (issue #44, S1): the machine's outputs as the
/// lister returned them when it opened, and the one in use, marked in the
/// drawing. There is no preview — a device is heard, not seen — so a move
/// touches nothing; `enter` switches, `esc` closes without changing
/// anything.
struct DevicePicker {
    /// The offered names, frozen at open: the machine's outputs, in the
    /// order the lister gave them.
    names: Vec<String>,
    /// The device in use when the picker opened: the row the mark sits on.
    in_use: String,
    /// The focused row.
    cursor: usize,
}

/// The open theme picker (issue #37): the row the cursor is on, and the
/// palette in force when it opened, which `esc` restores exactly.
#[derive(Debug, Clone, Copy)]
struct ThemePicker {
    /// The focused theme, as its index in [`App::theme_files`].
    cursor: usize,
    /// The palette the screen drew with when the picker opened: what `esc`
    /// puts back, `[theme]` overrides included.
    snapshot: ThemeColors,
}

/// The open settings card (S6): which setting the cursor is on, as its index
/// among the focusable rows, and the first row of the table in view. The
/// card's size comes from the whole table, so it never changes as the cursor
/// moves. Nothing here touches the filesystem.
#[derive(Debug, Default)]
struct SettingsCard {
    /// The focused setting, counted over the settings only.
    cursor: usize,
    /// The first row of the table in view.
    top: usize,
}

impl SettingsCard {
    /// The settings the cursor moves between, as their table rows.
    fn settings() -> impl Iterator<Item = usize> {
        SETTINGS_ROWS
            .iter()
            .enumerate()
            .filter_map(|(row, entry)| match entry {
                SettingsRow::Setting(_) => Some(row),
                SettingsRow::Header(_) => None,
            })
    }

    /// How many settings the card has.
    fn setting_count() -> usize {
        Self::settings().count()
    }

    /// The table row the cursor-th setting sits on.
    fn row_of(cursor: usize) -> usize {
        Self::settings().nth(cursor).unwrap_or(0)
    }

    fn move_down(&mut self) {
        self.cursor = (self.cursor + 1).min(Self::setting_count() - 1);
    }

    fn move_up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// The focused setting, counted over the settings only.
    fn focused(&self) -> SettingsKey {
        SETTINGS_ROWS
            .get(Self::row_of(self.cursor))
            .copied()
            .map_or(SettingsKey::ReplayGain, |row| match row {
                SettingsRow::Setting(key) => key,
                SettingsRow::Header(_) => SettingsKey::ReplayGain,
            })
    }

    /// What the card focuses, as the row's label.
    fn focused_label(&self) -> &'static str {
        self.focused().label()
    }

    /// Slides `top` so the focused row stays in view of `shown` rows.
    fn keep_visible(&mut self, shown: usize) {
        let row = Self::row_of(self.cursor);
        if row < self.top {
            self.top = row;
        } else if row >= self.top + shown {
            self.top = row + 1 - shown;
        }
    }

    /// The body rows the card keeps: every row, or as many as fit. The whole
    /// table decides, so the card never resizes as the cursor moves.
    fn card_rows(area: Rect) -> usize {
        if area.height == 0 {
            return SETTINGS_ROWS.len();
        }
        let room = usize::from(area.height).saturating_sub(2);
        SETTINGS_ROWS.len().min(room)
    }

    /// The last value `top` may take with `shown` rows on the card.
    const fn last_top(shown: usize) -> usize {
        SETTINGS_ROWS.len().saturating_sub(shown)
    }
}

/// The `ReplayGain` mode in the config's own words.
fn replay_gain_label(mode: ReplayGainMode) -> &'static str {
    match mode {
        ReplayGainMode::Track => "track",
        ReplayGainMode::Album => "album",
        ReplayGainMode::Off => "off",
    }
}

/// A `width` × `height` rectangle centred in `area`, clamped to fit.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// The open info card (issue #43): the rows it shows, gathered once at the
/// key press. Nothing here touches the filesystem, and every key but its
/// own `esc` waits while it is up.
#[derive(Debug, Default)]
struct InfoCard {
    /// The card's body, `field` then its value — in the order the brief
    /// walks them: the file's own facts, then what the index knew.
    rows: Vec<(&'static str, String)>,
}

impl InfoCard {
    /// The card one entry describes (issue #43), resolved now: the path or
    /// the remote URI verbatim, the header facts read without decoding,
    /// the `ReplayGain` tags the file carries and what the index knew. A
    /// file that cannot be read answers with its reason inside the card,
    /// and a station — which lives in no file — with its URI alone.
    fn for_entry(entry: &Entry, index: Option<&TrackEntry>) -> Self {
        let head = entry.as_path().map(read_head_facts);
        Self {
            rows: info_rows(entry, index, head),
        }
    }
}

/// What a file's header says about one track (issue #43), read before the
/// card composes its rows: the format header and the tags, never a decode
/// of the samples — that is `--info`'s report, not this one's.
#[derive(Debug, Default)]
struct HeadFacts {
    /// The container lofty recognized the file by, as the card prints it.
    container: Option<String>,
    /// The codec the container declared, through [`TrackSource::codec_name`].
    codec: Option<&'static str>,
    /// The audio bitrate, in kilobits per second, when the header states it.
    bitrate_kbps: Option<u32>,
    /// The sample rate, in hertz.
    sample_rate: Option<u32>,
    /// The channel count.
    channels: Option<u16>,
    /// The duration the container declares.
    duration: Option<Duration>,
    /// The `ReplayGain` tags the file carries, when it has them.
    loudness: Option<Loudness>,
}

/// Reads the header facts one card shows (issue #43): [`TrackSource::open`]
/// for the format header — the one block it decodes, for a container that
/// hides its format, is what playback itself pays — and one read through
/// lofty for what the header does not name: the container's own kind, the
/// bitrate and the `ReplayGain` tags, parsed by the same [`parse_db`] and
/// [`parse_peak`] the gain path applies. A file lofty cannot parse keeps
/// its header rows; `Err` carries the reason the file cannot be read at all.
fn read_head_facts(path: &Path) -> Result<HeadFacts, String> {
    let source = TrackSource::open(path).map_err(|error| error.to_string())?;
    let spec = source.spec();
    let mut head = HeadFacts {
        codec: source.codec_name(),
        sample_rate: Some(spec.sample_rate),
        channels: Some(spec.channels),
        duration: source.duration(),
        ..HeadFacts::default()
    };
    if let Ok(tagged) = Probe::open(path).and_then(Probe::read) {
        head.container = container_name(tagged.file_type()).map(str::to_owned);
        head.bitrate_kbps = tagged.properties().audio_bitrate();
        let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
        head.loudness = Some(Loudness {
            track_gain_db: tag
                .and_then(|tag| tag.get_string(ItemKey::ReplayGainTrackGain))
                .and_then(parse_db),
            track_peak: tag
                .and_then(|tag| tag.get_string(ItemKey::ReplayGainTrackPeak))
                .and_then(parse_peak),
            album_gain_db: tag
                .and_then(|tag| tag.get_string(ItemKey::ReplayGainAlbumGain))
                .and_then(parse_db),
            album_peak: tag
                .and_then(|tag| tag.get_string(ItemKey::ReplayGainAlbumPeak))
                .and_then(parse_peak),
        });
    }
    Ok(head)
}

/// The container a [`FileType`] names, as the card prints it (issue #43).
/// The type is `#[non_exhaustive]`, so an unmatched kind is not named: the
/// row is left out, never a wrong name.
fn container_name(file_type: FileType) -> Option<&'static str> {
    match file_type {
        FileType::Aac => Some("aac"),
        FileType::Aiff => Some("aiff"),
        FileType::Ape => Some("ape"),
        FileType::Flac => Some("flac"),
        FileType::Mpeg => Some("mpeg"),
        FileType::Mp4 => Some("mp4"),
        FileType::Mpc => Some("mpc"),
        FileType::Opus => Some("opus"),
        FileType::Speex => Some("speex"),
        FileType::Vorbis => Some("ogg"),
        FileType::Wav => Some("wav"),
        FileType::WavPack => Some("wavpack"),
        _ => None,
    }
}

/// The rows one info card shows (issue #43), pure so the rules are testable
/// headlessly: a field whose value is not known is left out, never a label
/// with nothing after it, and a file that cannot be read shows its reason
/// in place of its header facts. `head` is `None` for a station, whose only
/// fact is the URI it streams from.
fn info_rows(
    what: &Entry,
    index: Option<&TrackEntry>,
    head: Option<Result<HeadFacts, String>>,
) -> Vec<(&'static str, String)> {
    let mut rows = vec![(
        "path",
        match what {
            Entry::File(path) => path.to_string_lossy().into_owned(),
            Entry::Station { url, .. } => url.clone(),
        },
    )];
    let loudness = match head {
        Some(Ok(head)) => {
            if let Some(container) = head.container {
                rows.push(("container", container));
            }
            if let Some(codec) = head.codec {
                rows.push(("codec", codec.to_owned()));
            }
            if let Some(bitrate) = head.bitrate_kbps {
                rows.push(("bitrate", format!("{bitrate} kbps")));
            }
            if let Some(rate) = head.sample_rate {
                rows.push(("rate", format!("{rate} Hz")));
            }
            if let Some(channels) = head.channels {
                rows.push(("channels", channels.to_string()));
            }
            if let Some(duration) = head.duration {
                rows.push(("duration", mmss(duration)));
            }
            head.loudness
        }
        Some(Err(reason)) => {
            rows.push(("error", reason));
            None
        }
        // A station lives nowhere on disk: its URI is the whole truth.
        None => return rows,
    };
    if let Some(entry) = index {
        rows.push(("artist", entry.artist.clone()));
        rows.push(("album", entry.album.clone()));
        rows.push(("title", entry.title.clone()));
        if let Some(track_no) = entry.track_no {
            rows.push(("track", track_no.to_string()));
        }
    }
    if let Some(loudness) = loudness {
        if let Some(gain) = loudness.track_gain_db {
            rows.push(("track gain", format!("{gain:+.2} dB")));
        }
        if let Some(peak) = loudness.track_peak {
            rows.push(("track peak", format!("{peak:.6}")));
        }
        if let Some(gain) = loudness.album_gain_db {
            rows.push(("album gain", format!("{gain:+.2} dB")));
        }
        if let Some(peak) = loudness.album_peak {
            rows.push(("album peak", format!("{peak:.6}")));
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::App;
    use super::HeadFacts;
    use super::info_rows;
    use crate::audio::output::Entry;
    use std::path::PathBuf;
    use std::time::Duration;

    // The playing-entry refusals of T18b cannot be driven through `App::on_key`
    // without a sound device: marking an entry as playing means opening one.
    // The rule is pure and lives in one place that `remove_from_queue` and
    // `move_in_queue` both read, so it is pinned here, and the wiring from
    // `on_key` into `App::status` is pinned headlessly in `tests/ui_state.rs`
    // through the empty-queue refusal, which walks the same code path.

    // ---- the info card (issue #43): the pure rules of its rows ----------
    //
    // The card's key wiring is pinned headlessly in `tests/ui_info.rs`
    // through a real fixture file; these pin the rules of what the rows
    // themselves may say.

    fn info_entry() -> crate::library::TrackEntry {
        crate::library::TrackEntry {
            artist: "Tide".to_owned(),
            album: "Harbor".to_owned(),
            title: "Dawn".to_owned(),
            track_no: Some(1),
            disc_no: None,
            duration_ms: 1000,
            path: PathBuf::from("/music/Tide/01.flac"),
            source: crate::library::LOCAL_SOURCE.to_owned(),
            remote_id: None,
            cover_id: None,
        }
    }

    /// A field the source does not know is left out, never an empty line
    /// with a label and nothing after it (issue #43).
    #[test]
    fn a_field_the_source_does_not_know_is_left_out_of_the_card() {
        let mut entry = info_entry();
        entry.track_no = None;
        let known = info_rows(
            &Entry::from(PathBuf::from("/music/Tide/01.flac")),
            Some(&entry),
            Some(Ok(HeadFacts::default())),
        );

        assert_eq!(
            known.first().map(|(field, value)| (*field, value.as_str())),
            Some(("path", "/music/Tide/01.flac"))
        );
        assert!(
            !known.iter().any(|(field, _)| *field == "track"),
            "no track number in the index, no track row"
        );
        assert!(
            known.iter().all(|(_field, value)| !value.trim().is_empty()),
            "a row is never a label with nothing after it: {known:?}"
        );
        assert!(
            !known.iter().any(|(field, _)| *field == "container"),
            "an unreadable header leaves its facts out too"
        );
    }

    /// What the index never saw adds no rows: a file outside the index is
    /// described by its header alone (issue #43).
    #[test]
    fn a_file_outside_the_index_names_no_tags() {
        let rows = info_rows(
            &Entry::from(PathBuf::from("/elsewhere/two.flac")),
            None,
            Some(Ok(HeadFacts::default())),
        );
        assert!(
            !rows
                .iter()
                .any(|(field, _)| matches!(*field, "artist" | "album" | "title" | "track"))
        );
    }

    /// A file that cannot be read shows its reason in the card instead of
    /// the header rows (issue #43), and the key press does not fail.
    #[test]
    fn an_unreadable_file_shows_its_reason_in_the_card() {
        let rows = info_rows(
            &Entry::from(PathBuf::from("/gone/track.flac")),
            None,
            Some(Err("cannot read the file: permission denied".to_owned())),
        );
        assert_eq!(rows.first().map(|(field, _)| *field), Some("path"));
        let reason = rows.iter().find(|(field, _)| *field == "error");
        assert_eq!(
            reason.map(|(_field, value)| value.as_str()),
            Some("cannot read the file: permission denied")
        );
        assert!(!rows.iter().any(|(field, _)| *field == "codec"));
    }

    /// A station is described by its URI alone — it lives in no file's
    /// header (issue #43).
    #[test]
    fn a_station_is_described_by_its_uri_alone() {
        let rows = info_rows(
            &Entry::Station {
                url: "https://radio.example/stream".to_owned(),
                name: "Radio Example".to_owned(),
            },
            None,
            None,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0], ("path", "https://radio.example/stream".to_owned()));
    }

    /// The card keeps every known fact: the header rows and the index rows
    /// in place, with the duration as the lists write durations.
    #[test]
    fn a_fully_known_track_is_described_in_full() {
        let head = HeadFacts {
            container: Some("flac".to_owned()),
            codec: Some("flac"),
            bitrate_kbps: Some(962),
            sample_rate: Some(44_100),
            channels: Some(2),
            duration: Some(Duration::from_secs(198)),
            loudness: None,
        };
        let rows = info_rows(
            &Entry::from(PathBuf::from("/music/Tide/01.flac")),
            Some(&info_entry()),
            Some(Ok(head)),
        );

        let values: std::collections::HashMap<&str, &str> = rows
            .iter()
            .map(|(field, value)| (*field, value.as_str()))
            .collect();
        assert_eq!(values.get("container"), Some(&"flac"));
        assert_eq!(values.get("codec"), Some(&"flac"));
        assert_eq!(values.get("bitrate"), Some(&"962 kbps"));
        assert_eq!(values.get("rate"), Some(&"44100 Hz"));
        assert_eq!(values.get("channels"), Some(&"2"));
        assert_eq!(values.get("duration"), Some(&"03:18"));
        assert_eq!(values.get("artist"), Some(&"Tide"));
        assert_eq!(values.get("track"), Some(&"1"));

        // The order the brief walks: the file's own facts first, then what
        // the index knew.
        let order: Vec<&str> = rows.iter().map(|(field, _)| *field).collect();
        let files = [
            "container",
            "codec",
            "bitrate",
            "rate",
            "channels",
            "duration",
        ];
        let head_last = files
            .iter()
            .filter_map(|field| order.iter().position(|row_field| row_field == field))
            .max()
            .expect("header rows present");
        let index_first = ["artist", "album", "title", "track"]
            .iter()
            .filter_map(|field| order.iter().position(|row_field| row_field == field))
            .min()
            .expect("index rows present");
        assert!(head_last < index_first);
    }

    #[test]
    fn a_playing_file_is_named_by_its_library_row_or_else_by_its_file() {
        use std::path::{Path, PathBuf};

        let known = [crate::library::TrackEntry {
            artist: "Tide".to_owned(),
            album: "Harbor".to_owned(),
            title: "Dawn".to_owned(),
            track_no: Some(1),
            disc_no: None,
            duration_ms: 1000,
            path: PathBuf::from("/music/Tide/01.flac"),
            source: crate::library::LOCAL_SOURCE.to_owned(),
            remote_id: None,
            cover_id: None,
        }];

        assert_eq!(
            super::describe(
                Path::new("/music/Tide/01.flac"),
                &known,
                &std::collections::HashMap::new()
            ),
            ("Dawn".to_owned(), "Tide · Harbor".to_owned())
        );
        assert_eq!(
            super::describe(
                Path::new("/elsewhere/two.flac"),
                &known,
                &std::collections::HashMap::new()
            ),
            ("two.flac".to_owned(), "/elsewhere".to_owned()),
            "a file the index never saw keeps its own name, nothing invented"
        );
    }

    #[test]
    fn the_progress_bar_is_never_full_before_the_end_and_empty_without_a_length() {
        let filled = |ratio, width| {
            super::progress_bar(ratio, width, &super::Theme::default())[0]
                .content
                .chars()
                .count()
        };
        assert_eq!(filled(Some(0.999), 10), 9);
        assert_eq!(filled(Some(1.0), 10), 10);
        assert_eq!(filled(Some(0.5), 10), 5);
        assert_eq!(filled(None, 10), 0);
    }

    // The station-play wiring of cliamp S1–S3, S5, S6, with the URL
    // resolution injected: a real one would speak to the network, which no
    // test does.

    use crate::radio::stream::{Expansion, Song, StreamError};

    /// The queue's paths as strings, asserting on the way: every entry a
    /// song opens as must be a file row, never a station.
    fn queue_paths(app: &App<'_>) -> Vec<String> {
        app.queue
            .iter()
            .map(|entry| match entry {
                Entry::File(path) => path.to_string_lossy().into_owned(),
                Entry::Station { .. } => {
                    panic!("a song opens as a track, never as a station row: {entry:?}")
                }
            })
            .collect()
    }

    #[test]
    fn a_channel_of_songs_opens_as_one_file_row_per_song_in_wrapper_order() {
        let mut app = App::new(Vec::new());
        let songs = vec![
            Song {
                url: "https://radio.example/lofi/tracks/a".to_owned(),
                title: Some(String::from("Alice - First")),
            },
            Song {
                url: "https://radio.example/lofi/tracks/b".to_owned(),
                title: Some(String::from("Bob - Second")),
            },
            Song {
                url: "https://radio.example/lofi/tracks/c".to_owned(),
                title: None,
            },
        ];
        let action = app.play_station(
            "https://radio.example/lofi/tracks.m3u".to_owned(),
            "Lofi".to_owned(),
            Ok(Expansion::Songs(songs.clone())),
        );
        assert_eq!(action, super::Action::PlayQueue { from: 0 });
        // Identity is the URL (S3): the row's path is the song's `https://…`
        // URL, never a cache path — the cache resolves it when it plays.
        let urls: Vec<String> = songs.into_iter().map(|song| song.url).collect();
        assert_eq!(queue_paths(&app), urls, "one row per song, in order");
    }

    #[test]
    fn a_wrapper_that_cannot_expand_is_one_refusal_line_and_nothing_queued() {
        let mut app = App::new(Vec::new());
        let action = app.play_station(
            "https://radio.example/lofi/tracks.m3u".to_owned(),
            "Lofi".to_owned(),
            Err(StreamError::NotAudio {
                detail: "the wrapper playlist carries no song entry".to_owned(),
            }),
        );
        assert_eq!(action, super::Action::None);
        assert!(app.queue.is_empty(), "nothing was queued");
        assert_eq!(
            app.status,
            "velo: Lofi: the station body is not audio: the wrapper playlist \
             carries no song entry",
            "one line naming the station and the reason (S5)"
        );
    }

    #[test]
    fn a_station_that_resolves_to_live_plays_live_exactly_as_before() {
        let mut app = App::new(Vec::new());
        let action = app.play_station(
            "https://stream.example/nova".to_owned(),
            "Nova".to_owned(),
            Ok(Expansion::Live),
        );
        assert_eq!(action, super::Action::PlayQueue { from: 0 });
        assert_eq!(
            app.queue,
            vec![Entry::Station {
                url: "https://stream.example/nova".to_owned(),
                name: "Nova".to_owned(),
            }],
            "the live path is untouched (S6)"
        );
    }

    // The wrapper's titles reach the rows (issue #34): the expansion carries
    // each song's title beside its URL, the map is filled where the expansion
    // happens, and the expansion verdict is remembered for the session.

    #[test]
    fn a_queue_row_reads_the_wrappers_title_and_falls_back_to_the_url() {
        let mut app = App::new(Vec::new());
        let action = app.play_station(
            "https://radio.example/lofi/tracks.m3u".to_owned(),
            "Lofi".to_owned(),
            Ok(Expansion::Songs(vec![
                Song {
                    url: "https://radio.example/lofi/tracks/a".to_owned(),
                    title: Some(String::from("Alice - First")),
                },
                Song {
                    url: "https://radio.example/lofi/tracks/b".to_owned(),
                    title: None,
                },
            ])),
        );
        assert_eq!(action, super::Action::PlayQueue { from: 0 });

        // The queue view rendered as the user sees it: the titled song is
        // named by its title, the untitled one by its URL.
        app.view = super::View::Queue;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol().to_owned())
            .collect();
        assert!(
            screen.contains("Alice - First"),
            "the title the wrapper gave names the row"
        );
        assert!(
            !screen.contains("https://radio.example/lofi/tracks/a"),
            "the URL is not shown when the title is known"
        );
        assert!(
            screen.contains("https://radio.example/lofi/tracks"),
            "a song whose title is unknown keeps today's fallback, the URL"
        );
    }

    #[test]
    fn the_now_playing_block_shows_the_same_title() {
        use std::time::Duration;

        let mut app = App::new(Vec::new());
        let url = "https://radio.example/lofi/tracks/a".to_owned();
        app.play_station(
            "https://radio.example/lofi/tracks.m3u".to_owned(),
            "Lofi".to_owned(),
            Ok(Expansion::Songs(vec![Song {
                url: url.clone(),
                title: Some(String::from("Alice - First")),
            }])),
        );
        app.show_playing_for_tests(
            Entry::File(std::path::PathBuf::from(&url)),
            Duration::ZERO,
            None,
            None,
        );

        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let title: String = (0..120)
            .map(|x| buffer[(x, 2)].symbol().to_owned())
            .collect();
        assert!(
            title.contains("Alice - First"),
            "the block names the song by the wrapper's title: {title:?}"
        );
        assert!(
            !title.contains("radio.example"),
            "the URL is not shown when the title is known: {title:?}"
        );
    }

    /// A loopback server for the expansion-cache tests, on the pattern of
    /// `tests/radio_stream.rs`: one thread, canned routes, every request's
    /// path recorded, unmatched paths answered 404. No test touches the
    /// real network; the thread stops when the server drops.
    mod cache_server {
        use std::collections::HashMap;
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};

        pub struct Server {
            base: String,
            requests: Arc<Mutex<Vec<String>>>,
            stop: Arc<AtomicBool>,
        }

        impl Server {
            pub fn start(routes: Vec<(&'static str, Vec<u8>)>) -> Server {
                let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
                let base = format!("http://{}", listener.local_addr().expect("a local address"));
                let routes: HashMap<String, Vec<u8>> = routes
                    .into_iter()
                    .map(|(path, body)| (path.to_owned(), body))
                    .collect();
                let requests = Arc::new(Mutex::new(Vec::new()));
                let stop = Arc::new(AtomicBool::new(false));
                let seen = Arc::clone(&requests);
                let done = Arc::clone(&stop);
                std::thread::spawn(move || {
                    listener
                        .set_nonblocking(true)
                        .expect("the listener goes nonblocking");
                    while !done.load(Ordering::Relaxed) {
                        let Ok((mut connection, _)) = listener.accept() else {
                            std::thread::sleep(std::time::Duration::from_millis(10));
                            continue;
                        };
                        connection
                            .set_nonblocking(false)
                            .expect("the connection blocks");
                        let mut request = Vec::with_capacity(4096);
                        let mut chunk = [0_u8; 1024];
                        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                            let read = connection.read(&mut chunk).unwrap_or(0);
                            if read == 0 || request.len() > 4096 {
                                break;
                            }
                            request.extend_from_slice(&chunk[..read]);
                        }
                        let request = String::from_utf8_lossy(&request).to_string();
                        let path = request
                            .lines()
                            .next()
                            .and_then(|line| line.split_whitespace().nth(1))
                            .unwrap_or_default()
                            .to_string();
                        let body = routes.get(&path);
                        seen.lock()
                            .expect("the request log is uncontended")
                            .push(path);
                        if let Some(body) = body {
                            let head = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: audio/x-mpegurl\r\n\
                                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                            let _ = connection.write_all(head.as_bytes());
                            let _ = connection.write_all(body);
                        } else {
                            let _ = connection.write_all(
                                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\
                                  Connection: close\r\n\r\n",
                            );
                        }
                    }
                });
                Server {
                    base,
                    requests,
                    stop,
                }
            }

            pub fn url(&self, path: &str) -> String {
                format!("{}{path}", self.base)
            }

            pub fn requests(&self) -> Vec<String> {
                self.requests
                    .lock()
                    .expect("the request log is uncontended")
                    .clone()
            }
        }

        impl Drop for Server {
            fn drop(&mut self) {
                self.stop.store(true, Ordering::Relaxed);
            }
        }
    }

    /// An app with the radio open and one favourite — the configured row
    /// whose `enter` the shell resolves (cliamp S1) — pointing at `url`.
    fn app_with_favourite(url: String) -> App<'static> {
        let mut app = App::new(Vec::new());
        let mut store = crate::radio::store::RadioStore::default();
        store.favorite.push(crate::radio::store::Favorite {
            name: String::from("Lofi"),
            url,
            countrycode: String::new(),
            codec: String::new(),
        });
        app.radio = Some(super::radio::RadioView::new(
            store,
            Vec::new(),
            Vec::new(),
            None,
        ));
        app
    }

    #[test]
    fn the_second_open_of_a_configured_station_fetches_nothing_new() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent};

        let body = b"#EXTM3U\r\n\
                      #EXTINF:-1,Alice - First\r\n\
                      /channel/songs/a\r\n\
                      #EXTINF:-1,Bob - Second\r\n\
                      /channel/songs/b\r\n"
            .to_vec();
        let server = cache_server::Server::start(vec![("/channel.m3u", body)]);
        let mut app = app_with_favourite(server.url("/channel.m3u"));

        app.on_key(KeyEvent::from(KeyCode::Char('G'))); // the favourite: the last actable row
        let action = app.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(action, super::Action::PlayQueue { from: 0 });
        assert_eq!(
            server.requests(),
            vec!["/channel.m3u".to_owned()],
            "one probe fetch"
        );

        // Opening it again: the verdict is remembered for the session (S4),
        // so the wrapper is not fetched a second time.
        app.on_key(KeyEvent::from(KeyCode::Char('G')));
        let action = app.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(action, super::Action::PlayQueue { from: 2 });
        assert_eq!(
            server.requests(),
            vec!["/channel.m3u".to_owned()],
            "the second open fetched nothing new"
        );
    }

    #[test]
    fn a_failed_expansion_is_not_remembered_and_is_retried_on_the_next_open() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent};

        let server = cache_server::Server::start(Vec::new()); // every path 404s
        let mut app = app_with_favourite(server.url("/gone.m3u"));

        app.on_key(KeyEvent::from(KeyCode::Char('G')));
        let action = app.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(action, super::Action::None, "a refusal queues nothing");
        assert!(app.queue.is_empty(), "nothing was queued");
        assert!(app.status.contains("404"), "got: {:?}", app.status);

        // The failure is not remembered: the next open fetches again, so a
        // transient error stays retryable (S4).
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(
            server.requests(),
            vec!["/gone.m3u".to_owned(), "/gone.m3u".to_owned()],
            "the second open fetched again"
        );
    }

    /// The cover switch decides before the terminal's capability and the
    /// track's art do (S5): with it off a decoded cover takes no columns and
    /// is never explained; with it on, a cover that cannot be drawn is
    /// explained once. The integration tests cannot hand the interface a
    /// decoded cover without a live stream, so this differential lives here.
    #[test]
    fn the_cover_switch_decides_before_the_capability_and_the_art_do() {
        let mut app = super::App::new(Vec::new());
        app.picker = Some(super::Picker::new((8, 12)));
        app.artwork = Some(super::Artwork::for_test());

        // On: a decoded cover on a window below the art threshold is
        // explained once, in the config's own words.
        app.seen_area.set(ratatui::layout::Rect::new(0, 0, 96, 24));
        app.explain_hidden_art();
        assert!(
            app.status.contains("cover art hidden"),
            "got: {:?}",
            app.status
        );

        // Off: a decision, not a malfunction — no apology, and above the
        // threshold no columns either: the block's text stays at the left
        // edge where a cover would have pushed it right.
        app.status.clear();
        app.art_explained = false;
        app.ui.cover_art = false;
        app.explain_hidden_art();
        assert!(app.status.is_empty(), "got: {:?}", app.status);

        // Advisories off (S5.4): the same window is never explained — the
        // switch silences exactly this line, nothing else.
        app.status.clear();
        app.art_explained = false;
        app.ui.cover_art = true;
        app.ui.advisories = false;
        app.explain_hidden_art();
        assert!(app.status.is_empty(), "got: {:?}", app.status);

        // The draw leg below needs the cover deliberately hidden again.
        app.ui.cover_art = false;

        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let title: String = (0..120)
            .map(|x| buffer[(x, 2)].symbol().to_owned())
            .collect();
        assert_eq!(
            title.chars().position(|character| character == '♫'),
            Some(2),
            "the block's text starts at the left edge, got: {title}"
        );
    }

    #[test]
    fn the_cover_sits_at_the_left_of_the_now_playing_block() {
        use ratatui::layout::Rect;

        // The 11-row block with the spectrum: a cover two columns per row
        // (terminal cells are about twice as tall as wide), the block's
        // height less its trailing gap, and two columns of air after it.
        let block = Rect::new(2, 2, 96, 11);
        let (art, rest) = super::cover_slot(block).expect("room for a cover");
        assert_eq!(art, Rect::new(2, 2, 20, 10));
        assert_eq!(rest, Rect::new(24, 2, 74, 11));

        // The seven-row block: a smaller cover, same rule.
        let (art, rest) = super::cover_slot(Rect::new(2, 2, 96, 7)).expect("a cover");
        assert_eq!((art.width, art.height, rest.x), (12, 6, 16));

        // A compact block, or one too narrow to keep the rest readable, has
        // no cover at all.
        assert_eq!(super::cover_slot(Rect::new(2, 2, 96, 3)), None);
        assert_eq!(super::cover_slot(Rect::new(2, 2, 50, 11)), None);
    }

    #[test]
    fn only_a_spectrum_on_screen_that_moves_earns_thirty_frames_a_second() {
        use std::time::Duration;
        let (fast, slow) = (Duration::from_millis(33), Duration::from_millis(100));

        assert_eq!(
            super::cadence(true, true, false, false),
            fast,
            "visible and playing"
        );
        assert_eq!(
            super::cadence(true, false, true, false),
            fast,
            "visible, bars falling"
        );
        assert_eq!(
            super::cadence(false, true, false, false),
            slow,
            "playing, but hidden"
        );
        assert_eq!(
            super::cadence(false, false, true, false),
            slow,
            "hidden, falling"
        );
        assert_eq!(
            super::cadence(true, false, false, false),
            slow,
            "visible and still"
        );
        // Low power (issue #11, S2) draws at a quarter of whatever the
        // ordinary rate would have been, fast or slow.
        assert_eq!(
            super::cadence(true, true, false, true),
            Duration::from_millis(132)
        );
        assert_eq!(
            super::cadence(false, false, false, true),
            Duration::from_millis(400)
        );
    }

    #[test]
    fn spectrum_bars_fill_from_the_bottom_in_eighths_with_a_gap_between() {
        let rows = |levels: &[f32]| -> Vec<String> {
            super::spectrum_lines(levels, 4, &super::Theme::default())
                .iter()
                .map(ToString::to_string)
                .collect()
        };

        // Full, half, an eighth of the lowest row, and silence.
        assert_eq!(
            rows(&[1.0, 0.5, 1.0 / 32.0, 0.0]),
            ["█       ", "█       ", "█ █     ", "█ █ ▁ ▁ "],
            "four bars, each one column with one of air"
        );
    }

    #[test]
    fn hints_that_do_not_fit_lose_the_middle_and_keep_help_and_quit() {
        let hints = [
            ("j/k", "move"),
            ("enter", "play"),
            ("?", "help"),
            ("q", "quit"),
        ];
        let text = |width| super::hint_line(&hints, width, &super::Theme::default()).to_string();

        assert_eq!(text(80), "j/k move  enter play  ? help  q quit");
        assert_eq!(text(28), "j/k move  ? help  q quit");
        assert_eq!(text(5), "? help  q quit", "the last two always stay");
    }

    #[test]
    fn an_edit_refuses_the_entry_that_is_playing() {
        assert_eq!(
            App::editable_queue_index_among(3, Some(1), 1),
            Err("velo: cannot remove the track that is playing".to_owned())
        );
    }

    #[test]
    fn an_edit_refuses_every_entry_before_the_playing_one() {
        assert_eq!(
            App::editable_queue_index_among(3, Some(2), 0),
            Err("velo: that track already played".to_owned())
        );
        assert_eq!(
            App::editable_queue_index_among(3, Some(2), 1),
            Err("velo: that track already played".to_owned())
        );
    }

    #[test]
    fn an_edit_allows_every_entry_after_the_playing_one() {
        assert_eq!(App::editable_queue_index_among(3, Some(0), 1), Ok(1));
        assert_eq!(App::editable_queue_index_among(3, Some(0), 2), Ok(2));
        // Nothing is playing: the whole queue is editable.
        assert_eq!(App::editable_queue_index_among(3, None, 0), Ok(0));
    }

    #[test]
    fn a_move_refuses_to_land_on_or_across_the_playing_entry() {
        // Playing entry 0: nothing may move up into or across it.
        assert_eq!(App::queue_move_target(3, Some(0), 1, -1), None);
        assert_eq!(App::queue_move_target(3, Some(0), 2, -2), None);
        // Playing entry 1: the entry after it may not step back onto it.
        assert_eq!(App::queue_move_target(3, Some(1), 2, -1), None);
    }

    #[test]
    fn a_move_lands_only_after_the_playing_entry_or_outside_the_queue() {
        // Playing entry 0: entry 1 may move down, entry 2 up to 1 — never 0.
        assert_eq!(App::queue_move_target(3, Some(0), 1, 1), Some(2));
        assert_eq!(App::queue_move_target(3, Some(0), 2, -1), Some(1));
        // Nothing is playing: the whole queue is reachable, except outside it.
        assert_eq!(App::queue_move_target(3, None, 2, -1), Some(1));
        assert_eq!(App::queue_move_target(3, None, 0, -1), None);
        assert_eq!(App::queue_move_target(3, None, 2, 1), None);
    }

    // ---- the station's own `next` (issue #17) --------------------------------
    //
    // The human's rule, recorded in odd/tasks/radio.md (L29) before the code:
    // `next` while a station plays goes to the FIRST track of the queue —
    // "Ir a la primera pista de la cola" — and with no track in the queue it
    // refuses with one status line and leaves the station playing. The rule is
    // pure and lives in one place `skip` reads, the way the playing-entry
    // refusals above do, so the answer is the same on every screen.

    fn station(name: &str) -> Entry {
        Entry::Station {
            url: format!("https://stream.example/{name}"),
            name: name.to_owned(),
        }
    }

    #[test]
    fn next_while_a_station_plays_jumps_to_the_first_track_of_the_queue() {
        use std::path::PathBuf;

        // The station plays at the head; the first track is the very first
        // entry that is a track — not the entry after the station — so an
        // evening of stations lined up behind it is left whole.
        let queue = vec![
            station("nova"),
            Entry::from(PathBuf::from("/music/one.flac")),
        ];
        assert_eq!(App::station_next_target(&queue), Ok(1));

        // Stations first, tracks later: the first *track* is the answer.
        let queue = vec![
            station("nova"),
            station("cap"),
            Entry::from(PathBuf::from("/music/one.flac")),
            Entry::from(PathBuf::from("/music/two.flac")),
        ];
        assert_eq!(App::station_next_target(&queue), Ok(2));
    }

    #[test]
    fn next_while_a_station_plays_with_no_track_refuses_and_leaves_it_playing() {
        use std::path::PathBuf;

        let refusal = "velo: no track in the queue — the station keeps playing";
        assert_eq!(
            App::station_next_target(&[station("nova")]),
            Err(refusal),
            "nothing but the station: it keeps playing"
        );
        let queue = vec![station("nova"), station("cap")];
        assert_eq!(
            App::station_next_target(&queue),
            Err(refusal),
            "an evening of stations and not one track: the station keeps playing"
        );
        // A queue of tracks alone never reaches this rule: `skip` answers from
        // `skip_target` unless the entry that plays is a station.
        assert_eq!(
            App::station_next_target(&[Entry::from(PathBuf::from("/music/one.flac"))]),
            Ok(0)
        );
    }
}
