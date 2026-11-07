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

pub mod theme;

pub use ratatui;
pub use ratatui::crossterm;

use std::path::PathBuf;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph};

use crate::audio::Player;
use crate::library::{AlbumEntry, ArtistEntry, Library, TrackEntry};

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
    /// Go one level deeper into the current selection.
    Descend,
    /// Go one level up; at the top level this does nothing.
    Ascend,
    /// Pause or resume.
    TogglePause,
    /// The new volume, in percent.
    SetVolume(u8),
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
    /// An album stream started at this index of that album's track list.
    Album {
        artist: String,
        album: String,
        from: usize,
    },
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
    /// The stream that is playing, if any.
    playing: Option<Playback>,
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
            playing: None,
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
            playing: None,
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
                self.selected = 0;
                Action::None
            }
            KeyCode::Char('G') => {
                self.selected = self.row_count().saturating_sub(1);
                Action::None
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
            KeyCode::Char('?') => {
                self.help = !self.help;
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
            Action::None | Action::SetVolume(_) => {
                // The volume is already stored; sending it to the device is
                // T6's transport work.
            }
            Action::Quit => self.shutdown(),
            Action::Play { from } => self.play_from(from),
            Action::Descend => self.descend(),
            Action::Ascend => self.ascend(),
            Action::TogglePause => self.toggle_pause(),
        }
    }

    /// Renders the interface. Reads state, changes nothing.
    pub fn draw(&self, frame: &mut Frame) {
        let area = frame.area();
        frame.render_widget(Block::new().style(Style::new().bg(BACKGROUND)), area);

        let [header, body, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .areas(area);
        self.draw_header(frame, header);
        self.draw_body(frame, body);
        self.draw_footer(frame, footer);
        if self.help {
            Self::draw_help(frame, area);
        }
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

        if !self.status.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::styled(self.status.clone(), Style::new().fg(ACCENT))),
                status_row,
            );
        }

        frame.render_widget(
            Paragraph::new(Line::styled(
                "j/k move  enter open/play  esc back  space pause  +/- volume  ? help  q quit",
                Style::new().fg(DIM),
            )),
            hints_row,
        );
    }

    fn draw_help(frame: &mut Frame, area: Rect) {
        let rows = [
            "j / Down   move down",
            "k / Up     move up",
            "g          first track",
            "G          last track",
            "enter      open, or play from the selection",
            "esc        back one level / close help",
            "space      pause / resume",
            "+ / -      volume up / down",
            "?          toggle this help",
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

    fn select_next(&mut self) {
        if self.selected + 1 < self.row_count() {
            self.selected += 1;
        }
    }

    fn select_previous(&mut self) {
        self.selected = self.selected.saturating_sub(1);
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
                Some((self.argv[from..].to_vec(), Playback::Files { from }))
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

        // Dropping through `stop` also releases the producer thread.
        if let Some(old) = self.player.take() {
            old.stop();
        }
        match Player::play_gapless(paths) {
            Ok(player) => {
                self.player = Some(player);
                self.playing = Some(playback);
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
