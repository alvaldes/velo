//! The Subsonic client: URL building, the wire envelope and the probe
//! (T12a).
//!
//! The wire format is JSON (`f=json`) — a server that cannot answer JSON is
//! reported as too old, never parsed as XML. The one network primitive is
//! the [`Transport`] seam: the client builds the URL, the transport fetches
//! it, and every test injects canned bodies instead of a network.
//!
//! Authentication shape, decided once: every request carries `u` (user),
//! `v` (API version), `c=velo` (client name) and `f=json`, plus either
//! `apiKey=<secret>` when the server advertises the `apiKeyAuthentication`
//! extension, or `t=md5(password + salt)` + `s=<salt>` otherwise, with a
//! fresh salt per request — the token cannot be stored because it depends
//! on the salt.

use std::fmt;
use std::io::Read as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use md5::{Digest as _, Md5};
use miniserde::Deserialize;

use super::Error;

/// The Subsonic REST API version velo speaks.
pub const API_VERSION: &str = "1.16.1";

/// The client name sent as `c` with every request.
const CLIENT_NAME: &str = "velo";

/// The token auth credential: `t = md5(password + salt)`, a fresh salt per
/// request, so the token itself is never stored anywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Auth {
    /// `t` + `s`, derived from the password.
    Token { password: String },
    /// `apiKey`, for a server that advertises `apiKeyAuthentication`.
    ApiKey { key: String },
}

/// How the probe authenticated — what `velo --subsonic-test` reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthKind {
    /// Token + salt, from the password.
    Token,
    /// The API key parameter.
    ApiKey,
}

impl fmt::Display for AuthKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Token => "token + salt (md5 of the password)",
            Self::ApiKey => "api key",
        })
    }
}

/// The token auth digest: `md5(password + salt)`, hex. Pinned by the test
/// against the example in the Subsonic API documentation.
#[must_use]
pub fn token(password: &str, salt: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(password.as_bytes());
    hasher.update(salt.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// A fresh hex salt: wall-clock nanos plus a process counter, so two
/// requests never share a token.
fn fresh_salt() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0_u128, |since| since.as_nanos());
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:x}{counter:x}")
}

/// Percent-encodes one query value: everything outside the RFC 3986
/// unreserved set is escaped, so a password or a path with spaces, `&` or
/// `=` survives the round trip.
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

/// Builds one request URL.
///
/// `base_url` may carry a trailing slash (harmless) and an existing query
/// string (joined with `&`, with `/rest/<endpoint>` inserted before the
/// `?`). `extra` carries endpoint-specific parameters, already in order.
#[must_use]
pub fn build_request_url(
    base_url: &str,
    user: &str,
    auth: &Auth,
    salt: &str,
    endpoint: &str,
    extra: &[(&str, &str)],
) -> String {
    let base = base_url.trim_end_matches('/');
    let (path, existing_query) = match base.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (base, None),
    };

    let mut pairs: Vec<String> = Vec::new();
    if let Some(query) = existing_query {
        pairs.push(query.to_owned());
    }
    pairs.push(format!("u={}", percent_encode(user)));
    pairs.push(format!("v={API_VERSION}"));
    pairs.push(format!("c={CLIENT_NAME}"));
    pairs.push("f=json".to_owned());
    match auth {
        Auth::Token { password } => {
            pairs.push(format!("t={}", token(password, salt)));
            pairs.push(format!("s={}", percent_encode(salt)));
        }
        Auth::ApiKey { key } => pairs.push(format!("apiKey={}", percent_encode(key))),
    }
    for (name, value) in extra {
        pairs.push(format!(
            "{}={}",
            percent_encode(name),
            percent_encode(value)
        ));
    }
    format!("{path}/rest/{endpoint}?{}", pairs.join("&"))
}

/// The seam every request goes through: one GET, the body bytes back. The
/// production implementation is [`HttpTransport`]; tests inject canned
/// bodies and never touch the network.
pub trait Transport {
    /// Performs one GET of `url` and returns the body bytes. Transport
    /// trouble — refused connection, DNS failure, a non-200 answer — is
    /// [`Error::Unreachable`], never a credential problem.
    ///
    /// # Errors
    ///
    /// Fails when the request cannot be completed.
    fn get(&self, url: &str) -> Result<Vec<u8>, Error>;
}

impl<T: Transport + ?Sized> Transport for &T {
    fn get(&self, url: &str) -> Result<Vec<u8>, Error> {
        (**self).get(url)
    }
}

/// The production [`Transport`]: one blocking GET at a time over `ureq`
/// with rustls — no async stack, which fits the player's synchronous shape.
pub struct HttpTransport {
    agent: ureq::Agent,
}

impl HttpTransport {
    /// Builds the transport. `insecure` (the config's `insecure`) accepts
    /// self-signed certificates, for a server on the home network.
    ///
    /// # Errors
    ///
    /// Never fails today; the result keeps the seam free to grow.
    pub fn new(insecure: bool) -> Result<Self, Error> {
        let mut tls = ureq::tls::TlsConfig::builder();
        if insecure {
            tls = tls.disable_verification(true);
        }
        let agent = ureq::Agent::config_builder()
            .tls_config(tls.build())
            .timeout_global(Some(std::time::Duration::from_secs(30)))
            .build()
            .new_agent();
        Ok(Self { agent })
    }
}

impl Transport for HttpTransport {
    fn get(&self, url: &str) -> Result<Vec<u8>, Error> {
        let mut response = self
            .agent
            .get(url)
            .call()
            .map_err(|error| Error::Unreachable {
                detail: match error {
                    ureq::Error::StatusCode(status) => format!("the server answered HTTP {status}"),
                    other => other.to_string(),
                },
            })?;
        let status = response.status();
        if status != 200 {
            return Err(Error::Unreachable {
                detail: format!("the server answered HTTP {status}"),
            });
        }
        let mut body = Vec::new();
        response
            .body_mut()
            .as_reader()
            .read_to_end(&mut body)
            .map_err(|error| Error::Unreachable {
                detail: error.to_string(),
            })?;
        Ok(body)
    }
}

/// The parsed `subsonic-response` envelope, what a request answers with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// `ok` or `failed`; failures become [`Error::Server`].
    pub status: String,
    /// The server's REST API version (`v` echo, e.g. `1.16.1`).
    pub version: Option<String>,
    /// The server's own name (`type`), e.g. `Navidrome`.
    pub server_type: Option<String>,
    /// The server's own version (`serverVersion`).
    pub server_version: Option<String>,
    /// Whether the server says `openSubsonic: true`.
    pub open_subsonic: bool,
    /// The `OpenSubsonic` extensions the answer advertises (only
    /// `getOpenSubsonicExtensions` answers with any).
    pub extensions: Vec<String>,
}

/// What `velo --subsonic-test` reports about one server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInfo {
    /// The server's own name (`type`), e.g. `Navidrome`.
    pub server_type: Option<String>,
    /// The server's own version (`serverVersion`).
    pub server_version: Option<String>,
    /// The REST API version the server answers with.
    pub api_version: Option<String>,
    /// Whether the server says `openSubsonic: true`.
    pub open_subsonic: bool,
    /// The `OpenSubsonic` extensions the server advertises, by name.
    pub extensions: Vec<String>,
}

/// The outcome of one probe: what the server is and which authentication
/// was verified against it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeReport {
    /// What the server is.
    pub server: ServerInfo,
    /// The authentication that was actually used and verified.
    pub auth: AuthKind,
}

/// The client for one server. `T` is the [`Transport`].
#[derive(Debug)]
pub struct Client<T> {
    transport: T,
    base_url: String,
    user: String,
    auth: Auth,
}

impl<T: Transport> Client<T> {
    /// A client for one server, authenticating the given way.
    #[must_use]
    pub fn new(transport: T, base_url: &str, user: &str, auth: Auth) -> Self {
        Self {
            transport,
            base_url: base_url.trim_end_matches('/').to_owned(),
            user: user.to_owned(),
            auth,
        }
    }

    /// The same client authenticating another way, sharing the transport.
    #[must_use]
    pub fn with_auth(&self, auth: Auth) -> Client<&T> {
        Client {
            transport: &self.transport,
            base_url: self.base_url.clone(),
            user: self.user.clone(),
            auth,
        }
    }

    /// Performs one request: builds the URL with a fresh salt, fetches it
    /// through the transport, and either returns the parsed envelope or
    /// maps the server's error to [`Error::Server`].
    ///
    /// # Errors
    ///
    /// Fails with the transport's [`Error::Unreachable`], the server's
    /// [`Error::Server`], or [`Error::TooOld`] when the answer is not JSON.
    pub fn request(&self, endpoint: &str, extra: &[(&str, &str)]) -> Result<Response, Error> {
        let body = self.bytes(endpoint, extra)?;
        parse_response(&body)
    }

    /// Builds one request URL and fetches its body through the transport.
    /// Every endpoint method goes through here, so paging and endpoints
    /// share one URL shape.
    fn bytes(&self, endpoint: &str, extra: &[(&str, &str)]) -> Result<Vec<u8>, Error> {
        let salt = fresh_salt();
        let url = build_request_url(
            &self.base_url,
            &self.user,
            &self.auth,
            &salt,
            endpoint,
            extra,
        );
        self.transport.get(&url)
    }

    /// `ping`: the cheapest authenticated request, the probe's first call.
    ///
    /// # Errors
    ///
    /// As [`Client::request`].
    pub fn ping(&self) -> Result<Response, Error> {
        self.request("ping", &[])
    }

    /// `getOpenSubsonicExtensions`: what the server advertises. An old
    /// server that misses the endpoint or refuses it is not fatal — it just
    /// means no API key and no `ReplayGain` later.
    ///
    /// # Errors
    ///
    /// As [`Client::request`].
    pub fn open_subsonic_extensions(&self) -> Result<Vec<String>, Error> {
        Ok(self.request("getOpenSubsonicExtensions", &[])?.extensions)
    }

    /// The base URL every request is built from: no trailing slash, ready
    /// for `/rest/…` to be appended.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// `stream?id=<id>&format=raw`: the track's original bytes, fetched for
    /// the cache (T12c). `format=raw` asks for no transcoding, so what lands
    /// on disk is what the server holds — that is what makes bit-perfect and
    /// `ReplayGain`-from-tags honest over a remote track.
    ///
    /// A failed stream answers with the JSON error envelope, not audio: it
    /// is mapped to the server's own sentence. Any other body — and a real
    /// track's bytes are never JSON — is the track itself.
    ///
    /// # Errors
    ///
    /// As [`Client::bytes`], plus [`Error::Server`] when the server answers
    /// the stream request with its error envelope.
    pub fn stream_bytes(&self, id: &str) -> Result<Vec<u8>, Error> {
        let body = self.bytes("stream", &[("id", id), ("format", "raw")])?;
        match parse_response(&body) {
            Err(error @ Error::Server { .. }) => Err(error),
            _ => Ok(body),
        }
    }

    /// `getAlbumList2`: one page of the server's albums, in the given
    /// listing order — the sync (T12b) pages through them with
    /// `alphabeticalByName`, `size` albums at a time starting at `offset`.
    ///
    /// # Errors
    ///
    /// As [`Client::request`], plus [`Error::Server`] when an `ok` answer
    /// carries no album list at all.
    pub fn album_list2(
        &self,
        list_type: &str,
        size: u32,
        offset: u64,
    ) -> Result<Vec<RemoteAlbum>, Error> {
        let body = self.bytes(
            "getAlbumList2",
            &[
                ("type", list_type),
                ("size", &size.to_string()),
                ("offset", &offset.to_string()),
            ],
        )?;
        let envelope: AlbumListEnvelope = json_envelope(&body)?;
        require_ok(&envelope.response.status, envelope.response.error)?;
        let list = envelope.response.album_list2.ok_or(Error::Server {
            code: 0,
            message: "the answer said ok but carried no album list".to_owned(),
        })?;
        Ok(list
            .album
            .unwrap_or_default()
            .into_iter()
            .map(|album| RemoteAlbum {
                id: album.id,
                name: album.name.unwrap_or_default(),
                artist: album.artist,
                year: album.year,
            })
            .collect())
    }

    /// `getAlbum`: one album with its songs — what the sync (T12b) maps one
    /// request per album, not one per track.
    ///
    /// # Errors
    ///
    /// As [`Client::request`], plus [`Error::Server`] when an `ok` answer
    /// carries no album at all.
    pub fn album(&self, id: &str) -> Result<RemoteAlbumDetail, Error> {
        let body = self.bytes("getAlbum", &[("id", id)])?;
        let envelope: AlbumEnvelope = json_envelope(&body)?;
        require_ok(&envelope.response.status, envelope.response.error)?;
        let raw = envelope.response.album.ok_or(Error::Server {
            code: 0,
            message: "the answer said ok but carried no album".to_owned(),
        })?;
        Ok(RemoteAlbumDetail {
            id: raw.id,
            name: raw.name.unwrap_or_default(),
            artist: raw.artist,
            year: raw.year,
            songs: raw
                .songs
                .unwrap_or_default()
                .into_iter()
                .map(|song| RemoteSong {
                    id: song.id,
                    title: song.title,
                    artist: song.artist,
                    duration: song.duration,
                    track: song.track,
                    disc_number: song.disc_number,
                    year: song.year,
                })
                .collect(),
        })
    }

    /// Asks the server what it is and which authentication works, then
    /// reports both. This is what `velo --subsonic-test` runs.
    ///
    /// The order: a token-auth ping (every Subsonic-compatible server
    /// accepts it, and it needs no prior knowledge of extensions), the
    /// extension list, and — when the server advertises
    /// `apiKeyAuthentication` — an api-key ping to prefer the API key. A
    /// token ping refused as wrong credentials gets one api-key retry,
    /// because the stored secret may be a key rather than a password.
    ///
    /// # Errors
    ///
    /// Fails with the first ping's error when neither authentication works.
    pub fn probe(
        transport: T,
        base_url: &str,
        user: &str,
        secret: &str,
    ) -> Result<ProbeReport, Error> {
        let client = Client::new(
            transport,
            base_url,
            user,
            Auth::Token {
                password: secret.to_owned(),
            },
        );

        let mut auth = AuthKind::Token;
        let ping = match client.ping() {
            Ok(response) => response,
            Err(token_error @ Error::Server { code: 40, .. }) => {
                let key_client = client.with_auth(Auth::ApiKey {
                    key: secret.to_owned(),
                });
                match key_client.ping() {
                    Ok(response) => {
                        auth = AuthKind::ApiKey;
                        response
                    }
                    // The server's own message rides on the first error.
                    Err(_) => return Err(token_error),
                }
            }
            Err(other) => return Err(other),
        };

        let extensions = client
            .with_auth(match auth {
                AuthKind::Token => Auth::Token {
                    password: secret.to_owned(),
                },
                AuthKind::ApiKey => Auth::ApiKey {
                    key: secret.to_owned(),
                },
            })
            .open_subsonic_extensions()
            .unwrap_or_default();

        // Prefer the API key when the server advertises it and accepts the
        // secret as one.
        if auth == AuthKind::Token && extensions.iter().any(|name| name == "apiKeyAuthentication") {
            let key_client = client.with_auth(Auth::ApiKey {
                key: secret.to_owned(),
            });
            if key_client.ping().is_ok() {
                auth = AuthKind::ApiKey;
            }
        }

        Ok(ProbeReport {
            server: ServerInfo {
                server_type: ping.server_type,
                server_version: ping.server_version,
                api_version: ping.version,
                open_subsonic: ping.open_subsonic,
                extensions,
            },
            auth,
        })
    }
}

/// One album of the server's library, as `getAlbumList2` names it: the id
/// the sync fetches songs with, plus what the index's album views show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteAlbum {
    /// The server's album id, the `id` of the `getAlbum` call.
    pub id: String,
    /// The album name.
    pub name: String,
    /// The album artist, when the server says one.
    pub artist: Option<String>,
    /// The album year, when the server says one.
    pub year: Option<i64>,
}

/// One album with its songs, what `getAlbum` answers.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteAlbumDetail {
    /// The server's album id.
    pub id: String,
    /// The album name.
    pub name: String,
    /// The album artist, when the server says one.
    pub artist: Option<String>,
    /// The album year, when the server says one.
    pub year: Option<i64>,
    /// The album's songs, in the server's order.
    pub songs: Vec<RemoteSong>,
}

/// One song of one remote album.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteSong {
    /// The server's song id — the `remote_id` of the index row.
    pub id: String,
    /// The song title, when the server says one.
    pub title: Option<String>,
    /// The song artist, when the server says one.
    pub artist: Option<String>,
    /// The song length in seconds, when the server says one.
    pub duration: Option<f64>,
    /// The track number inside the album.
    pub track: Option<i64>,
    /// The disc number inside the album.
    pub disc_number: Option<i64>,
    /// The song year.
    pub year: Option<i64>,
}

/// The serde shape of the JSON envelope.
#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "subsonic-response")]
    response: RawResponse,
}

/// The serde shape of a `getAlbumList2` envelope.
#[derive(Deserialize)]
struct AlbumListEnvelope {
    #[serde(rename = "subsonic-response")]
    response: RawAlbumListResponse,
}

#[derive(Deserialize)]
struct RawAlbumListResponse {
    status: String,
    error: Option<RawError>,
    #[serde(rename = "albumList2")]
    album_list2: Option<RawAlbumList>,
}

#[derive(Deserialize)]
struct RawAlbumList {
    album: Option<Vec<RawAlbum>>,
}

#[derive(Deserialize)]
struct RawAlbum {
    id: String,
    name: Option<String>,
    artist: Option<String>,
    year: Option<i64>,
}

/// The serde shape of a `getAlbum` envelope.
#[derive(Deserialize)]
struct AlbumEnvelope {
    #[serde(rename = "subsonic-response")]
    response: RawAlbumResponse,
}

#[derive(Deserialize)]
struct RawAlbumResponse {
    status: String,
    error: Option<RawError>,
    album: Option<RawAlbumDetail>,
}

#[derive(Deserialize)]
struct RawAlbumDetail {
    id: String,
    name: Option<String>,
    artist: Option<String>,
    year: Option<i64>,
    #[serde(rename = "song")]
    songs: Option<Vec<RawSong>>,
}

#[derive(Deserialize)]
struct RawSong {
    id: String,
    title: Option<String>,
    artist: Option<String>,
    duration: Option<f64>,
    track: Option<i64>,
    #[serde(rename = "discNumber")]
    disc_number: Option<i64>,
    year: Option<i64>,
}

#[derive(Deserialize)]
struct RawResponse {
    status: String,
    version: Option<String>,
    #[serde(rename = "type")]
    server_type: Option<String>,
    #[serde(rename = "serverVersion")]
    server_version: Option<String>,
    #[serde(rename = "openSubsonic")]
    open_subsonic: Option<bool>,
    #[serde(rename = "openSubsonicExtensions")]
    extensions: Option<Vec<Extension>>,
    error: Option<RawError>,
}

#[derive(Deserialize)]
struct RawError {
    code: u32,
    message: Option<String>,
}

/// Parses one `subsonic-response` body. A body that is not JSON is a server
/// too old for velo — a classic XML-only Subsonic — never XML to parse.
fn parse_response(body: &[u8]) -> Result<Response, Error> {
    let too_old = || Error::TooOld {
        detail: format!(
            "the answer was {} bytes, not JSON: {}",
            body.len(),
            String::from_utf8_lossy(&body[..body.len().min(120)])
        ),
    };
    let text = std::str::from_utf8(body).map_err(|_| too_old())?;
    let envelope: Envelope = miniserde::json::from_str(text).map_err(|_| too_old())?;
    let raw = envelope.response;
    require_ok(&raw.status, raw.error)?;
    Ok(Response {
        status: raw.status,
        version: raw.version,
        server_type: raw.server_type,
        server_version: raw.server_version,
        open_subsonic: raw.open_subsonic.unwrap_or(false),
        extensions: raw
            .extensions
            .unwrap_or_default()
            .into_iter()
            .map(|extension| extension.name)
            .collect(),
    })
}

/// Parses one `subsonic-response` body carrying an endpoint payload: same
/// too-old rule as [`parse_response`], same failed-envelope mapping.
fn json_envelope<T: Deserialize>(body: &[u8]) -> Result<T, Error> {
    let too_old = || Error::TooOld {
        detail: format!(
            "the answer was {} bytes, not JSON: {}",
            body.len(),
            String::from_utf8_lossy(&body[..body.len().min(120)])
        ),
    };
    let text = std::str::from_utf8(body).map_err(|_| too_old())?;
    let envelope: T = miniserde::json::from_str(text).map_err(|_| too_old())?;
    Ok(envelope)
}

/// Refuses a `failed` envelope with the server's own code and message.
fn require_ok(status: &str, error: Option<RawError>) -> Result<(), Error> {
    if status == "ok" {
        return Ok(());
    }
    let (code, message) = match error {
        Some(error) => (
            error.code,
            error
                .message
                .unwrap_or_else(|| "the answer said failed without a message".to_owned()),
        ),
        None => (0, "the answer said failed without an error".to_owned()),
    };
    Err(Error::Server { code, message })
}

/// One advertised `OpenSubsonic` extension: only the name matters here.
#[derive(Deserialize)]
struct Extension {
    name: String,
}
