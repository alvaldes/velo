//! The cliamp radio service's client (issue #48): the channel list, the
//! listeners and one channel's songs, parsed with `miniserde` over the
//! directory's [`Transport`] seam.
//!
//! [`HOST`] is another project's service, not velo's (S5): it runs behind
//! its own nginx and its own cache, and velo controls nothing about it.
//! When it changes or dies, the cliamp section degrades — a failing
//! request is one status line and an empty or count-less section (S4) —
//! and the rest of velo does not; the `[radio] cliamp` switch exists so a
//! dead service needs a config line, not a release. The service asks for
//! no credential (verified against the running service, `odd/tasks/cliamp.md`).
//!
//! The three endpoints:
//!
//! - `GET /stations` — the channels: `{"stations":[{"id","name",…,
//!   "tracks",…}]}`. `tracks` is the channel's song count.
//! - `GET /statistics` — who is listening right now:
//!   `{"stations":{"<id>":{"active_listeners":N,…}}}`. The shape is a
//!   map of dynamic ids, so it is walked as `miniserde::json::Value`
//!   rather than a derived struct; a station without a count counts zero.
//! - `GET /<id>/tracks` — one channel's songs:
//!   `{"station","name","count","tracks":[{"id","title","artist","album",
//!   "filename","url"}]}`, in the service's order. Every song is a finite
//!   remote file (S3): its `url` is the identity, and the cache resolver
//!   fetches it.
//!
//! Responses carry more fields than the ones parsed here; unknown fields
//! are ignored, never an error. The one network primitive is the shared
//! [`Transport`] seam: the client builds the URL, the transport fetches
//! the body as text, and every test injects canned bodies instead of a
//! network.

use miniserde::Deserialize;

use crate::radio::client::{Error, Transport};

/// The cliamp radio service's host (S5): another project's service, not
/// velo's. The URL of every request is built from this constant here and
/// nowhere else. The service is at odds with nothing in velo — it asks for
/// no credential and answers plain JSON — but when it changes or dies, the
/// cliamp section degrades on its own: a failing request is one status
/// line, an empty channel list or a missing `●` count (S4), and the rest
/// of velo does not notice. The `[radio] cliamp` switch exists so a dead
/// service needs a config line, not a release.
pub const HOST: &str = "radio.cliamp.stream";

/// One channel of the cliamp section, as `/stations` lists it. The song
/// count is the row's fact beside the listeners (S2); `description`,
/// `genre` and `stream` are ignored here — a channel plays through its
/// songs, never as a live stream (S3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Channel {
    /// The channel's id, as the URLs name it (`lofi`).
    pub id: String,
    /// The channel's display name.
    pub name: String,
    /// The channel's song count, as `/stations` reports it.
    pub tracks: u32,
}

/// One station's row of `/statistics`: how many people the service says
/// are listening right now (S2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listeners {
    /// The station's id, matching a [`Channel::id`].
    pub id: String,
    /// The `active_listeners` the service reports; a station the answer
    /// leaves out, or one without the field, counts zero.
    pub active: u32,
}

/// One song of one channel, as `/<id>/tracks` lists it (S2). It is a
/// finite remote track (S3): the `url` is both its identity and the path
/// the queue carries — the cache resolver fetches it, so the transport
/// shows a position, never `● live`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Song {
    /// The song's id, as the service names it.
    pub id: String,
    /// The song's title — the row's label (S2).
    pub title: String,
    /// The song's artist; beside the album, the row's dim note.
    pub artist: String,
    /// The song's album; beside the artist, the row's dim note.
    pub album: String,
    /// The file's name on the service, as sent. Not shown; parsed because
    /// the endpoint always carries it and the shape is the contract.
    pub filename: String,
    /// The song's URL — the identity everywhere (S3).
    pub url: String,
}

/// Everything the wire answers with for one channel list.
#[derive(Debug, Deserialize)]
struct StationsWire {
    /// The channels, in the service's order.
    #[serde(rename = "stations")]
    stations: Vec<ChannelWire>,
}

/// One channel as the wire lists it.
#[derive(Debug, Deserialize)]
struct ChannelWire {
    id: String,
    name: String,
    tracks: u32,
}

/// Everything the wire answers with for one channel's songs.
#[derive(Debug, Deserialize)]
struct TracksWire {
    /// The songs, in the service's order.
    #[serde(rename = "tracks")]
    tracks: Vec<SongWire>,
}

/// One song as the wire lists it. The service omits `artist`, `album` and
/// `filename` for songs it has no metadata for — dozens per channel — so
/// those are optional here and an absent field is an empty one, never a
/// malformed answer (issue #55).
#[derive(Debug, Deserialize)]
struct SongWire {
    id: String,
    title: String,
    artist: Option<String>,
    album: Option<String>,
    filename: Option<String>,
    url: String,
}

/// The client over one transport. The host is [`HOST`] — another project's
/// service (S5) — and no config key moves it: a different service is a
/// different section, not a renamed one.
#[derive(Debug)]
pub struct Client<T> {
    /// The transport every request goes through; public so tests can read
    /// back what a canned transport recorded.
    pub transport: T,
}

impl<T: Transport> Client<T> {
    /// A client for the cliamp service over the given transport.
    #[must_use]
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    /// The channel list: `GET /stations` (S2).
    ///
    /// # Errors
    ///
    /// Fails with the transport's [`Error::Unreachable`] — its reason
    /// naming [`HOST`] — or [`Error::Malformed`] when the answer is not
    /// the expected JSON.
    pub fn stations(&self) -> Result<Vec<Channel>, Error> {
        let url = format!("https://{HOST}/stations");
        let wire: StationsWire = self.fetch_json(&url)?;
        Ok(wire
            .stations
            .into_iter()
            .map(|channel| Channel {
                id: channel.id,
                name: channel.name,
                tracks: channel.tracks,
            })
            .collect())
    }

    /// The listeners: `GET /statistics` (S2). The answer's map keys are
    /// the station ids, so the shape is walked as a [`miniserde::json::Value`]
    /// rather than a derived struct; a station without the field counts
    /// zero, and the order of the answer's map is not meaningful (the
    /// rows are looked up by id).
    ///
    /// # Errors
    ///
    /// As [`Client::stations`].
    pub fn statistics(&self) -> Result<Vec<Listeners>, Error> {
        let url = format!("https://{HOST}/statistics");
        let body = self.fetch(&url)?;
        let value: miniserde::json::Value =
            miniserde::json::from_str(&body).map_err(|error| Error::Malformed {
                detail: error.to_string(),
            })?;
        Ok(listeners_of(&value))
    }

    /// One channel's songs: `GET /<id>/tracks` (S2), in the service's
    /// order.
    ///
    /// # Errors
    ///
    /// As [`Client::stations`].
    pub fn songs(&self, id: &str) -> Result<Vec<Song>, Error> {
        let url = format!("https://{HOST}/{}/tracks", percent_encode(id));
        let wire: TracksWire = self.fetch_json(&url)?;
        Ok(wire
            .tracks
            .into_iter()
            .map(|song| Song {
                id: song.id,
                title: song.title,
                artist: song.artist.unwrap_or_default(),
                album: song.album.unwrap_or_default(),
                filename: song.filename.unwrap_or_default(),
                url: song.url,
            })
            .collect())
    }

    /// Fetches one URL through the transport and parses it as JSON of
    /// `T2`. A transport failure is wrapped here, where the host is known,
    /// so the one-line reason names where the request went (S4); the
    /// failure is one status-line message, never a retry.
    fn fetch_json<T2: Deserialize>(&self, url: &str) -> Result<T2, Error> {
        let body = self.fetch(url)?;
        miniserde::json::from_str(&body).map_err(|error| Error::Malformed {
            detail: error.to_string(),
        })
    }

    /// The body of one GET, with the transport's failure wrapped so the
    /// reason names the host.
    fn fetch(&self, url: &str) -> Result<String, Error> {
        self.transport.get(url).map_err(|error| match error {
            Error::Unreachable { detail } => Error::Unreachable {
                detail: format!("{HOST} could not be reached: {detail}"),
            },
            error @ Error::Malformed { .. } => error,
        })
    }
}

/// The listener rows of a parsed `/statistics` answer: every station the
/// answer names, its `active_listeners` when the field is there, zero
/// otherwise. Anything else unexpected in the answer is ignored — the
/// service owns the shape, and a stray field must not empty the section.
fn listeners_of(value: &miniserde::json::Value) -> Vec<Listeners> {
    let miniserde::json::Value::Object(root) = value else {
        return Vec::new();
    };
    let Some(miniserde::json::Value::Object(stations)) = root.get("stations") else {
        return Vec::new();
    };
    stations
        .iter()
        .map(|(id, station)| {
            let active = match station {
                miniserde::json::Value::Object(fields) => match fields.get("active_listeners") {
                    Some(miniserde::json::Value::Number(miniserde::json::Number::U64(count))) => {
                        u32::try_from(*count).unwrap_or(u32::MAX)
                    }
                    // The clamp above keeps the cast exact: the value is
                    // already inside `0..=u32::MAX`.
                    Some(miniserde::json::Value::Number(miniserde::json::Number::I64(count))) => {
                        u32::try_from((*count).clamp(0, i64::from(u32::MAX))).unwrap_or(u32::MAX)
                    }
                    _ => 0,
                },
                _ => 0,
            };
            Listeners {
                id: id.clone(),
                active,
            }
        })
        .collect()
}

/// Percent-encodes one path or query value, so a channel id the service
/// spells unusually survives the URL. The same rule the directory client
/// applies to its query values: everything outside the RFC 3986 unreserved
/// set is escaped.
fn percent_encode(value: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(char::from(byte));
            }
            other => {
                let _ = write!(out, "%{other:02X}");
            }
        }
    }
    out
}
