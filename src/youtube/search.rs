//! The search: one `yt-dlp` call, one flat JSON object per line.
//!
//! The tool is asked for JSON, never for a human list to scrape (S2): a flat
//! extraction of the search page, ten objects, and only the members a queue row
//! shows are read. A line nobody can use is dropped, because a broken answer
//! costs the lines that did parse and never the whole search.

use std::ffi::OsString;
use std::path::Path;

use crate::youtube::tool;

/// How many results a search asks for.
const RESULTS: usize = 10;

/// One result: what the search answered and everything a queue row shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// The video id: what the queue keeps as `youtube://<id>`.
    pub id: String,
    /// The video's own title, as the search page states it.
    pub title: String,
    /// The channel, shown where an artist would be. A search entry names its
    /// uploader and sometimes only its channel; either is that artist.
    pub uploader: Option<String>,
    /// Length in whole seconds, `None` for a broadcast that never ends.
    pub duration: Option<u64>,
    /// Whether the search says the result is a live broadcast: its line's
    /// `live_status` is `is_live`. The screen marks the row and the shell
    /// refuses it before anything is queued, because a live broadcast is HLS
    /// and velo cannot play HLS — a queue entry would fail at open time.
    pub live: bool,
}

/// Asks the tool for [`RESULTS`] answers to `query`, and reads them.
///
/// # Errors
///
/// Whatever [`tool::run`] fails with: the tool is missing, missed its deadline,
/// or refused in its own words.
pub fn run(program: &Path, query: &str) -> Result<Vec<Found>, tool::ToolError> {
    let out = tool::run(program, &arguments(query), tool::SEARCH)?;
    Ok(parse(&out))
}

/// The arguments one search runs with: a flat extraction, JSON, ten entries,
/// and the query as one argument after the `--` that stops the tool reading it
/// as an option.
#[must_use]
pub fn arguments(query: &str) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "--flat-playlist",
        "--dump-json",
        "--no-warnings",
        "--no-progress",
        "--playlist-end",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    args.push(OsString::from(RESULTS.to_string()));
    args.push(OsString::from("--"));
    args.push(OsString::from(format!("ytsearch{RESULTS}:{query}")));
    args
}

/// Reads one line per result out of the tool's answer.
#[must_use]
pub fn parse(out: &str) -> Vec<Found> {
    out.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| miniserde::json::from_str::<Line>(line).ok())
        .filter_map(Line::into_found)
        .collect()
}

/// The members velo reads out of a search line. Everything else in the object
/// the tool writes — descriptions, urls, thumbnails — is ignored.
#[derive(miniserde::Deserialize)]
struct Line {
    id: Option<String>,
    title: Option<String>,
    uploader: Option<String>,
    channel: Option<String>,
    duration: Option<f64>,
    live_status: Option<String>,
}

impl Line {
    fn into_found(self) -> Option<Found> {
        let id = self.id?;
        if id.is_empty() {
            return None;
        }
        let title = self
            .title
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| id.clone());
        // `is_live` is the only value of `live_status` that means live now;
        // `is_upcoming` or `was_live` names a recorded or scheduled video and
        // is not refused.
        let live = self.live_status.as_deref() == Some("is_live");
        Some(Found {
            id,
            title,
            uploader: self.uploader.or(self.channel),
            // A length the tool states as a float, and a live broadcast's
            // `null`: the id is a length only when it is one, and a row marked
            // live never invents one even if the line carried a number.
            duration: if live {
                None
            } else {
                self.duration
                    .and_then(|secs| std::time::Duration::try_from_secs_f64(secs).ok())
                    .map(|length| length.as_secs())
            },
            live,
        })
    }
}
