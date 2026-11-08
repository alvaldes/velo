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

/// Whether a station's `codec`, as the directory spells it, is one
/// symphonia has compiled in.
///
/// The table below only translates the directory's spellings to symphonia's
/// [`CodecType`]s; the accepted set itself is decided by asking the default
/// codec registry whether each type is registered. A codec absent from the
/// build — opus today, maybe compiled in tomorrow — answers `false` with no
/// change here beyond adding its spelling to the table.
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
        if registry.get_audio_decoder(*codec_id).is_some() {
            return true;
        }
    }
    !recognised
}
