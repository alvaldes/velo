//! Opus decoding via a statically linked libopus (T14).
//!
//! symphonia ships no Opus decoder in 0.5 or 0.6, but its Ogg reader *does*
//! recognise the Opus mapping (identification header, `OpusTags`, packet
//! durations), so the container half stays with symphonia and only the codec
//! half is bound here, through `audiopus` 0.3 over `audiopus_sys` 0.2, which
//! builds libopus statically with cmake — the single-binary promise holds
//! (L47: ~400 KB on an 8.9 MB binary, no dynamic linkage). 0.2 of the binding
//! would need autotools to build its libopus, which macOS runners do not
//! ship; 0.3 (pinned exactly — it is a release candidate) needs only cmake.
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

use std::sync::{Mutex, MutexGuard};

use audiopus::MutSignals;
use audiopus::packet::Packet;
use symphonia::core::audio::{
    AsGenericAudioBufferRef, AudioBuffer, AudioMut, AudioSpec, GenericAudioBufferRef,
};
use symphonia::core::codecs::CodecInfo;
use symphonia::core::codecs::audio::AudioCodecId;
use symphonia::core::codecs::audio::well_known::CODEC_ID_OPUS;
use symphonia::core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, AudioDecoderOptions, FinalizeResult,
};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::packet::PacketRef;

use crate::audio::AudioError;

/// The only rate Opus decodes to, whatever it was encoded at.
pub const SAMPLE_RATE: u32 = 48_000;

/// The most frames one Opus packet can decode to: 120 ms at 48 kHz.
const MAX_PACKET_FRAMES: usize = 5_760;

/// Answers whether this codec id can be decoded — true for Opus and, so far,
/// nothing else.
///
/// This lives in one place so the player and the radio stream cannot disagree
/// about which codecs are playable (L47), and so the decoder factory's Opus
/// arm and every other question about playability share one answer.
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
    /// libopus itself, behind a mutex: symphonia's decoder trait demands
    /// `Sync`, libopus's handle is a raw pointer, and a decode never holds the
    /// lock across an await or a callback, so the lock is never contended.
    decoder: Mutex<audiopus::coder::Decoder>,
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
            decoder: Mutex::new(decoder),
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
        self.libopus()
            .reset_state()
            .map_err(|error| AudioError::Decode(format!("libopus reset: {error}")))
    }

    /// Locks libopus for one decode or control call.
    fn libopus(&self) -> MutexGuard<'_, audiopus::coder::Decoder> {
        // A poisoned lock can only mean a panic mid-decode; the state is
        // unusable either way, but decoding continues rather than propagating
        // a poisoning panic into the audio path.
        self.decoder
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
    ///
    /// # Panics
    ///
    /// Never for the buffers this decoder owns: the scratch is 5 760 frames,
    /// far inside the `i32` length [`MutSignals`] requires, and the `expect`
    /// below exists only to say that invariant out loud.
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
        // The scratch is taken out while libopus holds the lock, so the guard
        // and the scratch never borrow `self` at the same time.
        let mut scratch = std::mem::take(&mut self.scratch);
        // 0.3's `decode_float` takes newtypes that carry libopus' invariants:
        // a packet is never empty (checked above) and a buffer fits an `i32`
        // length (the scratch is 5 760 frames, far inside it).
        let frames = self
            .libopus()
            .decode_float(
                Some(
                    Packet::try_from(packet)
                        .map_err(|error| AudioError::Decode(format!("Opus packet: {error}")))?,
                ),
                MutSignals::try_from(&mut scratch)
                    .expect("the decode scratch always fits an i32 length"),
                false,
            )
            .map_err(|error| AudioError::Decode(format!("libopus decode: {error}")));
        self.scratch = scratch;
        let frames = frames?;
        Ok(self.scratch[..frames * usize::from(self.channels)].to_vec())
    }
}

// --- the symphonia bridge --------------------------------------------------

/// Identity of the bridged codec, for symphonia's decoder contract.
const OPUS_CODEC_INFO: CodecInfo = CodecInfo {
    short_name: "opus",
    long_name: "Opus",
    profiles: &[],
};

/// [`OpusDecoder`] dressed as a symphonia [`AudioDecoder`], so the decoder
/// factory can build Opus through the same call it builds every other codec
/// with (T14b).
///
/// The bridge owns the decode buffer symphonia's trait borrows from; the
/// channel-count rule of [`OpusDecoder`] applies unchanged — a packet encoded
/// for a different channel count than the stream declared is an error, never
/// a silent downmix.
pub(crate) struct SymphoniaDecoder {
    decoder: OpusDecoder,
    /// The codec parameters the decoder was built from, reported back verbatim
    /// through [`AudioDecoder::codec_params`].
    params: AudioCodecParameters,
    /// The decode buffer, planar, that [`AudioDecoder::decode`] borrows.
    buf: AudioBuffer<f32>,
}

impl SymphoniaDecoder {
    /// Builds the bridge for an Opus track's codec parameters.
    ///
    /// # Errors
    ///
    /// [`AudioError::Unsupported`] when the codec is not Opus (the factory
    /// guards this with [`decodable`], but the bridge defends itself too),
    /// when the stream declares no channel count, and for every refusal of
    /// [`OpusDecoder::new`].
    pub(crate) fn try_new(
        params: &AudioCodecParameters,
        _options: AudioDecoderOptions,
    ) -> Result<Self, AudioError> {
        if !decodable(params.codec) {
            return Err(AudioError::Unsupported(format!(
                "codec {:?} is not Opus",
                params.codec
            )));
        }
        let channels = params.channels.as_ref().ok_or_else(|| {
            AudioError::Unsupported("Opus stream declares no channel count".to_owned())
        })?;
        let decoder = OpusDecoder::new(
            u16::try_from(channels.count())
                .map_err(|_| AudioError::Unsupported("too many channels".to_owned()))?,
        )?;
        let buf = AudioBuffer::new(
            AudioSpec::new(SAMPLE_RATE, channels.clone()),
            MAX_PACKET_FRAMES,
        );
        Ok(Self {
            decoder,
            params: params.clone(),
            buf,
        })
    }

    /// symphonia's errors are `&'static str`, so the bridge's refusals are
    /// named, not interpolated — [`TrackSource`](crate::audio::TrackSource)
    /// surfaces them as [`AudioError::Decode`] with the same text.
    fn refused(message: &'static str) -> SymphoniaError {
        SymphoniaError::DecodeError(message)
    }
}

impl AudioDecoder for SymphoniaDecoder {
    fn reset(&mut self) {
        // A decoder reset cannot fail meaningfully, and the trait gives the
        // failure nowhere to go; the next packet decodes from clean state.
        let _ = self.decoder.reset();
    }

    fn codec_info(&self) -> &CodecInfo {
        &OPUS_CODEC_INFO
    }

    fn codec_params(&self) -> &AudioCodecParameters {
        &self.params
    }

    fn decode_ref(
        &mut self,
        packet: &PacketRef<'_>,
    ) -> Result<GenericAudioBufferRef<'_>, SymphoniaError> {
        match self.decoder.decode_packet(packet.data) {
            Ok(samples) => {
                let frames = samples.len() / usize::from(self.decoder.channels());
                self.buf.clear();
                self.buf.render_uninit(Some(frames));
                self.buf.copy_from_slice_interleaved::<f32, _>(&samples);
                Ok(self.buf.as_generic_audio_buffer_ref())
            }
            Err(AudioError::Unsupported(_)) => Err(SymphoniaError::Unsupported(
                "Opus channel count out of range",
            )),
            Err(_) => Err(Self::refused("libopus refused the Opus packet")),
        }
    }

    fn finalize(&mut self) -> FinalizeResult {
        FinalizeResult::default()
    }

    fn last_decoded(&self) -> GenericAudioBufferRef<'_> {
        self.buf.as_generic_audio_buffer_ref()
    }
}
