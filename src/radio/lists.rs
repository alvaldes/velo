//! Unwrapping wrapper playlists into a playable stream URL.
//!
//! Radio Browser's `url_resolved` frequently points at a one-entry playlist
//! (`.pls` or `.m3u`) instead of audio; playing it directly sounds like a
//! station with no sound and nothing explains why. [`unwrap_station`] sniffs
//! the format by content — Content-Type lies in the wild and `.m3u` is also
//! what HLS uses — and resolves to the first playable entry, which means an
//! absolute `http` or `https` URL.
//!
//! An HLS body is a distinct failure, not a stream: its entries are `.ts`
//! segments no decoder here can play, so it is reported as [`ListsError::Hls`]
//! instead of a segment URL. A wrapper of a known format that yields nothing
//! playable is [`ListsError::NoStream`], naming what was wrong, and anything
//! else is [`ListsError::NotAPlaylist`]. Invalid UTF-8 never panics: the body
//! is scanned lossily, and a station URL is ASCII anyway.

/// Why [`unwrap_station`] could not hand back a stream URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListsError {
    /// The body is neither a PLS nor an M3U playlist.
    NotAPlaylist,
    /// The body is an HLS manifest; its `.ts` segments cannot be decoded.
    Hls,
    /// The body is a known wrapper format but carries no playable entry.
    NoStream,
}

impl std::fmt::Display for ListsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAPlaylist => write!(f, "body is neither a PLS nor an M3U playlist"),
            Self::Hls => write!(f, "HLS manifest: its .ts segments cannot be decoded"),
            Self::NoStream => write!(f, "playlist contains no playable http(s) URL"),
        }
    }
}

impl std::error::Error for ListsError {}

/// True when the trimmed line is an absolute `http` or `https` URL.
fn is_playable(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// The body as text: a leading UTF-8 BOM stripped, everything else lossy.
/// Invalid UTF-8 must not crash, and every token a playlist decision needs
/// (`[playlist]`, `#EXTM3U`, `File<n>=`, `http` URLs) is ASCII and survives
/// the substitution.
fn text(body: &[u8]) -> std::borrow::Cow<'_, str> {
    let body = body.strip_prefix([0xEF, 0xBB, 0xBF].as_slice()).unwrap_or(body);
    String::from_utf8_lossy(body)
}

/// The body's lines, each trimmed of surrounding whitespace and `\r`.
fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines().map(str::trim)
}

/// Sniffs the wrapper format by content and resolves to the first playable
/// entry, already trimmed.
///
/// `[playlist]` (case-insensitive, possibly after a BOM) means PLS, a leading
/// `#EXTM3U` means M3U. Entries whose URL is not an absolute `http`/`https`
/// URL are skipped; the first playable one wins.
///
/// # Errors
///
/// [`ListsError::NotAPlaylist`] when the body is neither format,
/// [`ListsError::Hls`] for an HLS manifest and [`ListsError::NoStream`] when
/// the wrapper carries no playable entry.
pub fn unwrap_station(body: &[u8]) -> Result<String, ListsError> {
    let text = text(body);
    // Sniff on the first non-blank line so leading whitespace stays harmless.
    let first = lines(&text).find(|line| !line.is_empty());
    match first {
        Some(line) if line.eq_ignore_ascii_case("[playlist]") => unwrap_pls(&text),
        Some(line) if line.starts_with("#EXTM3U") => unwrap_m3u(&text),
        _ => Err(ListsError::NotAPlaylist),
    }
}

/// Resolves a PLS body to its first playable entry.
///
/// `File<n>=` keys are matched case-insensitively and the entries are sorted
/// numerically by `n`, because real files are not written in order.
fn unwrap_pls(text: &str) -> Result<String, ListsError> {
    let mut entries = std::collections::BTreeMap::new();
    for line in lines(text) {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        if let Some(index) = key.strip_prefix("file")
            && let Ok(n) = index.trim().parse::<u32>()
        {
            entries.entry(n).or_insert_with(|| value.trim().to_string());
        }
    }
    entries
        .into_values()
        .find(|url| is_playable(url))
        .ok_or(ListsError::NoStream)
}

/// Resolves an M3U body to its first playable entry.
///
/// Tolerates a UTF-8 BOM, `#` comments and blank lines. An HLS manifest is
/// detected before any URL is returned: its entries are `.ts` segments that
/// symphonia cannot decode.
fn unwrap_m3u(text: &str) -> Result<String, ListsError> {
    let all: Vec<&str> = lines(text).collect();
    if all.iter().any(|line| line.starts_with("#EXT-X-")) {
        return Err(ListsError::Hls);
    }
    all.into_iter()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .find(|line| is_playable(line))
        .map(str::to_string)
        .ok_or(ListsError::NoStream)
}
