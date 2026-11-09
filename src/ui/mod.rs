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
pub mod search;
pub mod shuffle;
pub mod sources;
pub mod theme;

pub use ratatui;
pub use ratatui::crossterm;

use std::cell::RefCell;

use std::path::{Path, PathBuf};
use std::sync::mpsc::TryRecvError;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph};

use crate::audio::output::{Entry, PathResolver, Repeat, SharedQueue, TrackFailure};
use crate::audio::spectrum::Spectrum;
use crate::audio::{Player, seek_target, skip_target};
use crate::config::{OpenNode, ReplayGainMode, Session, ThemeColor, ThemeColors, UiParts};
use crate::library::{AlbumEntry, ArtistEntry, GenreEntry, IndexReport, Library, TrackEntry};
use crate::subsonic::cache::CoverArt;
use crate::subsonic::scrobble::{ScrobbleTracker, Scrobbles};

use self::artwork::{Artwork, Picker, StatefulImage, StatefulProtocol};
use self::search::Search;
use self::theme::Theme;

/// What the interface says at a launch with neither a library nor a Subsonic
/// server configured (T12c): one sentence naming both ways to configure, in
/// the place the floating prompt used to take over. `L` still opens the
/// prompt at any time.
pub const NO_LIBRARY_OR_SERVER_MESSAGE: &str = "velo: no library and no server configured — set \
    `library` in the config file (or run velo --library <PATH>), or add a \
    [subsonic] server and run velo --sync";

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
    /// Play the next track of the stream.
    Next,
    /// Restart the track, or play the one before it when it just began.
    Previous,
    /// Shuffle what is left of the queue, or put it back in order.
    ToggleShuffle,
    /// What follows the end of a track from now on.
    SetRepeat(Repeat),
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

/// Refreshing the index (library-refresh T3): which folder `u` re-indexes
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
    /// The open radio view (T6a): `None` while only the library and the
    /// queue are on screen.
    radio: Option<radio::RadioView>,
    /// The transport fixture the integration tests install (T6c): no test
    /// can build a `Player` — building one opens an audio device — so the
    /// tests stand in what a playing player would report. Production never
    /// sets it, and every reader takes the real player first, so the player
    /// stays the transport's only source of truth.
    transport_fixture: Option<TransportFixture>,
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
    /// The palette as the `[theme]` table wrote it (S4): what the settings
    /// card rebuilds with one role replaced when a colour is cycled.
    colors: ThemeColors,
    /// Present while no library root is set (S4): the empty browser then
    /// says how to set one, and while its prompt is open every key edits it.
    library_setup: Option<LibrarySetup>,
    /// The config file a library chosen with `L` is saved to, and the file
    /// every persisted value is written to (S2).
    config_file: Option<PathBuf>,
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
    /// An accepted library root, or the one `u` refreshes, waiting for the
    /// shell to index it.
    library_root: Option<PathBuf>,
    refresh: Refresh,
    /// Whether playback is bit-perfect (T17): the volume is pinned to 100%
    /// and `ReplayGain` is skipped, both explained once in the status line.
    bit_perfect: bool,
    /// Whether the bit-perfect volume pinning has been explained already, so
    /// the status line says it once, not once per volume key press.
    bit_perfect_volume_said: bool,
    volume: u8,
    state: PlayState,
    player: Option<Player>,
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
}

impl<'lib> App<'lib> {
    /// An interface over the given files, selection at the top.
    #[must_use]
    pub fn new(tracks: Vec<PathBuf>) -> App<'static> {
        App {
            position: Position::Files,
            sources: sources::Sources::default(),
            radio_from: View::Library,
            url_opener: open_url,
            tab_origin: View::Sources,
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
            radio: None,
            transport_fixture: None,
            radio_wire: None,
            radio_refusal: None,
            colors: ThemeColors::default(),
            library_setup: None,
            config_file: None,
            resolver: None,
            covers: None,
            pending_cover: None,
            scrobbles: None,
            subsonic_warning: None,
            saved: Session::default(),
            saved_volume: crate::config::DEFAULT_VOLUME,
            library_root: None,
            refresh: Refresh::default(),
            bit_perfect: false,
            bit_perfect_volume_said: false,
            volume: 100,
            state: PlayState::Stopped,
            player: None,
            status: String::new(),
            spectrum: Spectrum::new(),
            heard: Vec::new(),
            spectrum_bands: std::cell::Cell::new(0),
        }
    }

    /// An interface browsing the library index. The artists snapshot is read
    /// here, once; deeper levels are read when the position changes.
    #[must_use]
    pub fn browse(library: Option<&'lib Library>) -> Self {
        let mut app = Self {
            position: Position::Artists,
            sources: sources::Sources::default(),
            radio_from: View::Library,
            url_opener: open_url,
            tab_origin: View::Sources,
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
            radio: None,
            transport_fixture: None,
            radio_wire: None,
            radio_refusal: None,
            colors: ThemeColors::default(),
            library_setup: None,
            config_file: None,
            resolver: None,
            covers: None,
            pending_cover: None,
            scrobbles: None,
            subsonic_warning: None,
            saved: Session::default(),
            saved_volume: crate::config::DEFAULT_VOLUME,
            library_root: None,
            refresh: Refresh::default(),
            bit_perfect: false,
            bit_perfect_volume_said: false,
            volume: 100,
            state: PlayState::Stopped,
            player: None,
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

    /// The palette the interface draws with, from the config's `[theme]` (S4):
    /// `accent_bright` stays derived from the accent. The palette as the file
    /// wrote it is kept too, so the settings card can rebuild it with one
    /// role replaced (S7).
    pub fn set_theme(&mut self, colors: ThemeColors) {
        self.colors = colors;
        self.theme = colors.into();
    }

    /// Which parts of the interface are drawn, from the config's `[ui]` (S5):
    /// the switches the layout reads, everything on by default.
    pub fn set_ui(&mut self, ui: UiParts) {
        self.ui = ui;
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
            Box::new(radio_data::RadioData::open(store_path))
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
    /// card `L` opens.
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
        }
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

    /// How many rows the now-playing block takes out of a content area this
    /// tall, with the `[ui]` switches applied (S5). The spectrum belongs to
    /// the tall block: with the switch off the block never reserves its
    /// rows — the tall tier is capped at the seven-row block, and the bars'
    /// rows go to the list instead.
    fn now_playing_rows(&self, height: u16) -> u16 {
        // Content tall enough for the spectrum tier starts at 22 rows; any
        // shorter height keeps the block at its ordinary size.
        const SPECTRUM_TIER: u16 = 22;
        now_playing_rows(if self.ui.spectrum {
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
            Position::Tracks { .. } | Position::AllTracks | Position::Genre { .. } => self
                .tracks
                .get(from..)
                .unwrap_or_default()
                .iter()
                .map(|entry| entry.path.clone())
                .collect(),
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

    /// The config file a library chosen later with `L` is saved to.
    pub fn set_config_file(&mut self, config_file: Option<PathBuf>) {
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
    pub fn set_subsonic_warnings(&mut self, warnings: &[String]) {
        self.subsonic_warning = (!warnings.is_empty()).then(|| warnings.join("; "));
    }

    /// The launch with no `library` configured (T12c): with a Subsonic server
    /// configured, the synced index is the library — nothing is asked, and
    /// `/` reaches its search; with neither, one sentence says what to
    /// configure instead of asking for a folder. `L` still opens the prompt
    /// at any time.
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

    /// The library root `u` refreshes: the configured one, or `None` when no
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

    /// What `u` answers: the refresh to run, or why there is none.
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
        });
    }

    /// Runs once per turn of the event loop — the same cadence as key
    /// polling — so the producer's announcements surface without a key
    /// press, the cover art follows the track it plays (T8), and the server
    /// learns what is playing (T12d). Without a player there is nothing to
    /// collect.
    pub fn tick(&mut self) {
        // A credential warning is said once, here, where the user is looking
        // — not only inside a skip reason when a download later fails. The
        // `take` is the once: later ticks find nothing left to say.
        if let Some(warning) = self.subsonic_warning.take() {
            self.status = format!("velo: warning: {warning}");
        }
        // The radio wiring (T6b) turns once per tick too: a finished
        // directory request lands without a key press, and an in-flight
        // one says `⟳ loading…` on the status line.
        self.radio_poll();
        self.poll_cover();
        self.refresh_artwork();
        self.explain_hidden_art();
        self.listen();
        let path = self.playing_path().map(Path::to_path_buf);
        let (position, duration) = match &self.player {
            Some(player) => (player.position(), self.current_track_duration()),
            None => (Duration::ZERO, None),
        };
        self.scrobble_tick(path, position, duration);
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
    /// — a window too short for the spectrum has nothing to animate.
    #[must_use]
    pub fn frame_interval(&self) -> Duration {
        cadence(
            self.spectrum_bands.get() > 0,
            self.state == PlayState::Playing,
            self.spectrum.levels().iter().any(|&level| level > 0.0),
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
            Position::Files | Position::Artists => {}
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
        // An open settings card owns every key too (S6): nothing reaches the
        // lists or the transport while it is up, `q` and `space` included.
        if self.settings.is_some() {
            return self.on_settings_key(event.code);
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
            && let Some(action) = self.sources_key(event.code)
        {
            return action;
        }

        match event.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.select_next();
                Action::None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.select_previous();
                Action::None
            }
            KeyCode::Char('g') => {
                self.select_first();
                Action::None
            }
            KeyCode::Char('G') => {
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
            KeyCode::Char('d') if self.view == View::Queue => {
                self.remove_from_queue();
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
            KeyCode::Char(key @ '1'..='4') if !matches!(self.position, Position::Files) => {
                BrowseBy::from_key(key).map_or(Action::None, Action::Browse)
            }
            KeyCode::Char(' ') => Action::TogglePause,
            // The second seek gear (issue #6): shift on an arrow, which a
            // terminal does report as a modifier, unlike a character key that
            // arrives already shifted. `,` and `.` carry the same pair as the
            // characters shift produces, for both the layouts that need no
            // shift to type them and the terminals that drop the modifier.
            KeyCode::Left if event.modifiers.contains(KeyModifiers::SHIFT) => {
                Action::Seek { delta_secs: -30 }
            }
            KeyCode::Right if event.modifiers.contains(KeyModifiers::SHIFT) => {
                Action::Seek { delta_secs: 30 }
            }
            KeyCode::Char('<') => Action::Seek { delta_secs: -30 },
            KeyCode::Char('>') => Action::Seek { delta_secs: 30 },
            // The plain keys keep the coarse five points; the brackets are the
            // fine gear, a point a press (issue #16). Shift cannot carry it: a
            // terminal sends the shifted *character*, and on a US layout
            // `shift`+`=` arrives as the `+` that already means five points
            // up, so only the down side would have a modifier to read.
            KeyCode::Char('+' | '=') => Action::SetVolume(self.change_volume(5)),
            KeyCode::Char('-') => Action::SetVolume(self.change_volume(-5)),
            KeyCode::Char(']') => Action::SetVolume(self.change_volume(1)),
            KeyCode::Char('[') => Action::SetVolume(self.change_volume(-1)),
            KeyCode::Left | KeyCode::Char(',') => Action::Seek { delta_secs: -5 },
            KeyCode::Right | KeyCode::Char('.') => Action::Seek { delta_secs: 5 },
            KeyCode::Char('n') => Action::Next,
            KeyCode::Char('p') => Action::Previous,
            KeyCode::Char('s') => Action::ToggleShuffle,
            // The library can be set again at any time (library-refresh T1);
            // the files given on the command line have none to set.
            KeyCode::Char('L') if !matches!(self.position, Position::Files) => {
                self.ask_for_library(self.config_file.clone());
                Action::None
            }
            KeyCode::Char('u') if !matches!(self.position, Position::Files) => {
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
            KeyCode::Char('o') => {
                self.settings = Some(SettingsCard::default());
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
    /// `o`, `s`, `r` and the seek keys stay global (S7).
    fn sources_key(&mut self, code: ratatui::crossterm::event::KeyCode) -> Option<Action> {
        use ratatui::crossterm::event::KeyCode;

        match code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.sources.select_next();
                Some(Action::None)
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.sources.select_previous();
                Some(Action::None)
            }
            KeyCode::Char('g') => {
                self.sources.select_first();
                Some(Action::None)
            }
            KeyCode::Char('G') => {
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
            | KeyCode::Char('1'..='4' | 'a' | 'd' | 'J' | 'K' | '/') => Some(Action::None),
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
            radio::RadioOutcome::Play { url, name } => {
                // The stream can take a while to open (S10): the status
                // names the station now, before the player has answered.
                // `start_stream`'s success clears the line and its failure
                // names the error, so a stale "opening" never lingers.
                self.status = format!("velo: opening {name}…");
                // The station joins the queue and plays from it (T6c): the
                // queue is edited here, and the play action goes to the
                // shell, which builds the player the way `enter` on a queue
                // row does. The radio stays open.
                self.edit_queue(|queue| queue.push(Entry::Station { url, name }));
                let from = self.queue.len().saturating_sub(1);
                Some(Action::PlayQueue { from })
            }
            radio::RadioOutcome::Queue { url, name } => {
                self.edit_queue(|queue| queue.push(Entry::Station { url, name }));
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
        // is told, never guesses. A station playing marks its rows wherever
        // they are listed; a local file or silence marks none. radio_poll
        // runs on every tick and after every radio key, so no key can leave
        // the marker stale.
        let playing = match self.playing_entry() {
            Some(Entry::Station { url, .. }) => Some(url.clone()),
            _ => None,
        };
        if let Some(view) = self.radio.as_mut() {
            view.set_playing(playing);
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
            Action::Next => self.skip(true),
            Action::Previous => self.skip(false),
            Action::ToggleShuffle => self.toggle_shuffle(),
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
    fn enter(&self) -> Action {
        let play = Action::Play {
            from: self.selected,
        };
        match &self.position {
            Position::Files if !self.argv.is_empty() => play,
            Position::Tracks { .. } | Position::AllTracks | Position::Genre { .. }
                if !self.tracks.is_empty() =>
            {
                play
            }
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
            KeyCode::Char('g') => help.top = 0,
            KeyCode::Char('G') => help.top = last_top,
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
            KeyCode::Esc => self.settings = None,
            KeyCode::Char('j') | KeyCode::Down => {
                card.move_down();
                card.keep_visible(SettingsCard::card_rows(area));
            }
            KeyCode::Char('k') | KeyCode::Up => {
                card.move_up();
                card.keep_visible(SettingsCard::card_rows(area));
            }
            KeyCode::Char('g') => {
                card.cursor = 0;
                card.top = 0;
            }
            KeyCode::Char('G') => {
                card.cursor = SettingsCard::setting_count() - 1;
                card.keep_visible(SettingsCard::card_rows(area));
            }
            KeyCode::Enter | KeyCode::Right => {
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
            SettingsKey::Accent => self.change_color(ColorRole::Accent, forward),
            SettingsKey::Text => self.change_color(ColorRole::Text, forward),
            SettingsKey::Dim => self.change_color(ColorRole::Dim, forward),
            SettingsKey::Secondary => self.change_color(ColorRole::Secondary, forward),
            SettingsKey::Border => self.change_color(ColorRole::Border, forward),
            SettingsKey::Ink => self.change_color(ColorRole::Ink, forward),
            SettingsKey::Background => self.change_color(ColorRole::Background, forward),
            SettingsKey::CoverArt => self.toggle_ui("cover_art", |ui| &mut ui.cover_art),
            SettingsKey::Spectrum => self.toggle_ui("spectrum", |ui| &mut ui.spectrum),
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

    /// Cycles one colour role through [`COLOR_PRESETS`] (S7): forward or
    /// back, wrapping; a colour that is not a preset — the user's own hex —
    /// steps to the first preset going forward and to the last going back.
    /// The palette is rebuilt from the current one with the one role
    /// replaced, applied through [`App::set_theme`], and written to the file
    /// under `[theme]`.
    fn change_color(&mut self, role: ColorRole, forward: bool) {
        let current = role.get(self.colors).to_key();
        let next = Self::next_color(&current, role.allows_reset(), forward);
        let Some(colour) = parse_color(next) else {
            return;
        };
        let mut colors = self.colors;
        role.set(&mut colors, colour);
        self.set_theme(colors);
        self.remember(Some("theme"), role.key(), &Self::toml_string(next));
    }

    /// The colour `forward` cycles to from `current` (S7): the next preset
    /// forward, the previous back, both wrapping; a colour that is no preset
    /// steps to the first one forward and to the last one back. The two
    /// roles that may be `reset` cycle it ahead of the presets.
    fn next_color(current: &str, allows_reset: bool, forward: bool) -> &'static str {
        let reset = ["reset"];
        let head: &[&str] = if allows_reset { &reset } else { &[] };
        let mut list = Vec::with_capacity(head.len() + COLOR_PRESETS.len());
        list.extend_from_slice(head);
        list.extend_from_slice(&COLOR_PRESETS);
        match list.iter().position(|&colour| colour == current) {
            Some(index) if forward => list[(index + 1) % list.len()],
            Some(index) => list[(index + list.len() - 1) % list.len()],
            None if forward => list[0],
            None => list[list.len() - 1],
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
            SettingsKey::Accent => self.colors.accent.to_key(),
            SettingsKey::Text => self.colors.text.to_key(),
            SettingsKey::Dim => self.colors.dim.to_key(),
            SettingsKey::Secondary => self.colors.secondary.to_key(),
            SettingsKey::Border => self.colors.border.to_key(),
            SettingsKey::Ink => self.colors.ink.to_key(),
            SettingsKey::Background => self.colors.background.to_key(),
            SettingsKey::CoverArt
            | SettingsKey::Spectrum
            | SettingsKey::Hints
            | SettingsKey::TopBar
            | SettingsKey::Advisories
            | SettingsKey::ViewNumbers => {
                let drawn = match key {
                    SettingsKey::CoverArt => self.ui.cover_art,
                    SettingsKey::Spectrum => self.ui.spectrum,
                    SettingsKey::Hints => self.ui.hints,
                    SettingsKey::Advisories => self.ui.advisories,
                    SettingsKey::ViewNumbers => self.ui.view_numbers,
                    _ => self.ui.top_bar,
                };
                if drawn { "shown" } else { "hidden" }.to_owned()
            }
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
        // Set again by `draw_spectrum` only when this frame has room for it.
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
        let [now, header, body] = Layout::vertical([
            Constraint::Length(self.now_playing_rows(content.height)),
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
        self.draw_body(frame, body);
        // The radio view (T6a) replaces the list, not the block above it. It
        // never paints over the queue (issue #23): when the round trip put
        // the queue on screen, the queue is what the body draws.
        if self.view != View::Queue
            && let Some(view) = &self.radio
        {
            view.render(frame, body, &self.theme);
        }
        self.draw_footer(frame, footer);
        self.draw_artwork(frame, pane);
        if let Some(help) = &self.help {
            Self::draw_help(frame, area, help, &self.theme);
        }
        if let Some(card) = &self.settings {
            self.draw_settings_card(frame, area, card);
        }
        if let Some(prompt) = self.library_prompt() {
            Self::draw_library_prompt(frame, area, prompt, &self.theme);
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
            Some(Entry::File(path)) => describe(path, self.known_tracks()),
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
                self.draw_transport(frame, bottom, volume);
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
                self.draw_spectrum(frame, spectrum_area);
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
                self.draw_transport(frame, transport_row, volume);
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

    /// The buttons on the left, the volume slider on the right.
    fn draw_transport(&self, frame: &mut Frame, area: Rect, volume: u8) {
        let button = Style::new().fg(self.theme.secondary);
        let lit = |on: bool| {
            if on {
                self.theme.accent
            } else {
                self.theme.dim
            }
        };
        let primary = if self.state == PlayState::Playing {
            " ❚❚ pause "
        } else {
            " ▶ play "
        };
        let buttons = Line::from(vec![
            Span::styled(" ◂◂ ", button),
            Span::raw(" "),
            Span::styled(
                primary,
                Style::new().bg(self.theme.accent).fg(self.theme.ink).bold(),
            ),
            Span::raw(" "),
            Span::styled(" ▸▸ ", button),
            // Shuffle and repeat: dim while off, the accent while on.
            Span::raw("   "),
            Span::styled("shuffle", Style::new().fg(lit(self.shuffle))),
            Span::styled("  repeat ", Style::new().fg(self.theme.dim)),
            Span::styled(
                self.repeat.label(),
                Style::new().fg(lit(self.repeat != Repeat::Off)),
            ),
        ]);
        // The ReplayGain mode in the config's own words, padded to the
        // longest so the slider never moves with it (ui-redesign T10).
        let mode = match self.replay_gain {
            ReplayGainMode::Track => "track",
            ReplayGainMode::Album => "album",
            ReplayGainMode::Off => "off",
        };
        let mut right = vec![
            Span::styled("RG ", Style::new().fg(self.theme.dim)),
            Span::styled(format!("{mode:<5}"), Style::new().fg(self.theme.secondary)),
            Span::raw("   "),
        ];
        right.extend(volume_slider(volume, VOLUME_CELLS, &self.theme));
        split_row(frame, area, buttons, Line::from(right));
    }

    /// The position, and the length when it is known; a dim placeholder
    /// when nothing plays. A live station has neither (S3): the transport
    /// says so where a position and total would be.
    fn clock_spans(&self) -> Vec<Span<'static>> {
        if matches!(self.playing_entry(), Some(Entry::Station { .. })) {
            return vec![Span::styled("● live", Style::new().fg(self.theme.accent))];
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

    /// The spectrum bars, as many as the row holds at two columns each, and
    /// the band count remembered so the next tick analyzes exactly that many.
    fn draw_spectrum(&self, frame: &mut Frame, area: Rect) {
        if area.height == 0 {
            return;
        }
        let bands = usize::from(area.width).div_ceil(2);
        self.spectrum_bands.set(bands);
        let mut levels = self.spectrum.levels().to_vec();
        levels.resize(bands, 0.0);
        frame.render_widget(
            Paragraph::new(spectrum_lines(&levels, area.height, &self.theme)),
            area,
        );
    }

    /// The play state as a pill: `▶ playing` in the accent, the others dim.
    fn state_pill(&self) -> Span<'static> {
        let (glyph, color) = match self.state {
            PlayState::Playing => ("▶", self.theme.accent),
            PlayState::Paused => ("❚❚", self.theme.secondary),
            PlayState::Stopped => ("■", self.theme.dim),
        };
        Span::styled(
            format!(" {glyph} {} ", self.state.label()),
            Style::new().fg(color),
        )
    }

    /// The line over the list: what it lists and how many of them, as in
    /// `▸─ Queue  [12 tracks]`. Over the library the four views come first,
    /// the active one lit, as in
    /// `▸─ artists  albums  tracks  genres   Tide › Harbor  [12 tracks]`.
    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        if self.search.is_none()
            && self.view == View::Library
            && !matches!(self.position, Position::Files)
        {
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
            | Position::Genre { .. } => ("track", "tracks"),
            Position::Artists => ("artist", "artists"),
            Position::Albums { .. } | Position::AllAlbums => ("album", "albums"),
            Position::Genres => ("genre", "genres"),
        };
        plural(self.row_count(), one, many)
    }

    /// The list the keys act on — the search results, the queue or the
    /// library level — or the sentence an empty one says about itself.
    fn draw_body(&self, frame: &mut Frame, area: Rect) {
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
    fn source_rows(&self, width: u16) -> Vec<ListItem<'static>> {
        let digits = digits(sources::Source::ALL.len());
        sources::Source::ALL
            .into_iter()
            .enumerate()
            .map(|(index, source)| {
                ListItem::new(list_row(
                    ListRow {
                        playing: false,
                        number: index + 1,
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
        let playing = self.playing_row();
        (0..self.row_count())
            .map(|index| {
                let (name, note, fact) = self.row_text(index);
                ListItem::new(list_row(
                    ListRow {
                        playing: playing == Some(index),
                        number: index + 1,
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

    /// What row `index` of the current level says: its name, the note
    /// dimmed after it and the fact against the right edge.
    fn row_text(&self, index: usize) -> (String, String, String) {
        let count = |count: u64, one: &str, many: &str| {
            plural(usize::try_from(count).unwrap_or(usize::MAX), one, many)
        };
        match &self.position {
            Position::Files => {
                let (name, folder) = describe(&self.argv[index], []);
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
                    format!("{} · {}", entry.artist, entry.album)
                };
                (entry.title.clone(), note, Self::duration(entry.duration_ms))
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
    /// a file and marked the way every other list marks the row that plays.
    fn queue_rows(&self, width: u16) -> Vec<ListItem<'static>> {
        let digits = digits(self.queue.len());
        let playing = self.playing_queue_index();
        self.queue
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                // A station is named for itself, never resolved to a path:
                // its own name is what the row shows.
                let (name, note) = match entry {
                    Entry::File(path) => describe(path, self.known_tracks()),
                    Entry::Station { name, .. } => (name.clone(), String::new()),
                };
                ListItem::new(list_row(
                    ListRow {
                        playing: playing == Some(index),
                        number: index + 1,
                        digits,
                        name,
                        note,
                        fact: String::new(),
                    },
                    width,
                    &self.theme,
                ))
            })
            .collect()
    }

    /// The search results: a title with its artist and album dimmed after
    /// it and its length at the right edge.
    fn search_rows(&self, width: u16) -> Vec<ListItem<'static>> {
        let digits = digits(self.search_results.len());
        let playing = self.playing_path();
        self.search_results
            .iter()
            .filter_map(|&index| self.search_pool.get(index))
            .enumerate()
            .map(|(row, entry)| {
                ListItem::new(list_row(
                    ListRow {
                        playing: playing == Some(entry.path.as_path()),
                        number: row + 1,
                        digits,
                        name: entry.title.clone(),
                        note: format!("{} · {}", entry.artist, entry.album),
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
                ("n/p", "next/prev"),
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
                ("n/p", "next/prev"),
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

    /// How many rows the current level has.
    fn row_count(&self) -> usize {
        match &self.position {
            Position::Files => self.argv.len(),
            Position::Artists => self.artists.len(),
            Position::Albums { .. } | Position::AllAlbums => self.albums.len(),
            Position::Tracks { .. } | Position::AllTracks | Position::Genre { .. } => {
                self.tracks.len()
            }
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

    /// The list position of the row whose samples are being produced: the row
    /// holding the file that plays. Found by path, not by where the stream
    /// started plus the producer's index, because a shuffled queue no longer
    /// follows the list's order.
    fn playing_row(&self) -> Option<usize> {
        let path = self.playing_path()?;
        match &self.position {
            Position::Files => self.argv.iter().position(|row| row == path),
            Position::Tracks { .. } | Position::AllTracks | Position::Genre { .. } => {
                self.tracks.iter().position(|row| row.path == path)
            }
            Position::Artists
            | Position::Albums { .. }
            | Position::AllAlbums
            | Position::Genres => None,
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

    fn select_next(&mut self) {
        let index = self.view_selection();
        if index + 1 < self.view_len() {
            self.set_view_selection(index + 1);
        }
    }

    fn select_previous(&mut self) {
        let index = self.view_selection();
        self.set_view_selection(index.saturating_sub(1));
    }

    fn select_first(&mut self) {
        self.set_view_selection(0);
    }

    fn select_last(&mut self) {
        self.set_view_selection(self.view_len().saturating_sub(1));
    }

    /// The tab round trip (issue #23): from the queue, back to the screen
    /// `tab` was pressed on; from anywhere else — the sources screen, the
    /// library at any level, the radio at any depth — to the queue. The
    /// status line names the queue when it arrives and is cleared when it
    /// leaves.
    fn toggle_view(&mut self) {
        if self.view == View::Queue {
            self.view = self.tab_origin;
        } else {
            self.tab_origin = self.view;
            self.view = View::Queue;
        }
        if self.view == View::Queue {
            self.queue_selected = self.queue_selected.min(self.queue.len().saturating_sub(1));
            self.status = format!(
                "velo: queue, {}",
                plural(self.queue.len(), "track", "tracks")
            );
        } else {
            self.status.clear();
        }
        // The tab is a transient round trip, not a setting (S14): moving it
        // writes nothing.
    }

    /// The queue index that is playing, when anything is: the player counts
    /// from the list it was given, which is this queue.
    fn playing_queue_index(&self) -> Option<usize> {
        self.playing.as_ref()?;
        self.player.as_ref().map(Player::current_index)
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
            // `Files` does not descend; track lists are the deepest levels
            // and answer with `Action::Play` instead.
            Position::Files
            | Position::Tracks { .. }
            | Position::AllTracks
            | Position::Genre { .. } => {}
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
        if !matches!(self.position, Position::Files) {
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

    /// Opens a device for these entries and makes them the queue.
    fn start_stream(&mut self, paths: Vec<Entry>, playback: Playback) {
        // Dropping through `stop` also releases the producer thread.
        if let Some(old) = self.player.take() {
            old.stop();
        }
        // Under shuffle the first track is the one asked for and the rest
        // follow mixed; the order they came in is kept to put them back.
        let mut paths = paths;
        self.unshuffled.clone_from(&paths);
        if self.shuffle {
            shuffle::shuffle_after(&mut paths, 1, shuffle::seed());
        }
        // The player reads the queue it is given, so the interface keeps the
        // handle and edits it in place (T18).
        let queue = Arc::new(Mutex::new(paths.clone()));
        match Player::play_gapless_at_full(
            Arc::clone(&queue),
            0,
            Duration::ZERO,
            self.replay_gain,
            self.repeat,
            self.bit_perfect,
            self.resolver.clone(),
        ) {
            Ok(player) => {
                player.set_volume(self.volume);
                self.queue = paths;
                self.shared = Some(queue);
                self.player = Some(player);
                self.playing = Some(playback);
                self.state = PlayState::Playing;
                self.status.clear();
            }
            Err(error) => {
                self.queue.clear();
                self.shared = None;
                self.player = None;
                self.playing = None;
                self.state = PlayState::Stopped;
                self.status = format!("velo: {error}");
            }
        }
    }

    /// Moves the position by `delta_secs`: the current position plus the
    /// delta, clamped to the track, then the player rebuilt from that offset
    /// (T6). A few milliseconds of silence are expected; a click is not.
    fn seek_by(&mut self, delta_secs: i64) {
        // A live station has no position to seek within (S3): the seek path
        // says so instead of rebuilding the player — which would refetch the
        // stream — and never reaches the rebuild below.
        if matches!(self.playing_entry(), Some(Entry::Station { .. })) {
            "velo: nothing to seek in a live station".clone_into(&mut self.status);
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

    /// Moves to the next track, or back one (see [`skip_target`]), by
    /// rebuilding the player at that track the way a seek rebuilds it at an
    /// offset. Past the last track it says so instead of stopping the music.
    fn skip(&mut self, forward: bool) {
        let Some(player) = &self.player else {
            return;
        };
        let current = player.current_index();
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
        let Some(player) = self.player.take() else {
            return;
        };
        player.stop();

        if self.queue.is_empty() || track >= self.queue.len() {
            self.playing = None;
            self.state = PlayState::Stopped;
            return;
        }
        let queue = Arc::new(Mutex::new(self.queue.clone()));
        self.shared = Some(Arc::clone(&queue));
        match Player::play_gapless_at_full(
            queue,
            track,
            offset,
            self.replay_gain,
            self.repeat,
            self.bit_perfect,
            self.resolver.clone(),
        ) {
            Ok(player) => {
                // A rebuilt player starts at its own default level; without this
                // a seek would silently jump the volume back to it.
                player.set_volume(self.volume);
                self.player = Some(player);
                self.state = PlayState::Playing;
                self.status.clear();
            }
            Err(error) => {
                self.player = None;
                self.playing = None;
                self.state = PlayState::Stopped;
                self.status = format!("velo: {error}");
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

    fn shutdown(&mut self) {
        if let Some(player) = self.player.take() {
            player.stop();
        }
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

/// What one list row says, before it is laid out.
struct ListRow {
    playing: bool,
    number: usize,
    digits: usize,
    name: String,
    /// Dimmed after the name: a folder, or an artist and album.
    note: String,
    /// Dimmed against the right edge: a length, a year, counts.
    fact: String,
}

/// `▸ 3. Dawn  Tide · Harbor ……… 4:12`: the marker (`▶` in the accent on
/// the row that plays), the number, the name, the note, and the fact against
/// the right edge when it fits beside the rest; a fact that does not fit is
/// the part left out, never the name.
fn list_row(row: ListRow, width: u16, theme: &Theme) -> Line<'static> {
    let (marker, number, name) = if row.playing {
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
    let mut spans = vec![
        Span::styled(if row.playing { "▶ " } else { "▸ " }, marker),
        Span::styled(format!("{:>digits$}. ", row.number), number),
        Span::styled(row.name, name),
    ];
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
/// stopped. Hidden bars and still ones are drawn at ten.
const fn cadence(visible: bool, playing: bool, falling: bool) -> Duration {
    Duration::from_millis(if visible && (playing || falling) {
        33
    } else {
        100
    })
}

/// Partial blocks by eighths, empty to full.
const EIGHTHS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// The spectrum as `rows` lines, top first: one column per band and one of
/// air after it, each bar filling from the bottom in eighths of a row and
/// colored by height. A silent band keeps a baseline in the frame's color,
/// so the analyzer reads as there and quiet rather than missing.
fn spectrum_lines(levels: &[f32], rows: u16, theme: &Theme) -> Vec<Line<'static>> {
    let rows = usize::from(rows);
    let steps = rows * 8;
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
                    let cell = if here == 0 && from_bottom == 0 {
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
fn bar_color(from_bottom: usize, rows: usize, theme: &Theme) -> ratatui::style::Color {
    if from_bottom * 2 >= rows {
        theme.accent_bright
    } else {
        theme.accent
    }
}

/// The title and the dim subtitle of a playing file: its library row when
/// one of `known` has its path (`artist · album`), otherwise its file name
/// over its folder. Nothing is invented for a file the index never saw.
fn describe<'a>(path: &Path, known: impl IntoIterator<Item = &'a TrackEntry>) -> (String, String) {
    if let Some(entry) = known.into_iter().find(|entry| entry.path == path) {
        return (
            entry.title.clone(),
            format!("{} · {}", entry.artist, entry.album),
        );
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

fn mmss(duration: Duration) -> String {
    let seconds = duration.as_secs();
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

/// Every row of the help card: the key, then what it does.
const HELP_ROWS: [(&str, &str); 30] = [
    ("j / Down", "move down"),
    ("k / Up", "move up"),
    ("g", "first track"),
    ("G", "last track"),
    ("enter", "open, or play from the selection"),
    ("esc", "back one level / close help"),
    (
        "/",
        "search the whole library; in radio: filter what is loaded",
    ),
    ("space", "pause / resume"),
    ("← / →", "seek 5 s back / forward; shift: 30 s"),
    (", / .", "seek 5 s back / forward; < / >: 30 s"),
    ("n / p", "next / previous track"),
    ("s", "shuffle what is left of the queue"),
    ("r", "repeat: off, all, one"),
    ("+ / -", "volume up / down; [ / ] a point at a time"),
    ("?", "toggle this help"),
    ("o", "settings"),
    ("tab", "library / queue"),
    ("1 2 3 4", "artists, albums, tracks, genres"),
    ("L", "set the library folder"),
    ("u", "refresh the library index"),
    (
        "",
        "no library set: a prompt asks for its path, tab completes",
    ),
    ("a", "add the selection to the queue"),
    ("d", "remove from the queue"),
    ("J / K", "move it down / up"),
    ("R", "radio: the directory, your favourites, your stations"),
    ("f", "in radio: favourite a station, pin a place"),
    ("N", "in radio: the country list"),
    ("ctrl+r", "in radio: refetch the place you are in"),
    ("i", "in radio: report a station that cannot play"),
    ("q", "quit"),
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

/// The colour presets the settings card cycles a role through (S7): short,
/// distinct, readable on a dark terminal, with the defaults the file ships
/// with first so a cycled role can always be cycled back. `background` and
/// `text`, whose default is the terminal's own colour, also cycle `reset`
/// ahead of these.
const COLOR_PRESETS: [&str; 8] = [
    "#af00af", // the default accent — ANSI magenta
    "#5f87d7", // the default secondary — ANSI blue
    "#767676", // the default dim and border — bright black
    "#000000", // the default ink
    "#d70000", // red
    "#00af5f", // green
    "#d78700", // orange
    "#00d7d7", // cyan
];

/// One setting the card can change (S7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsKey {
    ReplayGain,
    Repeat,
    Shuffle,
    Accent,
    Text,
    Dim,
    Secondary,
    Border,
    Ink,
    Background,
    CoverArt,
    Spectrum,
    Hints,
    TopBar,
    Advisories,
    ViewNumbers,
    Folder,
}

impl SettingsKey {
    /// The setting's label on the card's left.
    fn label(self) -> &'static str {
        match self {
            Self::ReplayGain => "replay gain",
            Self::Repeat => "repeat",
            Self::Shuffle => "shuffle",
            Self::Accent => "accent",
            Self::Text => "text",
            Self::Dim => "dim",
            Self::Secondary => "secondary",
            Self::Border => "border",
            Self::Ink => "ink",
            Self::Background => "background",
            Self::CoverArt => "cover art",
            Self::Spectrum => "spectrum",
            Self::Hints => "hints",
            Self::TopBar => "top bar",
            Self::Advisories => "advisories",
            Self::ViewNumbers => "view numbers",
            Self::Folder => "folder",
        }
    }
}

/// One row of the settings card: a section header, or a setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsRow {
    Header(&'static str),
    Setting(SettingsKey),
}

/// The card's rows, in order (S7): the headers are never focused. The volume
/// is not a row (S7.3): it lives in `[state]` and on `+`/`-`, where it has
/// always been changed from the keyboard.
const SETTINGS_ROWS: [SettingsRow; 21] = [
    SettingsRow::Header("sound"),
    SettingsRow::Setting(SettingsKey::ReplayGain),
    SettingsRow::Setting(SettingsKey::Repeat),
    SettingsRow::Setting(SettingsKey::Shuffle),
    SettingsRow::Header("theme"),
    SettingsRow::Setting(SettingsKey::Accent),
    SettingsRow::Setting(SettingsKey::Text),
    SettingsRow::Setting(SettingsKey::Dim),
    SettingsRow::Setting(SettingsKey::Secondary),
    SettingsRow::Setting(SettingsKey::Border),
    SettingsRow::Setting(SettingsKey::Ink),
    SettingsRow::Setting(SettingsKey::Background),
    SettingsRow::Header("screen"),
    SettingsRow::Setting(SettingsKey::CoverArt),
    SettingsRow::Setting(SettingsKey::Spectrum),
    SettingsRow::Setting(SettingsKey::Hints),
    SettingsRow::Setting(SettingsKey::TopBar),
    SettingsRow::Setting(SettingsKey::Advisories),
    SettingsRow::Setting(SettingsKey::ViewNumbers),
    SettingsRow::Header("library"),
    SettingsRow::Setting(SettingsKey::Folder),
];

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

/// One colour role of the `[theme]` table the card can cycle (S7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColorRole {
    Accent,
    Text,
    Dim,
    Secondary,
    Border,
    Ink,
    Background,
}

impl ColorRole {
    /// The key the `[theme]` table writes the role under.
    fn key(self) -> &'static str {
        match self {
            Self::Accent => "accent",
            Self::Text => "text",
            Self::Dim => "dim",
            Self::Secondary => "secondary",
            Self::Border => "border",
            Self::Ink => "ink",
            Self::Background => "background",
        }
    }

    /// Whether the role may be `reset`, the terminal's own colour (S4):
    /// only the screen behind everything and the body text may.
    fn allows_reset(self) -> bool {
        matches!(self, Self::Background | Self::Text)
    }

    /// The role's colour in the palette the app draws with.
    fn get(self, colors: ThemeColors) -> ThemeColor {
        match self {
            Self::Accent => colors.accent,
            Self::Text => colors.text,
            Self::Dim => colors.dim,
            Self::Secondary => colors.secondary,
            Self::Border => colors.border,
            Self::Ink => colors.ink,
            Self::Background => colors.background,
        }
    }

    /// Replaces the role's colour in a palette.
    fn set(self, colors: &mut ThemeColors, colour: ThemeColor) {
        match self {
            Self::Accent => colors.accent = colour,
            Self::Text => colors.text = colour,
            Self::Dim => colors.dim = colour,
            Self::Secondary => colors.secondary = colour,
            Self::Border => colors.border = colour,
            Self::Ink => colors.ink = colour,
            Self::Background => colors.background = colour,
        }
    }
}

/// A colour as the `[theme]` table writes it (S4): `reset` or `#rrggbb`.
fn parse_color(text: &str) -> Option<ThemeColor> {
    if text == "reset" {
        return Some(ThemeColor::Terminal);
    }
    let hex = text.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some(ThemeColor::Rgb {
        red: u8::from_str_radix(&hex[0..2], 16).ok()?,
        green: u8::from_str_radix(&hex[2..4], 16).ok()?,
        blue: u8::from_str_radix(&hex[4..6], 16).ok()?,
    })
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

#[cfg(test)]
mod tests {
    use super::App;

    // The playing-entry refusals of T18b cannot be driven through `App::on_key`
    // without a sound device: marking an entry as playing means opening one.
    // The rule is pure and lives in one place that `remove_from_queue` and
    // `move_in_queue` both read, so it is pinned here, and the wiring from
    // `on_key` into `App::status` is pinned headlessly in `tests/ui_state.rs`
    // through the empty-queue refusal, which walks the same code path.

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
            super::describe(Path::new("/music/Tide/01.flac"), &known),
            ("Dawn".to_owned(), "Tide · Harbor".to_owned())
        );
        assert_eq!(
            super::describe(Path::new("/elsewhere/two.flac"), &known),
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
            super::cadence(true, true, false),
            fast,
            "visible and playing"
        );
        assert_eq!(
            super::cadence(true, false, true),
            fast,
            "visible, bars falling"
        );
        assert_eq!(
            super::cadence(false, true, false),
            slow,
            "playing, but hidden"
        );
        assert_eq!(super::cadence(false, false, true), slow, "hidden, falling");
        assert_eq!(
            super::cadence(true, false, false),
            slow,
            "visible and still"
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
}
