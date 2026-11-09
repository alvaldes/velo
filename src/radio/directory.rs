//! The station directory model: which listed stations may load, and which
//! codecs may play.
//!
//! Two small decisions live here so the rest of the radio feature never
//! re-derives them:
//!
//! - [`loadable_stations`] keeps the listed stations whose `url_resolved`
//!   is an absolute `http(s)` URL — the only thing a stream can open — and
//!   caps the list at [`STATION_CAP`], what at most loads per place.
//! - [`is_playable_codec`] is the one symphonia seam: the accepted codec
//!   set comes from the codecs actually compiled into symphonia, never a
//!   hardcoded list, so a later symphonia bump (e.g. enabling opus) widens
//!   it without touching any other file.
//! - [`is_playable_station`] is the per-station answer the views use: the
//!   codec seam above, minus the stations whose URL is an HLS manifest —
//!   a list of `.ts` segments nothing compiled here can decode, whatever
//!   the codec field claims.
//! - [`refusal`] is the same answer spelled out: why one station cannot
//!   play, as a sentence a person can read. The marker, the status line
//!   and any report all read it from here, so they can never disagree.

use crate::radio::client::Station;

/// At most this many stations load per place, replacing the queue.
pub const STATION_CAP: usize = 200;

/// Keeps the stations that can load: an absolute `http(s)` `url_resolved`
/// and nothing past [`STATION_CAP`]. Order is preserved, so the directory's
/// own ranking survives the filter.
#[must_use]
pub fn loadable_stations(stations: Vec<Station>) -> Vec<Station> {
    stations
        .into_iter()
        .filter(|station| absolute_http(&station.url_resolved))
        .take(STATION_CAP)
        .collect()
}

/// Whether one URL is an absolute `http` or `https` URL with a non-empty
/// host: the only shape a stream can open. Scheme case does not matter
/// here; identity normalisation is a later concern.
fn absolute_http(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    let Some(rest) = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
    else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    !host.is_empty()
}

/// Whether a station's `codec`, as the directory spells it, is one this build
/// can actually decode.
///
/// The table below only translates the directory's spellings to symphonia's
/// [`AudioCodecId`]s; whether a codec plays is the build's answer, never this
/// file's opinion. Two decoders count: symphonia's own, asked of its registry,
/// and the crate's own module ([`crate::audio::opus::decodable`]). Compiling one
/// of them flips the answer here without a line changing — which is why this
/// seam exists in exactly one function.
///
/// A spelling the table does not know is **allowed**: the directory leaves
/// `codec` blank or says `unknown` often enough that hiding those stations
/// would throw away playable ones, and the alternative is one decode error
/// that names the codec.
#[must_use]
pub fn is_playable_codec(codec: &str) -> bool {
    use symphonia::core::codecs::audio::{AudioCodecId, well_known};

    const SPELLINGS: &[(&str, AudioCodecId)] = &[
        ("mp3", well_known::CODEC_ID_MP3),
        ("aac", well_known::CODEC_ID_AAC),
        ("aac+", well_known::CODEC_ID_AAC),
        ("ogg", well_known::CODEC_ID_VORBIS),
        ("vorbis", well_known::CODEC_ID_VORBIS),
        ("flac", well_known::CODEC_ID_FLAC),
        ("opus", well_known::CODEC_ID_OPUS),
    ];

    let registry = symphonia::default::get_codecs();
    let mut recognised = false;
    for part in codec.split(',') {
        let spelling = part.trim().to_ascii_lowercase();
        let Some((_, codec_id)) = SPELLINGS.iter().find(|(name, _)| *name == spelling) else {
            continue;
        };
        recognised = true;
        if registry.get_audio_decoder(*codec_id).is_some()
            || crate::audio::opus::decodable(*codec_id)
        {
            return true;
        }
    }
    !recognised
}

/// Whether one URL points at an HLS manifest: a path ending in `.m3u8`,
/// case-insensitively, with any query or fragment ignored. The manifest
/// itself is a list of `.ts` segments, and no decoder compiled here plays
/// those — which is true regardless of which codecs the build ships, so
/// the answer cannot evaporate the way a codec table's can.
fn is_hls_manifest(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or_default();
    path.to_ascii_lowercase().ends_with(".m3u8")
}

/// Whether one station can play as things stand: its codec is decodable
/// ([`is_playable_codec`]) and its resolved URL is not an HLS manifest
/// ([`is_hls_manifest`]). This is [`refusal`] turned into a yes-or-no, so
/// the two can never disagree.
///
/// The URL rule is spelled from the directory's own `url_resolved` only —
/// no network, no sniffing. The stream layer keeps its body-level HLS
/// refusal as the backstop for servers that hide the extension.
#[must_use]
pub fn is_playable_station(station: &Station) -> bool {
    refusal(station).is_none()
}

/// Why one station cannot play: `None` when it plays, otherwise the reason
/// as a sentence a person can read — the marker `✗` never says only that,
/// it says this too (S1, issue #19). One place decides it, and
/// [`is_playable_station`] is this answer's yes-or-no shadow.
#[must_use]
pub fn refusal(station: &Station) -> Option<String> {
    refusal_for(
        &station.codec,
        is_playable_codec(&station.codec),
        &station.url_resolved,
    )
}

/// The refusal given the codec spelling, whether this build can decode it,
/// and the resolved URL. Every codec in the spelling table has a decoder in
/// this build, so no real [`Station`] can carry the codec refusal here; the
/// decodability is an argument so a test can state that branch without
/// faking a build. The public entry point [`refusal`] computes exactly this
/// argument and calls this — it never weakens the rule.
#[must_use]
pub fn refusal_for(codec: &str, decodable: bool, url: &str) -> Option<String> {
    if is_hls_manifest(url) {
        return Some(String::from(
            "an HLS manifest: its .ts segments cannot be decoded",
        ));
    }
    if !decodable {
        return Some(format!("the codec {codec} is not in this build"));
    }
    None
}
