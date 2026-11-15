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

/// What one finished background search hands back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// The tool failed: its own sentence, one line.
    Failed(String),
    /// The tool answered nothing usable: a query with no results.
    Empty,
    /// The tool answered results, now held by the screen's list.
    Filled,
}

/// The search of one query, in flight until its answer is polled.
struct Pending {
    /// The query the running search was asked for: what the status line
    /// names, and nothing else — the line is rewritten if the query on
    /// screen has moved on, never silently stale.
    query: String,
    /// The answer the worker hands back, polled once per tick.
    answer: Receiver<Result<Vec<Found>, ToolError>>,
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
    /// The last search's answers, in the order the tool answered them.
    results: Vec<Found>,
    /// The search in flight, its answer still to be polled. One at a time:
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
            results: Vec::new(),
            pending: None,
        }
    }

    /// What is typed, for the tests and the header.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The answers on the list, in the order the tool answered them.
    #[must_use]
    pub fn results(&self) -> &[Found] {
        &self.results
    }

    /// The query a running search was asked for, while one runs.
    #[must_use]
    pub fn searching_query(&self) -> Option<&str> {
        self.pending.as_ref().map(|pending| pending.query.as_str())
    }

    /// Starts one search on its own thread (S6): the tool runs off the event
    /// loop bounded by its own search deadline, the interface keeps
    /// answering keys, and the answer lands through the tick. Refused while
    /// one is already in flight — one request at a time, the rule every
    /// other wired view keeps; the running one's deadline is its end.
    /// Answers `false` when the ask was refused.
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
        self.pending = Some(Pending {
            query: asked,
            answer,
        });
        true
    }

    /// Collects a finished search, once per tick: still running is no news,
    /// an answer lands as [`Answer::Filled`] with the results held here, a
    /// query with nothing behind it as [`Answer::Empty`], a failure as
    /// [`Answer::Failed`] with the tool's own sentence, and a worker that
    /// died before it answered is one sentence too, never an eternal wait.
    pub fn poll(&mut self) -> Option<Answer> {
        let pending = self.pending.as_ref()?;
        let answer = match pending.answer.try_recv() {
            Ok(answer) => answer,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                self.pending = None;
                return Some(Answer::Failed(String::from(
                    "the search stopped without an answer",
                )));
            }
        };
        self.pending = None;
        match answer {
            Err(error) => Some(Answer::Failed(error.to_string())),
            Ok(results) if results.is_empty() => Some(Answer::Empty),
            Ok(results) => {
                self.results = results;
                Some(Answer::Filled)
            }
        }
    }

    /// The rows the keys move over: the query row and one row per result.
    fn rows(&self) -> usize {
        1 + self.results.len()
    }

    /// The keys the screen owns (T5); typing goes to the query, `enter`
    /// searches on the query row and plays on a result, `esc` goes back, and
    /// `j`/`k`/arrows move with the wrapping every other list has.
    pub fn key(&mut self, event: KeyEvent) -> Outcome {
        let rows = self.rows();
        match event.code {
            // Typing goes to the query (S7's only place it exists), and a
            // change brings the cursor home to the query row. The movement
            // keys are not typing, the way the library search's are not:
            // they move the list even while a query is open.
            KeyCode::Char(character)
                if !event.modifiers.contains(KeyModifiers::CONTROL)
                    && !matches!(character, 'j' | 'k') =>
            {
                self.query.push(character);
                self.cursor = 0;
                Outcome::Handled
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.cursor = 0;
                Outcome::Handled
            }
            // The movement, with the wrapping the other lists have (issue
            // #36): down from the last row lands on the first, up from the
            // first on the last.
            KeyCode::Char('j') | KeyCode::Down => {
                self.cursor = (self.cursor + 1) % rows;
                Outcome::Handled
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.cursor = (self.cursor + rows - 1) % rows;
                Outcome::Handled
            }
            KeyCode::Char('g') | KeyCode::Home | KeyCode::PageUp => {
                self.cursor = 0;
                Outcome::Handled
            }
            KeyCode::Char('G') | KeyCode::End | KeyCode::PageDown => {
                self.cursor = rows - 1;
                Outcome::Handled
            }
            KeyCode::Enter => match self.cursor {
                // The query row searches what is typed, whatever it holds.
                0 => Outcome::Search(self.query.clone()),
                // A result row queues and plays it (S3): the shell carries
                // it out, the screen holds no queue of its own.
                index => match self.results.get(index - 1).cloned() {
                    Some(found) => Outcome::Play(found),
                    None => Outcome::Handled,
                },
            },
            KeyCode::Esc => Outcome::Close,
            _ => Outcome::Unhandled,
        }
    }

    /// Draws the screen over the whole area the sources screen lent it —
    /// its own header line, the query, the results. `playing` names the
    /// entry standing when the shell asked, so the row it plays is the one
    /// marked, the way every list marks it (issue #53).
    pub fn render(
        &self,
        frame: &mut ratatui::Frame<'_>,
        area: Rect,
        theme: &Theme,
        playing: Option<(&std::path::Path, PlayState)>,
    ) {
        frame.render_widget(Block::new().style(Style::new().bg(theme.background)), area);
        let [header_area, query_area, list_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .areas(area);

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

        // The query row, with the caret showing while the cursor rests on
        // it: the row `enter` searches from.
        let caret = if self.cursor == 0 { "▏" } else { "" };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("search: ", Style::new().fg(theme.dim)),
                Span::styled(
                    format!("{}{caret}", self.query),
                    if self.cursor == 0 {
                        Style::new().fg(theme.accent)
                    } else {
                        Style::new().fg(theme.text)
                    },
                ),
            ])),
            query_area,
        );

        if self.results.is_empty() {
            let message = if self.pending.is_some() {
                "⟳ searching…"
            } else {
                "type a query — enter searches"
            };
            draw_empty(frame, list_area, message, theme.dim);
            return;
        }

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
                // A live broadcast is marked the way the radio marks a station
                // velo cannot play — the `✗` of [`station_label`], and no
                // length, because the row has none to give and would be
                // refused if chosen (`duration` is already `None` for it).
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
