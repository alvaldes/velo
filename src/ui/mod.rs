//! The terminal interface: an [`App`] whose keymap is pure and testable, and a
//! `draw` that only renders it.
//!
//! Every state change answers through an [`Action`], so the key rules are
//! tested in `tests/ui_state.rs` without a terminal. Wiring the [`Player`] to
//! those actions happens in [`App::apply`], which is never exercised by tests.
//!
//! `App` knows two ways to stand over a list of rows. `App::new` keeps the
//! flat list of files given on the command line. `App::browse` stands over the
//! library index instead: artists, then the albums of the selected artist,
//! then the tracks of the selected album. In browse mode the rows on screen
//! come from an in-memory snapshot that is refreshed when the position
//! changes, never once per frame.

pub mod artwork;
pub mod search;
pub mod theme;

pub use ratatui;
pub use ratatui::crossterm;

use std::cell::RefCell;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph};

use crate::audio::output::{SharedQueue, TrackFailure};
use crate::audio::{Player, seek_target};
use crate::config::ReplayGainMode;
use crate::library::{AlbumEntry, ArtistEntry, Library, TrackEntry};

use self::artwork::{Artwork, Picker, StatefulImage, StatefulProtocol};
use self::search::Search;
use self::theme::{ACCENT, BACKGROUND, DIM, SECONDARY, TEXT};

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
}

/// The stream that is playing, so rows can be marked with `▸`.
#[derive(Debug)]
enum Playback {
    /// A flat command-line stream started at this index.
    Files { from: usize },
    /// The queue itself, rotated so the entry it started from is its head.
    Queue,
    /// An album stream started at this index of that album's track list.
    Album {
        artist: String,
        album: String,
        from: usize,
    },
}

/// Which list the arrow keys and the editing commands act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    /// The library browser, or the flat command-line list.
    Library,
    /// The queue, in play order.
    Queue,
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
pub struct App<'lib> {
    position: Position,
    /// The files given on the command line, in [`Position::Files`].
    argv: Vec<PathBuf>,
    library: Option<&'lib Library>,
    artists: Vec<ArtistEntry>,
    albums: Vec<AlbumEntry>,
    tracks: Vec<TrackEntry>,
    selected: usize,
    /// The paths of the running stream, in queue order, so a seek can rebuild
    /// the player from the current track at an offset.
    queue: Vec<PathBuf>,
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
    help: bool,
    volume: u8,
    state: PlayState,
    player: Option<Player>,
    status: String,
}

impl<'lib> App<'lib> {
    /// An interface over the given files, selection at the top.
    #[must_use]
    pub fn new(tracks: Vec<PathBuf>) -> App<'static> {
        App {
            position: Position::Files,
            argv: tracks,
            library: None,
            artists: Vec::new(),
            albums: Vec::new(),
            tracks: Vec::new(),
            selected: 0,
            queue: Vec::new(),
            playing: None,
            view: View::Library,
            queue_selected: 0,
            shared: None,
            seen_area: std::cell::Cell::new(Rect::new(0, 0, 0, 0)),
            art_explained: false,
            replay_gain: ReplayGainMode::default(),
            search: None,
            search_pool: Vec::new(),
            search_haystacks: Vec::new(),
            search_results: Vec::new(),
            search_selected: 0,
            picker: None,
            artwork: None,
            artwork_path: None,
            protocol: RefCell::new(None),
            help: false,
            volume: 100,
            state: PlayState::Stopped,
            player: None,
            status: String::new(),
        }
    }

    /// An interface browsing the library index. The artists snapshot is read
    /// here, once; deeper levels are read when the position changes.
    #[must_use]
    pub fn browse(library: Option<&'lib Library>) -> Self {
        let mut app = Self {
            position: Position::Artists,
            argv: Vec::new(),
            library,
            artists: Vec::new(),
            albums: Vec::new(),
            tracks: Vec::new(),
            selected: 0,
            queue: Vec::new(),
            playing: None,
            view: View::Library,
            queue_selected: 0,
            shared: None,
            seen_area: std::cell::Cell::new(Rect::new(0, 0, 0, 0)),
            art_explained: false,
            replay_gain: ReplayGainMode::default(),
            search: None,
            search_pool: Vec::new(),
            search_haystacks: Vec::new(),
            search_results: Vec::new(),
            search_selected: 0,
            picker: None,
            artwork: None,
            artwork_path: None,
            protocol: RefCell::new(None),
            help: false,
            volume: 100,
            state: PlayState::Stopped,
            player: None,
            status: String::new(),
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

    /// The selected row of the queue view.
    #[must_use]
    pub const fn queue_selected(&self) -> usize {
        self.queue_selected
    }

    /// How `ReplayGain` is applied, as the config resolved it.
    pub const fn set_replay_gain(&mut self, mode: ReplayGainMode) {
        self.replay_gain = mode;
    }

    /// The level the config asks for. Applied to a running player and kept for
    /// the next one, so a stream built after a seek keeps it (T11).
    pub fn set_volume(&mut self, percent: u8) {
        self.volume = percent;
        if let Some(player) = &self.player {
            player.set_volume(percent);
        }
    }

    /// The queue, in play order.
    #[must_use]
    pub fn queue(&self) -> &[PathBuf] {
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
        }
    }

    /// The artists snapshot, empty when not browsing.
    #[must_use]
    pub fn artists(&self) -> &[ArtistEntry] {
        &self.artists
    }

    /// The albums of the artist being browsed, empty when not inside one.
    #[must_use]
    pub fn albums(&self) -> &[AlbumEntry] {
        &self.albums
    }

    /// The tracks of the album being browsed, empty when not inside one.
    #[must_use]
    pub fn tracks(&self) -> &[TrackEntry] {
        &self.tracks
    }

    /// Whether the help overlay is open.
    #[must_use]
    pub const fn help_open(&self) -> bool {
        self.help
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

    /// Runs once per turn of the event loop — the same cadence as key
    /// polling — so the producer's announcements surface without a key
    /// press, and the cover art follows the track it plays (T8). Without a
    /// player there is nothing to collect.
    pub fn tick(&mut self) {
        self.refresh_artwork();
        self.explain_hidden_art();
        let Some(player) = &self.player else {
            return;
        };
        let Some(failure) = player.take_failure() else {
            return;
        };
        self.announce_failure(&failure);
    }

    /// Puts a skipped track into the status line: `velo: skipped <file name>:
    /// <why>`. The message stays until the next status update — a play, a
    /// seek or another failure — because a skipped track is not a sequence
    /// error and nothing else has happened worth saying (T19).
    pub fn announce_failure(&mut self, failure: &TrackFailure) {
        let name = failure.path.file_name().map_or_else(
            || failure.path.display().to_string(),
            |name| name.to_string_lossy().to_string(),
        );
        self.status = format!("velo: skipped {name}: {}", failure.message);
    }

    /// Swaps the decoded cover when the track that is playing changes, and
    /// only then: the file is read once per track, never per frame, and never
    /// from the audio callback (T8). A track whose file carries no picture
    /// draws no art until a different track plays.
    /// Tells the user once why a cover they own is not on screen, instead of
    /// leaving them staring at a panel that never appears (T8).
    fn explain_hidden_art(&mut self) {
        if self.art_explained || !self.status.is_empty() {
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
        self.artwork = path.as_deref().and_then(Artwork::from_path);
        self.artwork_path = path;
        // A new track deserves its own explanation.
        self.art_explained = false;
    }

    /// Re-reads the artists snapshot from the index, e.g. right after
    /// indexing. Keeps the selection inside the (possibly shorter) list.
    pub fn reload(&mut self) {
        let Some(library) = self.library else {
            return;
        };
        match library.artists() {
            Ok(artists) => self.artists = artists,
            Err(error) => self.status = format!("velo: {error}"),
        }
        self.selected = self.selected.min(self.row_count().saturating_sub(1));
    }

    /// Applies one key press. Pure with respect to the terminal and the
    /// library: it only moves state and answers with an [`Action`].
    pub fn on_key(&mut self, event: ratatui::crossterm::event::KeyEvent) -> Action {
        use ratatui::crossterm::event::KeyCode;

        // An open search owns almost every key: `j`/`k` move through the
        // results, printable characters type in, `Esc` cancels. The browse
        // state is never touched while it is open.
        if self.search.is_some() {
            return self.on_search_key(event.code);
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
            KeyCode::Enter => match &self.position {
                Position::Files if !self.argv.is_empty() => Action::Play {
                    from: self.selected,
                },
                Position::Tracks { .. } if !self.tracks.is_empty() => Action::Play {
                    from: self.selected,
                },
                Position::Artists if !self.artists.is_empty() => Action::Descend,
                Position::Albums { .. } if !self.albums.is_empty() => Action::Descend,
                _ => Action::None,
            },
            KeyCode::Char(' ') => Action::TogglePause,
            KeyCode::Char('+' | '=') => Action::SetVolume(self.change_volume(5)),
            KeyCode::Char('-') => Action::SetVolume(self.change_volume(-5)),
            KeyCode::Left | KeyCode::Char(',') => Action::Seek { delta_secs: -5 },
            KeyCode::Right | KeyCode::Char('.') => Action::Seek { delta_secs: 5 },
            KeyCode::Char('?') => {
                self.help = !self.help;
                Action::None
            }
            KeyCode::Char('/') if self.library.is_some() => {
                self.open_search();
                Action::None
            }
            KeyCode::Esc | KeyCode::Backspace => {
                // `Esc` closes the help overlay when it is open, before it
                // would ascend; ascending is browse-only.
                self.help = false;
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

    /// Carries an [`Action`] into the audio or library half. Errors land in
    /// the status message instead of leaving the interface.
    pub fn apply(&mut self, action: Action) {
        match action {
            Action::None => {}
            Action::SetVolume(percent) => {
                self.volume = percent;
                if let Some(player) = &self.player {
                    player.set_volume(percent);
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
        }
    }

    /// Renders the interface. Reads state, changes nothing.
    pub fn draw(&self, frame: &mut Frame) {
        let area = frame.area();
        self.seen_area.set(area);
        frame.render_widget(Block::new().style(Style::new().bg(BACKGROUND)), area);

        let [header, body, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .areas(area);
        // Cover art (T8): when a protocol was detected, the art is set to draw
        // and the terminal meets the threshold, it takes the rightmost columns
        // of the header and the body, and those make way. Otherwise the frame
        // is exactly what it is without art — nothing to fake, nothing to move.
        let pane = self
            .picker
            .filter(|_| self.artwork.is_some())
            .and_then(|_| artwork::pane(area));
        let mut header = header;
        let mut body = body;
        if let Some(art) = pane {
            header.width = header.width.saturating_sub(art.width);
            body.width = body.width.saturating_sub(art.width);
        }
        self.draw_header(frame, header);
        self.draw_body(frame, body);
        self.draw_footer(frame, footer);
        self.draw_artwork(frame, pane);
        if self.help {
            Self::draw_help(frame, area);
        }
    }

    /// Renders the cover beside the header, fitting the source image into the
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

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let state = self.state.label();
        let [left, right] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(10)]).areas(area);
        let mut spans = vec![Span::styled("velo", Style::new().fg(ACCENT).bold())];
        match &self.position {
            Position::Files => {
                spans.push(Span::styled(
                    format!("  {}", plural(self.argv.len(), "track", "tracks")),
                    Style::new().fg(SECONDARY),
                ));
            }
            Position::Artists => {
                spans.push(Span::styled("  artists", Style::new().fg(SECONDARY)));
                spans.push(Span::styled(
                    format!("  {}", self.artists.len()),
                    Style::new().fg(SECONDARY),
                ));
            }
            Position::Albums { artist } => {
                spans.push(Span::styled(
                    format!("  {artist}"),
                    Style::new().fg(SECONDARY),
                ));
                spans.push(Span::styled(
                    format!("  {}", plural(self.albums.len(), "album", "albums")),
                    Style::new().fg(SECONDARY),
                ));
            }
            Position::Tracks { artist, album } => {
                spans.push(Span::styled(
                    format!("  {artist} · {album}"),
                    Style::new().fg(SECONDARY),
                ));
                spans.push(Span::styled(
                    format!("  {}", plural(self.tracks.len(), "track", "tracks")),
                    Style::new().fg(SECONDARY),
                ));
            }
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), left);
        frame.render_widget(
            Paragraph::new(Span::styled(state, Style::new().fg(DIM))).alignment(Alignment::Right),
            right,
        );
    }

    fn draw_body(&self, frame: &mut Frame, area: Rect) {
        if self.search.is_some() {
            self.draw_search(frame, area);
            return;
        }
        if self.view == View::Queue {
            if self.queue.is_empty() {
                frame.render_widget(
                    Paragraph::new(Line::styled(
                        "nothing queued — press a on a track to add it",
                        Style::new().fg(DIM),
                    )),
                    area,
                );
                return;
            }
            let list = List::new(self.queue_rows())
                .style(Style::new().bg(BACKGROUND))
                .highlight_style(Style::new().bg(ACCENT).fg(BACKGROUND));
            let mut state = ListState::default().with_selected(Some(self.queue_selected));
            frame.render_stateful_widget(list, area, &mut state);
            return;
        }
        if let Some(message) = self.empty_message() {
            frame.render_widget(
                Paragraph::new(Line::styled(message, Style::new().fg(DIM))),
                area,
            );
            return;
        }

        let list = List::new(self.rows())
            .style(Style::new().bg(BACKGROUND))
            .highlight_style(Style::new().bg(ACCENT).fg(BACKGROUND));
        let mut state = ListState::default().with_selected(Some(self.selected));
        frame.render_stateful_widget(list, area, &mut state);
    }

    /// The rows of the current level, as they are on screen.
    fn rows(&self) -> Vec<ListItem<'_>> {
        match &self.position {
            Position::Files => self
                .argv
                .iter()
                .enumerate()
                .map(|(index, path)| ListItem::new(self.file_row(index, path)))
                .collect(),
            Position::Artists => self
                .artists
                .iter()
                .enumerate()
                .map(|(index, entry)| {
                    ListItem::new(Line::from(vec![
                        self.marker(index),
                        Span::styled(entry.name.clone(), Style::new().fg(TEXT)),
                        Span::styled(
                            format!(
                                "  {} · {}",
                                plural(
                                    usize::try_from(entry.albums).unwrap_or(usize::MAX),
                                    "album",
                                    "albums"
                                ),
                                plural(
                                    usize::try_from(entry.tracks).unwrap_or(usize::MAX),
                                    "track",
                                    "tracks"
                                )
                            ),
                            Style::new().fg(DIM),
                        ),
                    ]))
                })
                .collect(),
            Position::Albums { .. } => self
                .albums
                .iter()
                .enumerate()
                .map(|(index, entry)| {
                    let year = entry
                        .year
                        .map_or_else(String::new, |year| format!("  {year}"));
                    ListItem::new(Line::from(vec![
                        self.marker(index),
                        Span::styled(entry.title.clone(), Style::new().fg(TEXT)),
                        Span::styled(year, Style::new().fg(DIM)),
                    ]))
                })
                .collect(),
            Position::Tracks { .. } => self
                .tracks
                .iter()
                .enumerate()
                .map(|(index, entry)| {
                    ListItem::new(Line::from(vec![
                        self.marker(index),
                        Span::styled(entry.title.clone(), Style::new().fg(TEXT)),
                        Span::styled(Self::duration(entry.duration_ms), Style::new().fg(DIM)),
                    ]))
                })
                .collect(),
        }
    }

    /// The queue, in play order, marked the way every other list marks the row
    /// that is playing.
    fn queue_rows(&self) -> Vec<ListItem<'static>> {
        let playing = self.playing_queue_index();
        self.queue
            .iter()
            .enumerate()
            .map(|(index, path)| {
                let marker = if playing == Some(index) {
                    Span::styled("▸ ", Style::new().fg(ACCENT))
                } else {
                    Span::raw("  ")
                };
                let mut spans = vec![
                    marker,
                    Span::styled(Self::file_name(path), Style::new().fg(TEXT)),
                ];
                if let Some(parent) = path.parent() {
                    spans.push(Span::styled(
                        format!("  {}", parent.display()),
                        Style::new().fg(DIM),
                    ));
                }
                ListItem::new(Line::from(spans))
            })
            .collect()
    }

    /// One flat row: the play marker, the file name, the parent directory dimmed.
    fn file_row(&self, index: usize, path: &std::path::Path) -> Line<'_> {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().to_string(),
        );
        let mut spans = vec![
            self.marker(index),
            Span::styled(name, Style::new().fg(TEXT)),
        ];

        if let Some(parent) = path.parent() {
            let parent = parent.display().to_string();
            if !parent.is_empty() {
                spans.push(Span::styled(format!("  {parent}"), Style::new().fg(DIM)));
            }
        }
        Line::from(spans)
    }

    /// The play marker: `▸` on the row whose samples are being produced.
    fn marker(&self, index: usize) -> Span<'static> {
        if self.playing_row() == Some(index) {
            Span::styled("▸ ", Style::new().fg(ACCENT))
        } else {
            Span::raw("  ")
        }
    }

    /// A duration in `m:ss`, empty for tracks with no known length.
    fn duration(duration_ms: i64) -> String {
        if duration_ms <= 0 {
            return String::new();
        }
        let seconds = u64::try_from(duration_ms / 1000).unwrap_or(0);
        format!("  {}:{:02}", seconds / 60, seconds % 60)
    }

    fn draw_footer(&self, frame: &mut Frame, area: Rect) {
        let [status_row, hints_row] =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);

        let status = if self.search.is_some() {
            // The query itself, over the transport line, so the user sees what
            // they are typing while the body lists the results.
            Line::styled(format!("/{}", self.search_query()), Style::new().fg(ACCENT))
        } else if self.status.is_empty() {
            Line::styled(self.transport_line(), Style::new().fg(ACCENT))
        } else {
            Line::styled(self.status.clone(), Style::new().fg(ACCENT))
        };
        if !status.spans.is_empty() {
            frame.render_widget(Paragraph::new(status), status_row);
        }

        let hints = if self.search.is_some() {
            "type to search  j/k move  enter play  backspace delete  esc cancel"
        } else {
            "j/k move  enter play  space pause  ←/→ seek  / search  tab queue  ? help  q quit"
        };
        frame.render_widget(
            Paragraph::new(Line::styled(hints, Style::new().fg(DIM))),
            hints_row,
        );
    }

    /// The search results: a track title with its artist and album dimmed,
    /// and the playing marker drawn the way every other list draws it.
    fn draw_search(&self, frame: &mut Frame, area: Rect) {
        if self.search_results.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::styled("no tracks match", Style::new().fg(DIM))),
                area,
            );
            return;
        }
        let playing = self.playing_path();
        let items: Vec<ListItem<'_>> = self
            .search_results
            .iter()
            .filter_map(|&index| self.search_pool.get(index))
            .map(|entry| {
                let marker = if playing == Some(entry.path.as_path()) {
                    Span::styled("▸ ", Style::new().fg(ACCENT))
                } else {
                    Span::raw("  ")
                };
                ListItem::new(Line::from(vec![
                    marker,
                    Span::styled(entry.title.clone(), Style::new().fg(TEXT)),
                    Span::styled(
                        format!("  {} · {}", entry.artist, entry.album),
                        Style::new().fg(DIM),
                    ),
                ]))
            })
            .collect();
        let list = List::new(items)
            .style(Style::new().bg(BACKGROUND))
            .highlight_style(Style::new().bg(ACCENT).fg(BACKGROUND));
        let mut state = ListState::default().with_selected(Some(self.search_selected));
        frame.render_stateful_widget(list, area, &mut state);
    }

    /// The path of the file whose samples are being produced, if any: the
    /// marker has to find the playing row wherever a list shows whole tracks.
    fn playing_path(&self) -> Option<&std::path::Path> {
        let player = self.player.as_ref()?;
        self.queue
            .get(player.current_index())
            .map(std::path::PathBuf::as_path)
    }

    /// The transport line: `mm:ss / mm:ss` when the track's length is known,
    /// then the volume percentage.
    fn transport_line(&self) -> String {
        let mut line = String::new();
        if self.player.is_some() {
            let position = self
                .player
                .as_ref()
                .map_or(Duration::ZERO, Player::position);
            line.push_str(&mmss(position));
            if let Some(total) = self.current_track_duration() {
                line.push_str(" / ");
                line.push_str(&mmss(total));
            }
            line.push_str("  ");
        }
        // Once a player exists it owns the level: showing a mirrored copy is
        // exactly how a rebuild's default volume went unnoticed and unheard.
        let volume = self.player.as_ref().map_or(self.volume, Player::volume);
        let _ = write!(line, "vol {volume}%");
        line
    }

    /// The length of the track that is playing: from the interface's own track
    /// row when the queue came from the library, otherwise from the decoder,
    /// which has always known it. Without the fallback a command-line queue
    /// showed a bare clock that a seek could push past the end of the file.
    fn current_track_duration(&self) -> Option<Duration> {
        let player = self.player.as_ref()?;
        let from_row = match (&self.playing, &self.position) {
            (Some(Playback::Album { from, .. }), Position::Tracks { .. }) => {
                let entry = self.tracks.get(from + player.current_index())?;
                let millis = u64::try_from(entry.duration_ms).ok()?;
                (millis > 0).then(|| Duration::from_millis(millis))
            }
            _ => None,
        };
        from_row.or_else(|| player.current_duration())
    }

    fn draw_help(frame: &mut Frame, area: Rect) {
        let rows = [
            "j / Down   move down",
            "k / Up     move up",
            "g          first track",
            "G          last track",
            "enter      open, or play from the selection",
            "esc        back one level / close help",
            "/          search the whole library",
            "space      pause / resume",
            "← / →      seek 5 s back / forward",
            ", / .      seek 5 s back / forward",
            "+ / -      volume up / down",
            "?          toggle this help",
            "tab        library / queue",
            "a          add the selection to the queue",
            "d          remove from the queue",
            "J / K      move it down / up",
            "q          quit",
        ];
        let widest = rows.iter().map(|row| row.len()).max().unwrap_or(0);
        let width = u16::try_from(widest + 4).unwrap_or(area.width);
        let height = u16::try_from(rows.len() + 2).unwrap_or(area.height);
        let box_area = centered(area, width, height);

        frame.render_widget(Clear, box_area);
        let text: Vec<Line> = rows
            .iter()
            .map(|row| Line::styled((*row).to_string(), Style::new().fg(TEXT)))
            .collect();
        frame.render_widget(
            Paragraph::new(text)
                .style(Style::new().bg(BACKGROUND))
                .block(
                    Block::bordered()
                        .border_style(Style::new().fg(ACCENT))
                        .style(Style::new().bg(BACKGROUND)),
                ),
            box_area,
        );
    }

    /// How many rows the current level has.
    fn row_count(&self) -> usize {
        match &self.position {
            Position::Files => self.argv.len(),
            Position::Artists => self.artists.len(),
            Position::Albums { .. } => self.albums.len(),
            Position::Tracks { .. } => self.tracks.len(),
        }
    }

    /// What an empty level says about itself, if it is empty.
    fn empty_message(&self) -> Option<&'static str> {
        match &self.position {
            Position::Files if self.argv.is_empty() => {
                Some("no tracks — pass files on the command line")
            }
            Position::Artists if self.artists.is_empty() => Some("no artists in the index yet"),
            Position::Albums { .. } if self.albums.is_empty() => Some("no albums for this artist"),
            Position::Tracks { .. } if self.tracks.is_empty() => Some("no tracks in this album"),
            _ => None,
        }
    }

    /// The list position of the row whose samples are being produced: where
    /// the stream was started plus the producer's track index.
    fn playing_row(&self) -> Option<usize> {
        let produced = self.player.as_ref().map_or(0, Player::current_index);
        match (&self.playing, &self.position) {
            (Some(Playback::Files { from }), Position::Files) => Some(from + produced),
            (
                Some(Playback::Album {
                    artist,
                    album,
                    from,
                }),
                Position::Tracks {
                    artist: level_artist,
                    album: level_album,
                },
            ) if artist == level_artist && album == level_album => Some(from + produced),
            _ => None,
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
                self.search_haystacks = pool
                    .iter()
                    .map(|entry| format!("{} {} {}", entry.artist, entry.album, entry.title))
                    .collect();
                self.search_pool = pool;
                self.search_results = (0..self.search_pool.len()).collect();
                self.search_selected = 0;
                self.search = Some(Search::new());
            }
            Err(error) => self.status = format!("velo: {error}"),
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
    /// interface returns to exactly where it was.
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
        self.start_stream(paths, Playback::Queue);
    }

    /// How many rows the view the keys act on holds.
    fn view_len(&self) -> usize {
        match self.view {
            View::Library => self.row_count(),
            View::Queue => self.queue.len(),
        }
    }

    /// The selected row of the view the keys act on.
    const fn view_selection(&self) -> usize {
        match self.view {
            View::Library => self.selected,
            View::Queue => self.queue_selected,
        }
    }

    fn set_view_selection(&mut self, index: usize) {
        match self.view {
            View::Library => self.selected = index,
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

    /// Switches between the library and the queue, naming the new view in the
    /// status line so the screen says where the keys went.
    fn toggle_view(&mut self) {
        self.view = match self.view {
            View::Library => View::Queue,
            View::Queue => View::Library,
        };
        match self.view {
            View::Queue => {
                self.queue_selected = self.queue_selected.min(self.queue.len().saturating_sub(1));
                self.status = format!(
                    "velo: queue, {}",
                    plural(self.queue.len(), "track", "tracks")
                );
            }
            View::Library => self.status.clear(),
        }
    }

    /// The queue index that is playing, when anything is: the player counts
    /// from the list it was given, which is this queue.
    fn playing_queue_index(&self) -> Option<usize> {
        self.playing.as_ref()?;
        self.player.as_ref().map(Player::current_index)
    }

    /// Applies an edit to the queue and to the handle the player reads, in that
    /// order and in one place: two call sites that had to agree would drift.
    fn edit_queue(&mut self, edit: impl FnOnce(&mut Vec<PathBuf>)) {
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
        let name = Self::file_name(&self.queue[index]);
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
    /// playing. A track appends itself; an album appends its tracks.
    fn append_selection(&mut self) {
        let paths: Vec<PathBuf> = match (&self.position, self.view) {
            (Position::Tracks { .. }, _) => self
                .tracks
                .get(self.selected)
                .map(|entry| vec![entry.path.clone()])
                .unwrap_or_default(),
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
            (Position::Artists, _) => {
                "velo: open an album to add its tracks".clone_into(&mut self.status);
                return;
            }
        };
        if paths.is_empty() {
            return;
        }
        let count = paths.len();
        self.edit_queue(|queue| queue.extend(paths));
        self.status = format!("velo: added {}", plural(count, "track", "tracks"));
    }

    fn file_name(path: &std::path::Path) -> String {
        path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
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
            // `Files` does not descend; `Tracks` is the deepest level and
            // answers with `Action::Play` instead.
            Position::Files | Position::Tracks { .. } => {}
        }
    }

    /// Goes one level up. The parent's row snapshot is kept as it was when
    /// the level was entered — no query runs on ascent.
    fn ascend(&mut self) {
        match &self.position {
            Position::Files | Position::Artists => {
                // Already at the top: ascending stays at the artists.
            }
            Position::Albums { .. } => {
                self.position = Position::Artists;
                self.selected = 0;
            }
            Position::Tracks { artist, .. } => {
                self.position = Position::Albums {
                    artist: artist.clone(),
                };
                self.selected = 0;
            }
        }
    }

    /// Starts a gapless stream. On the tracks level this queues the whole
    /// album from the selection onwards; a track that cannot play is reported
    /// in the status line instead of exiting.
    fn play_from(&mut self, from: usize) {
        let started = match &self.position {
            Position::Files if from < self.argv.len() => {
                Some((self.argv.clone(), Playback::Files { from }))
            }
            Position::Tracks { artist, album } if from < self.tracks.len() => Some((
                self.tracks[from..]
                    .iter()
                    .map(|entry| entry.path.clone())
                    .collect(),
                Playback::Album {
                    artist: artist.clone(),
                    album: album.clone(),
                    from,
                },
            )),
            _ => None,
        };
        let Some((paths, playback)) = started else {
            return;
        };
        self.start_stream(paths, playback);
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

    /// Opens a device for these paths and makes them the queue.
    fn start_stream(&mut self, paths: Vec<PathBuf>, playback: Playback) {
        // Dropping through `stop` also releases the producer thread.
        if let Some(old) = self.player.take() {
            old.stop();
        }
        // The player reads the queue it is given, so the interface keeps the
        // handle and edits it in place (T18).
        let queue = Arc::new(Mutex::new(paths.clone()));
        match Player::play_gapless_with_gain(Arc::clone(&queue), self.replay_gain) {
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
        let Some(player) = &self.player else {
            return;
        };
        let position = player.position();
        let duration = self.current_track_duration();
        let target = seek_target(position, delta_secs, duration);
        self.start_at(target);
    }

    /// Closes the player and starts it again at the same track, `offset` into
    /// it. The queue and the marker stay as they were.
    fn start_at(&mut self, offset: Duration) {
        let Some(player) = self.player.take() else {
            return;
        };
        let track = player.current_index();
        player.stop();

        if self.queue.is_empty() || track >= self.queue.len() {
            self.playing = None;
            self.state = PlayState::Stopped;
            return;
        }
        let queue = Arc::new(Mutex::new(self.queue.clone()));
        self.shared = Some(Arc::clone(&queue));
        match Player::play_gapless_at_with_gain(queue, track, offset, self.replay_gain) {
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
fn mmss(duration: Duration) -> String {
    let seconds = duration.as_secs();
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
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
