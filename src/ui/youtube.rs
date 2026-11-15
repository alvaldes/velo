//! The `YouTube` search screen (issue #70, T5): one query line, the results
//! the tool answers, and nothing kept anywhere.
//!
//! The shape is the tree's own: the screen is a state machine like the radio
//! view — it never touches the network and never plays on its own, it holds
//! the query, the cursor and the answers, and its `esc` answers
//! [`Outcome::Close`] to the shell that owns the queue. The search itself
//! follows the wiring every background request follows (
//! [`crate::ui::radio_data`], the library index): one request runs on its
//! own thread and hands its outcome back through a channel the interface
//! polls once per tick. One request is in flight at a time; while it runs
//! the status line names what is being looked for, and a finished or failed
//! answer lands without a key press.
//!
//! Nothing about the search is stored (S7): the query and the answers live
//! in memory only, die with the process, and never reach the config, the
//! history or a log. Closing the screen drops them outright.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};

use crate::youtube::search::{self, Found};
use crate::youtube::tool::ToolError;

use super::rowtext::station_label;
use super::theme::Theme;
use super::{ListRow, PlayState, RowMark, digits, draw_empty, list_row};

/// What one key press asked the shell to do with the search screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The key moved the screen's own state; the shell only redraws.
    Handled,
    /// The key is not the screen's: it falls through to the keymap, the way
    /// the sources screen lets the transport's keys through.
    Unhandled,
    /// `esc`: the screen closes and the answers it held are dropped.
    Close,
    /// `enter` on the query row: the query the screen holds is to be
    /// searched. The shell carries it to the tool off the event loop.
    Search(String),
    /// `enter` on a result: it is queued as one [`Entry::Youtube`] and
    /// played from its own row, the way `enter` on a queue row plays from
    /// that row — unless the result is a live broadcast, which is refused
    /// with a sentence instead of a queue entry that would fail at open
    /// time. The shell carries both out; the screen holds no queue.
    Play(Found),
}

/// What one finished background job hands back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// The tool failed: its own sentence, one line.
    Failed(String),
    /// The tool answered nothing usable: a query with no results.
    Empty,
    /// The tool answered results, now held by the screen's list.
    Filled,
    /// The one video a URL or a bare id described: the shell queues exactly
    /// one [`Entry::Youtube`] and plays it the way a chosen result plays;
    /// the screen holds no queue and never touches one.
    Described(Found),
}

/// One background job in flight until its answer is polled: a search, or
/// the description of the one video a URL or bare id names. One at a time.
enum Pending {
    /// A search: the query it was asked for, and its answer.
    Search {
        query: String,
        answer: Receiver<Result<Vec<Found>, ToolError>>,
    },
    /// A description: the text it was asked about, and its answer.
    Describe {
        input: String,
        answer: Receiver<Result<Found, ToolError>>,
    },
}

impl Pending {
    /// What the status line names while this job runs: the query or the
    /// input it was asked for, and nothing else.
    fn subject(&self) -> &str {
        match self {
            Self::Search { query, .. } => query,
            Self::Describe { input, .. } => input,
        }
    }
}

/// The screen's own state: the query being typed, the cursor — the query row
/// is the list's first row, the results follow it — and the answers of the
/// last search. `program` is the tool the launch found; the screen exists
/// only when there is one.
pub struct Search {
    /// The `yt-dlp` this machine has: the worker spawns it and nothing else
    /// does.
    program: PathBuf,
    /// What is typed. Typed characters land here and the cursor comes back
    /// to the query row, so `enter` after typing always searches and never
    /// plays a row of an answer that no longer belongs to the query.
    query: String,
    /// The row the cursor is on: the query row is row 0, the results follow.
    cursor: usize,
    /// Whether the query is what is being typed into. The screen opens
    /// typing, `/` (or `i`) returns to it, and anything that moves the
    /// focus away from the query row — a started search, the arrow keys —
    /// leaves it: one mode at a time, so the keyboard has one meaning.
    typing: bool,
    /// The last search's answers, in the order the tool answered them.
    results: Vec<Found>,
    /// The job in flight, its answer still to be polled. One at a time:
    /// `enter` while one runs is spent on the running one.
    pending: Option<Pending>,
}

impl Search {
    /// A screen over the tool the launch found: query and answers empty.
    #[must_use]
    pub fn new(program: PathBuf) -> Self {
        Self {
            program,
            query: String::new(),
            cursor: 0,
            typing: true,
            results: Vec::new(),
            pending: None,
        }
    }

    /// What is typed, for the tests and the header.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Whether the query is what is being typed into: the shell reads it
    /// for the footer's keys, the tests read it to pin the mode model.
    #[must_use]
    pub fn typing(&self) -> bool {
        self.typing
    }

    /// The answers on the list, in the order the tool answered them.
    #[must_use]
    pub fn results(&self) -> &[Found] {
        &self.results
    }

    /// The query a running job was asked for, while one runs: what the
    /// status line names, never a stale one.
    #[must_use]
    pub fn searching_query(&self) -> Option<&str> {
        self.pending.as_ref().map(Pending::subject)
    }

    /// Starts one search on its own thread (S6): the tool runs off the event
    /// loop bounded by its own search deadline, the interface keeps
    /// answering keys, and the answer lands through the tick. Refused while
    /// one is already in flight — one request at a time, the rule every
    /// other wired view keeps; the running one's deadline is its end.
    /// Answers `false` when the ask was refused. A started search is what
    /// enters the browsing mode: the keys stop typing and start moving.
    pub fn search(&mut self, query: String) -> bool {
        if self.pending.is_some() {
            return false;
        }
        let program = self.program.clone();
        let (sender, answer) = mpsc::channel();
        let asked = query.clone();
        thread::spawn(move || {
            // A dropped receiver — a screen closed mid-search — answers into
            // nothing and is gone; the deadline ended the child already.
            let _ = sender.send(search::run(&program, &query));
        });
        self.pending = Some(Pending::Search {
            query: asked,
            answer,
        });
        self.typing = false;
        true
    }

    /// Describes the one video a URL or a bare id names, on the same worker
    /// and under the same rules as [`Self::search`]: off the event loop,
    /// bounded by the resolve deadline, one job at a time, the answer
    /// landing through the tick. Answers `false` when refused.
    pub fn describe(&mut self, input: String) -> bool {
        if self.pending.is_some() {
            return false;
        }
        let program = self.program.clone();
        let (sender, answer) = mpsc::channel();
        let asked = input.clone();
        thread::spawn(move || {
            let _ = sender.send(search::describe(&program, &input));
        });
        self.pending = Some(Pending::Describe {
            input: asked,
            answer,
        });
        self.typing = false;
        true
    }

    /// Collects a finished job, once per tick: still running is no news, a
    /// search's answer lands as [`Answer::Filled`] with the results held
    /// here, a query with nothing behind it as [`Answer::Empty`], a
    /// description's as [`Answer::Described`] with the one video the shell
    /// queues and plays, and a failure as [`Answer::Failed`] with the
    /// tool's own sentence. A worker that died before it answered is one
    /// sentence too, never an eternal wait.
    pub fn poll(&mut self) -> Option<Answer> {
        /// What one tick's try at the in-flight job found.
        enum Polled {
            StillRunning,
            Search(Result<Vec<Found>, ToolError>),
            Describe(Result<Found, ToolError>),
            Lost,
        }

        let pending = self.pending.take()?;
        let polled = match pending {
            Pending::Search { query, answer } => match answer.try_recv() {
                Ok(answer) => Polled::Search(answer),
                Err(TryRecvError::Empty) => {
                    self.pending = Some(Pending::Search { query, answer });
                    Polled::StillRunning
                }
                Err(TryRecvError::Disconnected) => Polled::Lost,
            },
            Pending::Describe { input, answer } => match answer.try_recv() {
                Ok(answer) => Polled::Describe(answer),
                Err(TryRecvError::Empty) => {
                    self.pending = Some(Pending::Describe { input, answer });
                    Polled::StillRunning
                }
                Err(TryRecvError::Disconnected) => Polled::Lost,
            },
        };
        match polled {
            Polled::StillRunning => None,
            Polled::Lost => Some(Answer::Failed(String::from(
                "the search stopped without an answer",
            ))),
            Polled::Search(Err(error)) | Polled::Describe(Err(error)) => {
                Some(Answer::Failed(error.to_string()))
            }
            Polled::Search(Ok(results)) if results.is_empty() => Some(Answer::Empty),
            Polled::Search(Ok(results)) => {
                self.results = results;
                Some(Answer::Filled)
            }
            Polled::Describe(Ok(found)) => Some(Answer::Described(found)),
        }
    }

    /// The rows the keys move over: the query row and one row per result.
    fn rows(&self) -> usize {
        1 + self.results.len()
    }

    /// The keys the screen owns (T5), by the mode it is in — one meaning at
    /// a time:
    ///
    /// **Typing** (the mode it opens in, `/` or `i` returning to it): every
    /// printable goes to the query, `enter` searches, `esc` closes — and
    /// the arrow keys still move the results list, because they are not
    /// text; moving off the query row is also what leaves typing.
    ///
    /// **Browsing** (entered when a search starts, or the focus moves away
    /// from the query): `j`/`k`/`g`/`G` and the arrows move, `enter` plays
    /// the selected result, `esc` closes — and every other key falls
    /// through ([`Outcome::Unhandled`]) so the app's own keys work: `a`,
    /// `?`, `u`, the transport, everything a user already knows.
    pub fn key(&mut self, event: KeyEvent) -> Outcome {
        let rows = self.rows();
        match event.code {
            // Typing owns every printable, `j` and `k` included: the
            // movement keys are the browsing mode's keys, not text's.
            KeyCode::Char(character)
                if self.typing && !event.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.query.push(character);
                self.cursor = 0;
                Outcome::Handled
            }
            KeyCode::Backspace if self.typing => {
                self.query.pop();
                self.cursor = 0;
                Outcome::Handled
            }
            // `enter`: the query row searches in typing mode; in browsing
            // mode a result row queues and plays it (S3), the shell
            // carries it out, the screen holds no queue of its own.
            KeyCode::Enter => match (self.typing, self.cursor) {
                (true, _) => Outcome::Search(self.query.clone()),
                (false, 0) => Outcome::Handled,
                (_, index) => match self.results.get(index - 1).cloned() {
                    Some(found) => Outcome::Play(found),
                    None => Outcome::Handled,
                },
            },
            KeyCode::Esc => Outcome::Close,
            // `/` (and `i`, the tree's insert instinct) return to typing
            // with the current query and the cursor at its end.
            KeyCode::Char('/' | 'i') if !self.typing => {
                self.typing = true;
                self.cursor = 0;
                Outcome::Handled
            }
            // The movement, with the wrapping the other lists have (issue
            // #36): down from the last row lands on the first, up from the
            // first on the last. In browsing mode `j`/`k` move with the
            // arrows; in typing mode the arrows still move — they are not
            // text — and moving off the query row is the refocus that
            // enters browsing.
            KeyCode::Char('j') | KeyCode::Down if !self.typing => {
                self.cursor = (self.cursor + 1) % rows;
                Outcome::Handled
            }
            KeyCode::Char('k') | KeyCode::Up if !self.typing => {
                self.cursor = (self.cursor + rows - 1) % rows;
                Outcome::Handled
            }
            KeyCode::Char('g') | KeyCode::Home | KeyCode::PageUp if !self.typing => {
                self.cursor = 0;
                Outcome::Handled
            }
            KeyCode::Char('G') | KeyCode::End | KeyCode::PageDown if !self.typing => {
                self.cursor = rows - 1;
                Outcome::Handled
            }
            KeyCode::Down => {
                self.cursor = (self.cursor + 1) % rows;
                self.leave_typing_off_the_query_row();
                Outcome::Handled
            }
            KeyCode::Up => {
                self.cursor = (self.cursor + rows - 1) % rows;
                self.leave_typing_off_the_query_row();
                Outcome::Handled
            }
            KeyCode::Home | KeyCode::PageUp => {
                self.cursor = 0;
                self.leave_typing_off_the_query_row();
                Outcome::Handled
            }
            KeyCode::End | KeyCode::PageDown => {
                self.cursor = rows - 1;
                self.leave_typing_off_the_query_row();
                Outcome::Handled
            }
            // Browsing hands every other key to the app; typing hands the
            // ones it cannot type — the ctrl pairs above all, and the
            // keys no screen owns — to it too.
            _ => Outcome::Unhandled,
        }
    }

    /// The focus is off the query row: typing stops, browsing begins —
    /// unless the movement landed back on the query row itself, which is
    /// where typing still lives.
    fn leave_typing_off_the_query_row(&mut self) {
        if self.cursor != 0 {
            self.typing = false;
        }
    }

    /// Draws the screen over the whole area the sources screen lent it —
    /// its own header line, the query, the results. `playing` names the
    /// entry standing when the shell asked, so the row it plays is the one
    /// marked, the way every list marks it (issue #53). `now_playing` names
    /// the one now-playing line: what the shell's own playing entry is
    /// when it is a `YouTube` one, nothing when it is not — the shell reads
    /// the queue, the screen only draws it and holds none of it.
    pub fn render(
        &self,
        frame: &mut ratatui::Frame<'_>,
        area: Rect,
        theme: &Theme,
        playing: Option<(&std::path::Path, PlayState)>,
        now_playing: Option<&str>,
    ) {
        frame.render_widget(Block::new().style(Style::new().bg(theme.background)), area);
        let [header_area, rest_area] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);

        let count = if self.results.is_empty() {
            String::new()
        } else {
            format!("  [{}]", self.results.len())
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("▸─ ", Style::new().fg(theme.border)),
                Span::styled("YouTube search", Style::new().fg(theme.text).bold()),
                Span::styled(count, Style::new().fg(theme.dim)),
            ])),
            header_area,
        );

        // The query row, the one line naming what is playing when there is
        // one, and the list. The mode shows on the query row: typing is the
        // row being typed into, with the caret showing; browsing dims it,
        // and the selected result carries the list's own highlight instead.
        let (query_area, playing_area, list_area) =
            Self::list_layout(rest_area, now_playing.is_some());
        let caret = if self.typing { "▏" } else { "" };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("search: ", Style::new().fg(theme.dim)),
                Span::styled(
                    format!("{}{caret}", self.query),
                    if self.typing {
                        Style::new().fg(theme.accent)
                    } else {
                        Style::new().fg(theme.dim)
                    },
                ),
            ])),
            query_area,
        );
        if let Some((title, playing_area)) = now_playing.zip(playing_area) {
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("▶ ", Style::new().fg(theme.accent)),
                    Span::styled(title.to_owned(), Style::new().fg(theme.text)),
                ])),
                playing_area,
            );
        }

        if self.results.is_empty() {
            let message = if self.pending.is_some() {
                "⟳ searching…"
            } else {
                "type a query — enter searches"
            };
            draw_empty(frame, list_area, message, theme.dim);
            return;
        }

        self.draw_results(frame, list_area, theme, playing);
    }

    /// The three areas the body splits into: the query row, the one line
    /// naming what is playing when there is one, and the list.
    fn list_layout(area: Rect, now_playing: bool) -> (Rect, Option<Rect>, Rect) {
        if now_playing {
            let [query_area, playing_area, list_area] = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .areas(area);
            (query_area, Some(playing_area), list_area)
        } else {
            let [query_area, list_area] =
                Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
            (query_area, None, list_area)
        }
    }

    /// The results list: one row per answer, the standing one marked, a
    /// live broadcast marked the way the radio marks a station velo cannot
    /// play — the `✗` of [`station_label`], and no length, because the row
    /// has none to give and would be refused if chosen (`duration` is
    /// already `None` for it).
    fn draw_results(
        &self,
        frame: &mut ratatui::Frame<'_>,
        list_area: Rect,
        theme: &Theme,
        playing: Option<(&std::path::Path, PlayState)>,
    ) {
        let digits = digits(self.results.len());
        let rows: Vec<ListItem<'static>> = self
            .results
            .iter()
            .enumerate()
            .map(|(index, found)| {
                let path = crate::youtube::pseudo_path(&found.id);
                let mark = match playing {
                    Some((entry, PlayState::Playing)) if entry == path.as_path() => {
                        RowMark::Playing
                    }
                    Some((entry, PlayState::Paused)) if entry == path.as_path() => RowMark::Paused,
                    _ => RowMark::None,
                };
                // The `✗` and no length are [`draw_results`]'s to explain;
                // the row itself is every result row's shape.
                let name = if found.live {
                    station_label(false, true, &found.title, "")
                } else {
                    found.title.clone()
                };
                ListItem::new(list_row(
                    ListRow {
                        mark,
                        number: Some(index + 1),
                        digits,
                        name,
                        note: found.uploader.clone().unwrap_or_default(),
                        fact: clock(found.duration),
                    },
                    list_area.width,
                    theme,
                ))
            })
            .collect();
        let list = List::new(rows)
            .style(Style::new().bg(theme.background))
            .highlight_style(Style::new().bg(theme.accent).fg(theme.ink));
        let mut state = ListState::default();
        if self.cursor > 0 {
            state.select(Some(self.cursor - 1));
        }
        frame.render_stateful_widget(list, list_area, &mut state);
    }
}

/// `1:23` — the fact column of a result with a known length; empty for a
/// broadcast that never ends (S3), whose row never pretends to one.
fn clock(secs: Option<u64>) -> String {
    let Some(secs) = secs else {
        return String::new();
    };
    format!("{}:{:02}", secs / 60, secs % 60)
}
