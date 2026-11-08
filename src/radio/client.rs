//! The Radio Browser client: URL building, the transport seam and the wire
//! structs.
//!
//! The directory service is Radio Browser, queried exactly as the CLIAMP
//! client queries it: base [`BASE_URL`], a `User-Agent` of [`USER_AGENT`] on
//! every request, `/stations/search` for stations and three index endpoints
//! (`/countries`, `/states/<country>/`, `/tags`). The playback URL is
//! `url_resolved`, never `url`. Responses carry many more fields than the
//! ones parsed here; unknown fields are ignored, never an error.
//!
//! The one network primitive is the [`Transport`] seam: the client builds
//! the URL, the transport fetches the body as text, and every test injects
//! canned bodies instead of a network. It mirrors the Subsonic transport's
//! shape but lives here because that one returns Subsonic's own error type
//! and sends no `User-Agent`; unifying them is a possible follow-up.
//!
//! Timeouts are bounded per request (10 s, what the reference client uses)
//! by [`HttpTransport`]'s agent. The audio stream is a different path and
//! carries no whole-repo timeout.

use std::fmt;

use miniserde::Deserialize;

/// The Radio Browser base the directory is queried through.
pub const BASE_URL: &str = "https://de1.api.radio-browser.info/json";

/// The required `User-Agent`: the API rejects requests without one.
pub const USER_AGENT: &str = concat!("velo/", env!("CARGO_PKG_VERSION"));

/// The default and maximum number of stations one search loads.
pub const DEFAULT_LIMIT: u32 = 200;

/// Everything the directory client fails with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The service could not be reached: refused connection, DNS failure, a
    /// non-200 answer.
    Unreachable {
        /// What the transport observed.
        detail: String,
    },
    /// The service answered but the body is not the JSON that was expected.
    Malformed {
        /// What the body looked like, for the one-line reason.
        detail: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreachable { detail } => {
                write!(
                    formatter,
                    "the station directory could not be reached: {detail}"
                )
            }
            Self::Malformed { detail } => {
                write!(
                    formatter,
                    "the station directory answered malformed JSON: {detail}"
                )
            }
        }
    }
}

impl std::error::Error for Error {}

/// The seam every request goes through: one GET, the body back as text. The
/// production implementation is [`HttpTransport`]; tests inject canned
/// bodies and never touch the network.
pub trait Transport {
    /// Performs one GET of `url` and returns the body text.
    ///
    /// # Errors
    ///
    /// Fails when the request cannot be completed.
    fn get(&self, url: &str) -> Result<String, Error>;
}

/// The production [`Transport`]: one blocking GET at a time over `ureq`
/// with rustls, a 10 s bound per request and the required `User-Agent`.
#[derive(Clone)]
pub struct HttpTransport {
    agent: ureq::Agent,
}

impl HttpTransport {
    /// Builds the transport.
    ///
    /// # Errors
    ///
    /// Never fails today; the result keeps the seam free to grow.
    pub fn new() -> Result<Self, Error> {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(10)))
            .build()
            .new_agent();
        Ok(Self { agent })
    }
}

impl Transport for HttpTransport {
    fn get(&self, url: &str) -> Result<String, Error> {
        let mut response = self
            .agent
            .get(url)
            .header("User-Agent", USER_AGENT)
            .call()
            .map_err(|error| Error::Unreachable {
                detail: match error {
                    ureq::Error::StatusCode(status) => format!("the server answered HTTP {status}"),
                    other => other.to_string(),
                },
            })?;
        response
            .body_mut()
            .read_to_string()
            .map_err(|error| Error::Malformed {
                detail: error.to_string(),
            })
    }
}

/// The sort order of a station search.
///
/// `reverse=true` is sent for every order except `name` and `random`: the
/// API's `reverse` flips a low-to-high order, and `name` is wanted A→Z
/// while `random` has no meaningful direction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Order {
    /// By community votes, most first.
    #[default]
    Votes,
    /// By total click count, most first.
    ClickCount,
    /// By recent click trend, rising first.
    ClickTrend,
    /// Alphabetically, A→Z.
    Name,
    /// A fresh shuffle per request.
    Random,
}

impl Order {
    /// The API's spelling of the order, as sent in the query string.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Votes => "votes",
            Self::ClickCount => "clickcount",
            Self::ClickTrend => "clicktrend",
            Self::Name => "name",
            Self::Random => "random",
        }
    }

    /// Whether the request sends `reverse=true` for this order.
    #[must_use]
    pub fn reversed(self) -> bool {
        !matches!(self, Self::Name | Self::Random)
    }
}

/// One station search. `Default` is the plain first page: the 200 most
/// voted non-broken stations, no filters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StationSearch {
    /// Match the station name.
    pub name: Option<String>,
    /// Match one tag, exactly (`tagExact=true` is always sent so filtering
    /// "Georgia" cannot pull in "South Georgia").
    pub tag: Option<String>,
    /// Two-letter ISO 3166-1 country code, matched exactly.
    pub countrycode: Option<String>,
    /// Match the state, exactly (`stateExact=true` is always sent).
    pub state: Option<String>,
    /// The sort order.
    pub order: Order,
    /// The first result to return, for paging.
    pub offset: u32,
    /// How many results to return; the API never loads more than
    /// [`DEFAULT_LIMIT`] per place.
    pub limit: u32,
}

impl Default for StationSearch {
    fn default() -> Self {
        Self {
            name: None,
            tag: None,
            countrycode: None,
            state: None,
            order: Order::default(),
            offset: 0,
            limit: DEFAULT_LIMIT,
        }
    }
}

/// One station as the directory lists it.
///
/// Every field is the API's own; the playback URL is `url_resolved`, the
/// resolved stream and never the registration `url`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Station {
    /// The station's display name.
    #[serde(rename = "name")]
    pub name: String,
    /// The resolved stream URL — the one playback opens.
    #[serde(rename = "url_resolved")]
    pub url_resolved: String,
    /// The country's display name.
    #[serde(rename = "country")]
    pub country: String,
    /// The two-letter ISO 3166-1 country code.
    #[serde(rename = "countrycode")]
    pub countrycode: String,
    /// The state or region the station lists itself under.
    #[serde(rename = "state")]
    pub state: String,
    /// Comma-separated tags.
    #[serde(rename = "tags")]
    pub tags: String,
    /// The codec as the directory spells it, e.g. `MP3`, `AAC`, `OGG`.
    #[serde(rename = "codec")]
    pub codec: String,
    /// The advertised bitrate in kbps.
    #[serde(rename = "bitrate")]
    pub bitrate: u32,
    /// The community vote count.
    #[serde(rename = "votes")]
    pub votes: u32,
    /// The station's homepage.
    #[serde(rename = "homepage")]
    pub homepage: String,
}

/// One country in the index: `/countries`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Country {
    /// The country's display name.
    #[serde(rename = "name")]
    pub name: String,
    /// The two-letter ISO 3166-1 code, what `/states/<country>/` wants.
    #[serde(rename = "iso_3166_1")]
    pub iso_3166_1: String,
    /// How many non-broken stations the country lists.
    #[serde(rename = "stationcount")]
    pub stationcount: u32,
}

/// One state or region in the index: `/states/<country>/`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct State {
    /// The state's display name.
    #[serde(rename = "name")]
    pub name: String,
    /// The country's display name.
    #[serde(rename = "country")]
    pub country: String,
    /// How many non-broken stations the state lists.
    #[serde(rename = "stationcount")]
    pub stationcount: u32,
}

/// One tag in the index: `/tags`, most used first.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Tag {
    /// The tag's display name.
    #[serde(rename = "name")]
    pub name: String,
    /// How many non-broken stations carry the tag.
    #[serde(rename = "stationcount")]
    pub stationcount: u32,
}

/// The client over one transport. The base URL is fixed: the directory is
/// one well-known service, not a user-configured server.
#[derive(Debug)]
pub struct Client<T> {
    /// The transport every request goes through; public so tests can read
    /// back what a canned transport recorded.
    pub transport: T,
}

impl<T: Transport> Client<T> {
    /// A client for the directory over the given transport.
    #[must_use]
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    /// Searches stations: builds the query, fetches it and parses the list.
    ///
    /// # Errors
    ///
    /// Fails with the transport's [`Error::Unreachable`] or
    /// [`Error::Malformed`] when the answer is not the expected JSON array.
    pub fn search_stations(&self, search: &StationSearch) -> Result<Vec<Station>, Error> {
        let url = search_url(search);
        self.fetch_json(&url)
    }

    /// The country index.
    ///
    /// # Errors
    ///
    /// As [`Client::search_stations`].
    pub fn countries(&self) -> Result<Vec<Country>, Error> {
        self.fetch_json(&format!("{BASE_URL}/countries"))
    }

    /// The state index of one country, by its ISO 3166-1 code.
    ///
    /// # Errors
    ///
    /// As [`Client::search_stations`].
    pub fn states(&self, country: &str) -> Result<Vec<State>, Error> {
        self.fetch_json(&format!("{BASE_URL}/states/{}/", percent_encode(country)))
    }

    /// The tag index, most used first.
    ///
    /// # Errors
    ///
    /// As [`Client::search_stations`].
    pub fn tags(&self) -> Result<Vec<Tag>, Error> {
        self.fetch_json(&tags_url())
    }

    /// Fetches one URL through the transport and parses the JSON array.
    fn fetch_json<T2: Deserialize>(&self, url: &str) -> Result<Vec<T2>, Error> {
        let body = self.transport.get(url)?;
        parse_array(&body)
    }
}

/// Builds the `/stations/search` URL for one search. The parameter order is
/// fixed so the query string is byte-for-byte testable.
#[must_use]
pub fn search_url(search: &StationSearch) -> String {
    let mut pairs = vec![
        format!("limit={}", search.limit),
        format!("offset={}", search.offset),
        format!("order={}", search.order.as_str()),
    ];
    if search.order.reversed() {
        pairs.push("reverse=true".to_owned());
    }
    pairs.push("hidebroken=true".to_owned());
    if let Some(name) = &search.name {
        pairs.push(format!("name={}", percent_encode(name)));
    }
    if let Some(tag) = &search.tag {
        pairs.push(format!("tag={}", percent_encode(tag)));
        pairs.push("tagExact=true".to_owned());
    }
    if let Some(countrycode) = &search.countrycode {
        pairs.push(format!("countrycode={}", percent_encode(countrycode)));
    }
    if let Some(state) = &search.state {
        pairs.push(format!("state={}", percent_encode(state)));
        pairs.push("stateExact=true".to_owned());
    }
    format!("{BASE_URL}/stations/search?{}", pairs.join("&"))
}

/// Builds the tag index URL: most used first, broken stations hidden, no
/// artificial cap on the count.
#[must_use]
pub fn tags_url() -> String {
    format!("{BASE_URL}/tags?order=stationcount&reverse=true&hidebroken=true&limit=100000")
}

/// Parses one JSON array of rows, naming the body on failure instead of
/// panicking. Unknown fields are ignored by the wire structs.
fn parse_array<T2: Deserialize>(body: &str) -> Result<Vec<T2>, Error> {
    miniserde::json::from_str(body).map_err(|error| Error::Malformed {
        detail: error.to_string(),
    })
}

/// Percent-encodes one query value: everything outside the RFC 3986
/// unreserved set is escaped, so a name with spaces, `&` or `=` survives
/// the round trip.
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
