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
//!
//! The same parse, without the station-only filter, is [`entries`]: every
//! entry the body carries, used by `velo playlist import` (issue #9), whose
//! saved playlists are paths and not only streams.

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
    let body = body
        .strip_prefix([0xEF, 0xBB, 0xBF].as_slice())
        .unwrap_or(body);
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
    pls_entries(text.as_bytes())
        .into_iter()
        .find(|url| is_playable(url))
        .ok_or(ListsError::NoStream)
}

/// The PLS body's entries with their titles, sorted numerically by `n`,
/// because real files are not written in order; keys are matched
/// case-insensitively. The title is the entry's `Title<n>` value when the
/// body declares one and it is not empty (issue #34). Nothing is filtered:
/// the station resolver picks the first playable one, the playlist importer
/// (issue #9) takes every entry. Whether the body is a PLS at all is the
/// caller's business: the importer dispatches on the file's extension.
fn pls_pairs(body: &[u8]) -> Vec<(String, Option<String>)> {
    let text = text(body);
    let mut files: std::collections::BTreeMap<u32, String> = std::collections::BTreeMap::new();
    let mut titles: std::collections::BTreeMap<u32, String> = std::collections::BTreeMap::new();
    for line in lines(&text) {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        if let Some(index) = key.strip_prefix("file")
            && let Ok(n) = index.trim().parse::<u32>()
        {
            files.entry(n).or_insert_with(|| value.trim().to_string());
        } else if let Some(index) = key.strip_prefix("title")
            && let Ok(n) = index.trim().parse::<u32>()
        {
            titles.entry(n).or_insert_with(|| value.trim().to_string());
        }
    }
    files
        .into_iter()
        .map(|(n, url)| {
            let title = titles.get(&n).filter(|title| !title.is_empty()).cloned();
            (url, title)
        })
        .collect()
}

/// The PLS body's `File<n>` values, sorted numerically by `n`, because real
/// files are not written in order; keys are matched case-insensitively.
/// Nothing is filtered: the station resolver picks the first playable one,
/// the playlist importer (issue #9) takes every entry. Whether the body is
/// a PLS at all is the caller's business: the importer dispatches on the
/// file's extension.
#[must_use]
pub fn pls_entries(body: &[u8]) -> Vec<String> {
    pls_pairs(body).into_iter().map(|(url, _)| url).collect()
}

/// Resolves an M3U body to its first playable entry.
///
/// Tolerates a UTF-8 BOM, `#` comments and blank lines. An HLS manifest is
/// detected before any URL is returned: its entries are `.ts` segments that
/// symphonia cannot decode.
fn unwrap_m3u(text: &str) -> Result<String, ListsError> {
    m3u_entries(text.as_bytes())?
        .into_iter()
        .find(|line| is_playable(line))
        .ok_or(ListsError::NoStream)
}

/// The M3U body's entries with their titles, in file order. An `#EXTINF`
/// line's text after the first comma — `Artist - Title` in the wild — names
/// the entry that follows it (issue #34); an entry with no `#EXTINF` of its
/// own carries no title, and the last `#EXTINF` before an entry wins. An
/// HLS manifest is detected before any entry is returned: its entries are
/// `.ts` segments that symphonia cannot decode.
///
/// # Errors
///
/// [`ListsError::Hls`] for an HLS manifest: its `.ts` segments cannot be
/// decoded, and saying so beats handing back a segment list.
fn m3u_pairs(body: &[u8]) -> Result<Vec<(String, Option<String>)>, ListsError> {
    let text = text(body);
    let all: Vec<&str> = lines(&text).collect();
    if all.iter().any(|line| line.starts_with("#EXT-X-")) {
        return Err(ListsError::Hls);
    }
    let mut pairs = Vec::new();
    let mut pending: Option<String> = None;
    for line in all {
        if let Some(rest) = line.strip_prefix("#EXTINF") {
            pending = rest
                .split_once(',')
                .map(|(_, title)| title.trim().to_owned())
                .filter(|title| !title.is_empty());
        } else if !line.is_empty() && !line.starts_with('#') {
            pairs.push((line.to_string(), pending.take()));
        }
    }
    Ok(pairs)
}

/// The M3U body's entries in file order, `#` comments and blank lines
/// skipped. An HLS manifest is detected before any entry is returned: its
/// entries are `.ts` segments that symphonia cannot decode. No `#EXTM3U`
/// header is required — hand-written playlists often lack it — so the
/// importer can trust the file's `.m3u`/`.m3u8` extension.
///
/// # Errors
///
/// [`ListsError::Hls`] for an HLS manifest: its `.ts` segments cannot be
/// decoded, and saying so beats handing back a segment list.
pub fn m3u_entries(body: &[u8]) -> Result<Vec<String>, ListsError> {
    Ok(m3u_pairs(body)?.into_iter().map(|(url, _)| url).collect())
}

/// Every entry the body carries, in order — the whole parse
/// [`unwrap_station`] starts from, exposed for `velo playlist import`
/// (issue #9): PLS `File<n>` entries sorted numerically by `n`, M3U lines in
/// file order, `#` comments and blank lines skipped. Nothing is filtered:
/// relative paths and local files come through too, because a saved playlist
/// is paths — resolving them and reporting the ones whose file is gone is
/// the importer's job, not the parser's.
///
/// # Errors
///
/// [`ListsError::NotAPlaylist`] when the body is neither format,
/// [`ListsError::Hls`] for an HLS manifest — the same verdicts
/// [`unwrap_station`] makes.
pub fn entries(body: &[u8]) -> Result<Vec<String>, ListsError> {
    Ok(entries_with_titles(body)?
        .into_iter()
        .map(|(url, _)| url)
        .collect())
}

/// Every entry the body carries with the title each one was given, in
/// order — the whole parse [`entries`] starts from, with the wrapper's own
/// names kept beside the URLs (issue #34): a PLS's `Title<n>` value, an
/// M3U's `#EXTINF` text after the first comma, each naming the entry it
/// precedes. Titles are what the wrapper declares and nothing more: an
/// entry the wrapper did not name carries no title, and no name is ever
/// invented from the URL. The same verdicts as [`entries`]:
///
/// # Errors
///
/// [`ListsError::NotAPlaylist`] when the body is neither format,
/// [`ListsError::Hls`] for an HLS manifest — the same verdicts
/// [`unwrap_station`] makes.
pub fn entries_with_titles(body: &[u8]) -> Result<Vec<(String, Option<String>)>, ListsError> {
    let text = text(body);
    // Sniff on the first non-blank line so leading whitespace stays harmless.
    let first = lines(&text).find(|line| !line.is_empty());
    match first {
        Some(line) if line.eq_ignore_ascii_case("[playlist]") => Ok(pls_pairs(body)),
        Some(line) if line.starts_with("#EXTM3U") => m3u_pairs(body),
        _ => Err(ListsError::NotAPlaylist),
    }
}
