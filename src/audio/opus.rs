//! Opus decoding via a statically linked libopus (T14).
//!
//! symphonia ships no Opus decoder in 0.5 or 0.6, but its Ogg reader *does*
//! recognise the Opus mapping (identification header, `OpusTags`, packet
//! durations), so the container half stays with symphonia and only the codec
//! half is bound here, through `audiopus` 0.2 over `audiopus_sys`, which
//! builds libopus statically — the single-binary promise holds (L47: ~400 KB
//! on an 8.9 MB binary, no dynamic linkage).
//!
//! Two facts this module is built around: Opus **always** decodes at 48 kHz
//! regardless of the rate it was encoded at, and the decoder must be told
//! mono or stereo at construction. A packet carries its channel count in its
//! TOC byte, so a packet that disagrees with the decoder it reaches is
//! reported as an error rather than silently mixed — a downmix is a product
//! decision, not a codec detail.
//!
//! Truncation: a *single-frame* packet truncated to fewer bytes is a different
//! valid packet as far as RFC 6716 §3.1 is concerned (a code-0 frame's length
//! is the rest of the packet), so no packet-level decoder can refuse one;
//! losing the tail of a stream is refused by the Ogg container before such a
//! packet is ever decoded, and a multi-frame packet whose frame table overruns
//! its bytes is refused here. Both are pinned in `tests/opus.rs`.

use symphonia::core::codecs::audio::AudioCodecId;
use symphonia::core::codecs::audio::well_known::CODEC_ID_OPUS;

use crate::audio::AudioError;

/// The only rate Opus decodes to, whatever it was encoded at.
pub const SAMPLE_RATE: u32 = 48_000;

/// The most frames one Opus packet can decode to: 120 ms at 48 kHz.
const MAX_PACKET_FRAMES: usize = 5_760;

/// Answers whether this codec id can be decoded — true for Opus and, so far,
/// nothing else.
///
/// This lives in one place so the player and the radio stream cannot disagree
/// about which codecs are playable (L47).
#[must_use]
pub fn decodable(id: AudioCodecId) -> bool {
    id == CODEC_ID_OPUS
}

/// The TOC byte's stereo flag (RFC 6716 §3.1: bit 2 of the first byte).
const TOC_STEREO_FLAG: u8 = 0x04;

/// Decodes Opus packets into interleaved `f32` frames at [`SAMPLE_RATE`].
///
/// Constructed for a fixed channel count: every packet handed to
/// [`OpusDecoder::decode_packet`] must agree with it, and the decoder keeps
/// cross-packet state, so [`OpusDecoder::reset`] belongs after every seek or
/// stream change.
pub struct OpusDecoder {
    decoder: audiopus::coder::Decoder,
    channels: u16,
    /// Decode scratch, reused so a packet allocates only its output.
    scratch: Vec<f32>,
}

impl OpusDecoder {
    /// Opens a decoder for `channels` (1 or 2) at [`SAMPLE_RATE`].
    ///
    /// # Errors
    ///
    /// [`AudioError::Unsupported`] for a channel count Opus cannot decode and
    /// [`AudioError::Decode`] when libopus itself refuses.
    pub fn new(channels: u16) -> Result<Self, AudioError> {
        let lib_channels = match channels {
            1 => audiopus::Channels::Mono,
            2 => audiopus::Channels::Stereo,
            _ => {
                return Err(AudioError::Unsupported(format!(
                    "Opus decodes mono or stereo, not {channels} channels"
                )));
            }
        };
        let decoder = audiopus::coder::Decoder::new(audiopus::SampleRate::Hz48000, lib_channels)
            .map_err(|error| AudioError::Decode(format!("libopus decoder: {error}")))?;
        Ok(Self {
            decoder,
            channels,
            scratch: vec![0.0; MAX_PACKET_FRAMES * usize::from(channels)],
        })
    }

    /// The channel count every packet must agree with.
    #[must_use]
    pub const fn channels(&self) -> u16 {
        self.channels
    }

    /// Drops the decoder's cross-packet state, as a seek or a stream change
    /// must before the next packet is decoded.
    ///
    /// # Errors
    ///
    /// [`AudioError::Decode`] when libopus refuses.
    pub fn reset(&mut self) -> Result<(), AudioError> {
        use audiopus::coder::GenericCtl;
        self.decoder
            .reset_state()
            .map_err(|error| AudioError::Decode(format!("libopus reset: {error}")))
    }

    /// Decodes one Opus packet into interleaved `f32` samples, one entry per
    /// channel per frame, at [`SAMPLE_RATE`].
    ///
    /// A packet that is empty, truncated, or encoded for a different channel
    /// count than this decoder was opened for is an error — never silence.
    ///
    /// # Errors
    ///
    /// [`AudioError::Decode`] for every refusal above and any libopus error.
    pub fn decode_packet(&mut self, packet: &[u8]) -> Result<Vec<f32>, AudioError> {
        if packet.is_empty() {
            return Err(AudioError::Decode("empty Opus packet".to_owned()));
        }
        let stereo = packet[0] & TOC_STEREO_FLAG != 0;
        if stereo != (self.channels == 2) {
            return Err(AudioError::Decode(format!(
                "Opus packet is {} but the decoder was opened for {} channel{}",
                if stereo { "stereo" } else { "mono" },
                self.channels,
                if self.channels == 1 { "" } else { "s" }
            )));
        }
        let frames = self
            .decoder
            .decode_float(Some(packet), &mut self.scratch, false)
            .map_err(|error| AudioError::Decode(format!("libopus decode: {error}")))?;
        Ok(self.scratch[..frames * usize::from(self.channels)].to_vec())
    }
}
