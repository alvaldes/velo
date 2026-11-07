//! The terminal interface: an [`App`] whose keymap is pure and testable, and a
//! `draw` that only renders it.
//!
//! Every state change answers through an [`Action`], so the key rules are
//! tested in `tests/ui_state.rs` without a terminal. Wiring the [`Player`] to
//! those actions happens in [`App::apply`], which is never exercised by tests.

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
    /// Pause or resume.
    TogglePause,
    /// The new volume, in percent.
    SetVolume(u8),
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

/// The whole interface state, minus the terminal.
pub struct App {
    tracks: Vec<PathBuf>,
    selected: usize,
    /// The track the running stream was started from, if any.
    playing_from: Option<usize>,
    help: bool,
    volume: u8,
    state: PlayState,
    player: Option<Player>,
    status: String,
}

impl App {
    /// An interface over the given tracks, selection at the top.
    #[must_use]
    pub fn new(tracks: Vec<PathBuf>) -> Self {
        Self {
            tracks,
            selected: 0,
            playing_from: None,
            help: false,
            volume: 100,
            state: PlayState::Stopped,
            player: None,
            status: String::new(),
        }
    }

    /// The index of the selected row.
    #[must_use]
    pub const fn selected(&self) -> usize {
        self.selected
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

    /// Applies one key press. Pure with respect to the terminal: it only
    /// moves state and answers with an [`Action`].
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
                self.selected = self.tracks.len().saturating_sub(1);
                Action::None
            }
            KeyCode::Enter if !self.tracks.is_empty() => Action::Play {
                from: self.selected,
            },
            KeyCode::Char(' ') => Action::TogglePause,
            KeyCode::Char('+' | '=') => Action::SetVolume(self.change_volume(5)),
            KeyCode::Char('-') => Action::SetVolume(self.change_volume(-5)),
            KeyCode::Char('?') => {
                self.help = !self.help;
                Action::None
            }
            KeyCode::Esc => {
                self.help = false;
                Action::None
            }
            KeyCode::Char('q') => Action::Quit,
            _ => Action::None,
        }
    }

    /// Carries an [`Action`] into the audio half. Errors land in the status
    /// message instead of leaving the interface.
    pub fn apply(&mut self, action: Action) {
        match action {
            Action::None | Action::SetVolume(_) => {
                // The volume is already stored; sending it to the device is
                // T6's transport work.
            }
            Action::Quit => self.shutdown(),
            Action::Play { from } => self.play_from(from),
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
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("velo", Style::new().fg(ACCENT).bold()),
                Span::styled(
                    format!("  {} tracks", self.tracks.len()),
                    Style::new().fg(SECONDARY),
                ),
            ])),
            left,
        );
        frame.render_widget(
            Paragraph::new(Span::styled(state, Style::new().fg(DIM))).alignment(Alignment::Right),
            right,
        );
    }

    fn draw_body(&self, frame: &mut Frame, area: Rect) {
        if self.tracks.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::styled(
                    "no tracks — pass files on the command line",
                    Style::new().fg(DIM),
                )),
                area,
            );
            return;
        }

        let items: Vec<ListItem> = self
            .tracks
            .iter()
            .enumerate()
            .map(|(index, path)| ListItem::new(self.track_row(index, path)))
            .collect();
        let list = List::new(items)
            .style(Style::new().bg(BACKGROUND))
            .highlight_style(Style::new().bg(ACCENT).fg(BACKGROUND));
        let mut state = ListState::default().with_selected(Some(self.selected));
        frame.render_stateful_widget(list, area, &mut state);
    }

    /// One row: the play marker, the file name, the parent directory dimmed.
    fn track_row(&self, index: usize, path: &std::path::Path) -> Line<'_> {
        let marker = if self.playing_from == Some(index) {
            Span::styled("▸ ", Style::new().fg(ACCENT))
        } else {
            Span::raw("  ")
        };

        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().to_string(),
        );
        let mut spans = vec![marker, Span::styled(name, Style::new().fg(TEXT))];

        if let Some(parent) = path.parent() {
            let parent = parent.display().to_string();
            if !parent.is_empty() {
                spans.push(Span::styled(format!("  {parent}"), Style::new().fg(DIM)));
            }
        }
        Line::from(spans)
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
                "j/k move  enter play  space pause  +/- volume  ? help  q quit",
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
            "enter      play from the selection",
            "space      pause / resume",
            "+ / -      volume up / down",
            "?          toggle this help",
            "esc        close help",
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

    fn select_next(&mut self) {
        if self.selected + 1 < self.tracks.len() {
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

    fn play_from(&mut self, from: usize) {
        if from >= self.tracks.len() {
            return;
        }
        // Dropping through `stop` also releases the producer thread.
        if let Some(old) = self.player.take() {
            old.stop();
        }
        match Player::play_gapless(self.tracks[from..].to_vec()) {
            Ok(player) => {
                self.player = Some(player);
                self.playing_from = Some(from);
                self.state = PlayState::Playing;
                self.status.clear();
            }
            Err(error) => {
                self.player = None;
                self.playing_from = None;
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

impl Default for App {
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
