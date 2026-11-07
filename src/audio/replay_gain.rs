//! `ReplayGain` in the audio path (T11, S15): read the tags, choose a gain, cap
//! it against the declared peak, and multiply the samples once, on the
//! producer's way into the ring.
//!
//! The parsing is pure: strings in, numbers out. [`Gain::read_from`] is the
//! only part that touches a file, and everything else is testable without one.
//!
//! Clipping and honesty: when the track declares a peak, the applied gain is
//! capped so `peak × gain ≤ 1.0`, which is [`capped_gain`]'s whole contract —
//! a positive gain cannot push a track past full scale. When the peak is
//! *missing*, the gain is applied as read and clipping is possible; that is
//! the honest choice, because inventing a peak (1.0 or otherwise) would change
//! the music in a way nobody asked for. An unparseable tag yields no gain at
//! all rather than a wrong one.
//!
//! The gain belongs to a track: the producer recomputes it whenever the track
//! changes, never once per queue, because the queue is shared and its entries
//! change while it plays (T18).

use std::path::Path;

use lofty::file::TaggedFileExt;
use lofty::tag::ItemKey;

use crate::config::ReplayGainMode;

/// The `ReplayGain` values a file's primary tag can declare, already parsed.
///
/// Gains are in dB; peaks are linear, 1.0 being full scale.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Loudness {
    /// `REPLAYGAIN_TRACK_GAIN`, in dB.
    pub track_gain_db: Option<f32>,
    /// `REPLAYGAIN_TRACK_PEAK`, linear.
    pub track_peak: Option<f32>,
    /// `REPLAYGAIN_ALBUM_GAIN`, in dB.
    pub album_gain_db: Option<f32>,
    /// `REPLAYGAIN_ALBUM_PEAK`, linear.
    pub album_peak: Option<f32>,
}

/// Parses a `ReplayGain` dB value: `-7.35 dB`, `-7.35dB`, `-7.35`, with any
/// surrounding whitespace and an optional unit. Anything else — garbage,
/// `inf`, `NaN`, trailing text — is no gain at all, never a wrong one.
#[must_use]
pub fn parse_db(value: &str) -> Option<f32> {
    let db = strip_db_unit(value).parse::<f32>().ok()?;
    db.is_finite().then_some(db)
}

/// Drops an optional, case-insensitive `dB` unit and the whitespace around it.
fn strip_db_unit(value: &str) -> &str {
    let text = value.trim();
    match text.char_indices().nth_back(1) {
        Some((index, _)) if text[index..].eq_ignore_ascii_case("db") => text[..index].trim_end(),
        _ => text,
    }
}

/// Parses a `ReplayGain` peak: a finite, non-negative number like `0.987654`.
#[must_use]
pub fn parse_peak(value: &str) -> Option<f32> {
    let peak = value.trim().parse::<f32>().ok()?;
    (peak.is_finite() && peak >= 0.0).then_some(peak)
}

/// The gain the producer applies to every sample of a track, once, on its way
/// into the ring — before the user's own volume and its ramp, which stay
/// exactly what they were.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gain {
    /// The linear multiplier; `None` is "no adjustment", not "multiply by 0".
    factor: Option<f32>,
}

impl Gain {
    /// The gain that multiplies nothing.
    #[must_use]
    pub const fn identity() -> Self {
        Self { factor: None }
    }

    /// Chooses the gain for one track from its parsed loudness, purely.
    ///
    /// `track` uses the track tags, `album` prefers the album tags and falls
    /// back to the track tags, `off` applies nothing. The peak that caps the
    /// gain is the one belonging to the gain that was chosen. A missing or
    /// unusable value is no gain, never a wrong one.
    #[must_use]
    pub fn for_track(mode: ReplayGainMode, loudness: Option<&Loudness>) -> Self {
        let Some(loudness) = loudness else {
            return Self::identity();
        };
        let (gain_db, peak) = match mode {
            ReplayGainMode::Off => return Self::identity(),
            ReplayGainMode::Track => (loudness.track_gain_db, loudness.track_peak),
            ReplayGainMode::Album => match loudness.album_gain_db {
                Some(album_gain_db) => (Some(album_gain_db), loudness.album_peak),
                None => (loudness.track_gain_db, loudness.track_peak),
            },
        };
        let Some(gain_db) = gain_db else {
            return Self::identity();
        };
        Self {
            factor: Some(capped_gain(db_to_linear(gain_db), peak)),
        }
    }

    /// Reads `path`'s `ReplayGain` tags and resolves them under `mode`.
    ///
    /// The values are read from the file's primary tag (its first tag, when it
    /// has no primary one), where lofty has already mapped ID3 `TXXX`
    /// descriptions, Vorbis comment keys and APE keys onto
    /// [`ItemKey::ReplayGainTrackGain`] and its three siblings. A file that
    /// cannot be read, has no usable tag, or carries an unparseable value
    /// yields [`Gain::identity`]: a track without usable tags keeps its own
    /// loudness, it never gets a wrong one.
    #[must_use]
    pub fn read_from(path: &Path, mode: ReplayGainMode) -> Self {
        let Some(loudness) = read_loudness(path) else {
            return Self::identity();
        };
        Self::for_track(mode, Some(&loudness))
    }

    /// Whether this gain must touch the samples at all.
    #[must_use]
    pub const fn applies(&self) -> bool {
        self.factor.is_some()
    }

    /// The linear multiplier; 1.0 when there is no gain.
    #[must_use]
    pub const fn factor(&self) -> f32 {
        match self.factor {
            Some(factor) => factor,
            None => 1.0,
        }
    }

    /// Multiplies `samples` in place, once per block; a no-op without a gain.
    pub fn apply(&self, samples: &mut [f32]) {
        if let Some(factor) = self.factor {
            for sample in samples {
                *sample *= factor;
            }
        }
    }
}

/// Caps a linear factor against a declared peak so `peak × factor ≤ 1.0`.
///
/// Exactly full scale is kept. A missing, zero or negative peak cannot cap
/// anything, so the factor is returned as read. Pure by design: the boundary
/// is where the rule lives.
#[must_use]
pub fn capped_gain(linear: f32, peak: Option<f32>) -> f32 {
    match peak {
        Some(peak) if peak > 0.0 && linear * peak > 1.0 => 1.0 / peak,
        _ => linear,
    }
}

/// Converts a `ReplayGain` dB value into its linear factor: `10^(dB / 20)`.
fn db_to_linear(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

/// Reads the four `ReplayGain` values from a file's primary tag.
fn read_loudness(path: &Path) -> Option<Loudness> {
    let tagged = lofty::read_from_path(path).ok()?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;
    Some(Loudness {
        track_gain_db: tag
            .get_string(ItemKey::ReplayGainTrackGain)
            .and_then(parse_db),
        track_peak: tag
            .get_string(ItemKey::ReplayGainTrackPeak)
            .and_then(parse_peak),
        album_gain_db: tag
            .get_string(ItemKey::ReplayGainAlbumGain)
            .and_then(parse_db),
        album_peak: tag
            .get_string(ItemKey::ReplayGainAlbumPeak)
            .and_then(parse_peak),
    })
}
