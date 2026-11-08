//! Opus decoder tests (T14).
//!
//! The fixtures are made, never stored: `audiopus` can encode as well as
//! decode, so every test encodes a known signal into Opus packets and reads it
//! back through `velo::audio::opus` — which proves the encoder path of the
//! binding on the way in. Lossy codecs do not promise sample equality, so the
//! assertions are what the codec promises: frequency (via rising zero
//! crossings), amplitude (RMS, within a couple of dB), and errors — never
//! silence, never a panic, never a hang — for malformed input.

use std::io::Cursor;
use std::time::Duration;

use symphonia::core::codecs::audio::well_known::{
    CODEC_ID_AAC, CODEC_ID_FLAC, CODEC_ID_MP3, CODEC_ID_OPUS, CODEC_ID_PCM_S16LE, CODEC_ID_VORBIS,
};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;

use velo::audio::AudioError;
use velo::audio::opus::{OpusDecoder, SAMPLE_RATE, decodable};

/// Opus always decodes at 48 kHz, so the fixtures are generated there.
const RATE: u32 = 48_000;
/// 20 ms — an Opus encoder's natural frame size.
const FRAME: usize = 960;
/// Every fixture is one second of signal.
const FIXTURE_FRAMES: usize = 48_000;

// --- fixtures -------------------------------------------------------------

/// An interleaved sine, stereo when `channels` is 2 (right channel at a
/// different frequency and level, so a decode that mixed the channels cannot
/// pass).
fn sine(channels: u16, left_hz: f64, left_amp: f64) -> Vec<f32> {
    let frames = FIXTURE_FRAMES;
    let mut samples = Vec::with_capacity(frames * usize::from(channels));
    for frame in 0..frames {
        #[expect(
            clippy::cast_precision_loss,
            reason = "fixture frame counts are far below f64's exact integer range"
        )]
        let t = frame as f64 / f64::from(RATE);
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the sine's amplitude is far inside f32's range"
        )]
        let left = (left_amp * (2.0 * std::f64::consts::PI * left_hz * t).sin()) as f32;
        samples.push(left);
        if channels == 2 {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "the sine's amplitude is far inside f32's range"
            )]
            let right =
                ((left_amp / 2.0) * (2.0 * std::f64::consts::PI * 2.0 * left_hz * t).sin()) as f32;
            samples.push(right);
        }
    }
    samples
}

fn audiopus_channels(channels: u16) -> audiopus::Channels {
    match channels {
        1 => audiopus::Channels::Mono,
        2 => audiopus::Channels::Stereo,
        _ => unreachable!("fixtures are mono or stereo"),
    }
}

fn encoder(channels: u16) -> audiopus::coder::Encoder {
    let mut encoder = audiopus::coder::Encoder::new(
        audiopus::SampleRate::Hz48000,
        audiopus_channels(channels),
        audiopus::Application::Audio,
    )
    .expect("an encoder at 48 kHz");
    encoder
        .set_bitrate(audiopus::Bitrate::BitsPerSecond(128_000))
        .expect("a bitrate");
    encoder
}

/// The encoder delay the container must declare as pre-skip.
fn pre_skip(channels: u16) -> u16 {
    let lookahead = encoder(channels).lookahead().expect("encoder lookahead");
    u16::try_from(lookahead).expect("lookahead fits a u16")
}

/// Encodes interleaved samples into Opus packets, 20 ms each, the tail padded
/// with silence into a full frame.
fn encode(channels: u16, samples: &[f32]) -> Vec<Vec<u8>> {
    let encoder = encoder(channels);
    let mut packet = [0u8; 4000];
    let mut packets = Vec::new();
    let frame_len = FRAME * usize::from(channels);
    for chunk in samples.chunks(frame_len) {
        let mut frame = chunk.to_vec();
        frame.resize(frame_len, 0.0);
        let bytes = encoder.encode_float(&frame, &mut packet).expect("encode");
        packets.push(packet[..bytes].to_vec());
    }
    packets
}

fn decode_all(channels: u16, packets: &[Vec<u8>]) -> Vec<f32> {
    let mut decoder = OpusDecoder::new(channels).expect("a decoder");
    let mut samples = Vec::new();
    for packet in packets {
        samples.extend(decoder.decode_packet(packet).expect("decode"));
    }
    samples
}

// --- measurement ----------------------------------------------------------

/// Rising zero crossings: each one is one period, so
/// `crossings × rate / samples` is the frequency in Hz. Phase-free, FFT-free.
fn frequency_hz(samples: &[f32]) -> f64 {
    let crossings = samples
        .windows(2)
        .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
        .count();
    #[expect(
        clippy::cast_precision_loss,
        reason = "crossing counts and window lengths are far below f64's exact integer range"
    )]
    let crossings = crossings as f64;
    #[expect(
        clippy::cast_precision_loss,
        reason = "window lengths are far below f64's exact integer range"
    )]
    let length = samples.len() as f64;
    crossings * f64::from(RATE) / length
}

/// RMS level in dB.
fn rms_db(samples: &[f32]) -> f64 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "window lengths are far below f64's exact integer range"
    )]
    let count = samples.len().max(1) as f64;
    let power = samples
        .iter()
        .map(|s| f64::from(*s) * f64::from(*s))
        .sum::<f64>()
        / count;
    10.0 * power.log10()
}

/// The sine's own RMS level, the reference the decoded level is judged against.
fn expected_rms_db(amplitude: f64) -> f64 {
    10.0 * (amplitude * amplitude / 2.0).log10()
}

/// Everything before the first content frame and after the last full one is
/// codec warm-up, not signal.
fn content_window(decoded: &[f32], channels: u16) -> &[f32] {
    let stride = usize::from(channels);
    &decoded[2 * FRAME * stride..decoded.len() - 2 * FRAME * stride]
}

fn assert_pitch_and_level(samples: &[f32], hz: f64, amplitude: f64) {
    let measured = frequency_hz(samples);
    assert!(
        (measured - hz).abs() < 20.0,
        "measured {measured} Hz, expected {hz} Hz"
    );
    let level = rms_db(samples) - expected_rms_db(amplitude);
    assert!(
        level.abs() < 2.0,
        "measured level is {level:.2} dB off the reference, beyond the couple of dB a lossy codec owes"
    );
}

// --- Ogg Opus container ---------------------------------------------------

/// Ogg's CRC: polynomial `0x04c1_1db7`, initial value 0, no reflection, no final
/// XOR, stored little-endian.
fn ogg_crc(page: &[u8]) -> u32 {
    let mut crc: u32 = 0;
    for &byte in page {
        crc ^= u32::from(byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// One Ogg page carrying `packets` (each a whole packet; the test never needs
/// a packet to span pages).
fn ogg_page(
    header_type: u8,
    granule: u64,
    serial: u32,
    sequence: u32,
    packets: &[&[u8]],
) -> Vec<u8> {
    let mut lacing = Vec::new();
    for packet in packets {
        let whole = packet.len() / 255;
        lacing.extend(std::iter::repeat_n(255u8, whole));
        lacing.push(u8::try_from(packet.len() % 255).expect("lacing fits a byte"));
    }
    let mut page =
        Vec::with_capacity(27 + lacing.len() + packets.iter().map(|p| p.len()).sum::<usize>());
    page.extend_from_slice(b"OggS");
    page.push(0); // stream structure version
    page.push(header_type); // 2 = BOS, 4 = EOS
    page.extend_from_slice(&granule.to_le_bytes());
    page.extend_from_slice(&serial.to_le_bytes());
    page.extend_from_slice(&sequence.to_le_bytes());
    page.extend_from_slice(&[0, 0, 0, 0]); // CRC placeholder
    page.push(u8::try_from(lacing.len()).expect("few segments"));
    page.extend_from_slice(&lacing);
    for packet in packets {
        page.extend_from_slice(packet);
    }
    let crc = ogg_crc(&page);
    page[22..26].copy_from_slice(&crc.to_le_bytes());
    page
}

fn opus_head(channels: u16, pre_skip: u16) -> Vec<u8> {
    let mut head = Vec::new();
    head.extend_from_slice(b"OpusHead");
    head.push(1); // version
    head.push(u8::try_from(channels).expect("mono or stereo"));
    head.extend_from_slice(&pre_skip.to_le_bytes());
    head.extend_from_slice(&RATE.to_le_bytes()); // input sample rate
    head.extend_from_slice(&0i16.to_le_bytes()); // output gain
    head.push(0); // mapping family
    head
}

fn opus_tags() -> Vec<u8> {
    let mut tags = Vec::new();
    tags.extend_from_slice(b"OpusTags");
    tags.extend_from_slice(&4u32.to_le_bytes());
    tags.extend_from_slice(b"velo");
    tags.extend_from_slice(&0u32.to_le_bytes()); // no comments
    tags
}

/// A complete Ogg Opus file: identification page, tags page, one page per
/// packet, the last one carrying EOS.
fn ogg_opus_file(channels: u16, packets: &[Vec<u8>], pre_skip: u16) -> Vec<u8> {
    let serial = 0x1f4a_9e0b;
    let head = opus_head(channels, pre_skip);
    let tags = opus_tags();
    let mut file = ogg_page(2, 0, serial, 0, &[&head]);
    file.extend(ogg_page(0, 0, serial, 1, &[&tags]));
    let mut granule = u64::from(pre_skip);
    let last_page = packets.len();
    for (index, packet) in packets.iter().enumerate() {
        granule += FRAME as u64;
        let last = index + 1 == last_page;
        let sequence = u32::try_from(2 + index).expect("page sequences fit a u32");
        file.extend(ogg_page(
            u8::from(last) * 4,
            granule,
            serial,
            sequence,
            &[packet],
        ));
    }
    file
}

/// Reads an Ogg Opus file the way the player will: symphonia's Ogg reader for
/// the packets, `OpusDecoder` for the samples. `Err` on any refusal.
fn decode_ogg_opus(bytes: &[u8]) -> Result<(u16, Vec<f32>), String> {
    let cursor = Cursor::new(bytes.to_vec());
    let stream = MediaSourceStream::new(Box::new(cursor), MediaSourceStreamOptions::default());
    let mut hint = Hint::new();
    hint.with_extension("opus");

    let mut reader = symphonia::default::get_probe()
        .probe(
            &hint,
            stream,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|error| error.to_string())?;
    let track = reader
        .default_track(TrackType::Audio)
        .ok_or("no audio track")?;
    let track_id = track.id;
    let channels = track
        .codec_params
        .as_ref()
        .and_then(symphonia::core::codecs::CodecParameters::audio)
        .and_then(|params| params.channels.as_ref())
        .map(symphonia::core::audio::Channels::count)
        .ok_or("the stream declares no channel count")?;
    let channels = u16::try_from(channels).map_err(|_| "too many channels")?;

    let mut decoder = OpusDecoder::new(channels).map_err(|error| error.to_string())?;
    let mut samples = Vec::new();
    loop {
        match reader.next_packet() {
            Ok(Some(packet)) => {
                if packet.track_id != track_id {
                    continue;
                }
                samples.extend(
                    decoder
                        .decode_packet(&packet.data)
                        .map_err(|e| e.to_string())?,
                );
            }
            Ok(None) | Err(SymphoniaError::ResetRequired) => break,
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok((channels, samples))
}

/// Runs `f` in a thread with a timeout, so a hang is reported as a hang and a
/// panic is reported as a panic, never mistaken for an `Err`.
fn without_hang_or_panic<T, F>(name: &'static str, f: F) -> T
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + std::panic::UnwindSafe + 'static,
{
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(std::panic::catch_unwind(f));
    });
    receiver
        .recv_timeout(Duration::from_secs(30))
        .expect("the worker answers within 30 s — a timeout is a hang")
        .unwrap_or_else(|_panic| panic!("{name} panicked"))
}

// --- the rules ------------------------------------------------------------

#[test]
fn a_1khz_sine_round_trips_through_encode_then_decode() {
    let packets = encode(1, &sine(1, 1000.0, 0.5));
    let decoded = decode_all(1, &packets);
    // 48 kHz, 1 s in, 20 ms packets: the decode must not lose or gain time.
    assert!(
        decoded.len() > FIXTURE_FRAMES - 1_000 && decoded.len() < FIXTURE_FRAMES + 1_000,
        "decoded {} samples for 1 s of mono input",
        decoded.len()
    );
    assert_pitch_and_level(content_window(&decoded, 1), 1000.0, 0.5);
    assert_eq!(SAMPLE_RATE, 48_000, "Opus decodes at 48 kHz, always");
}

#[test]
fn a_stereo_sine_round_trips_with_its_channels_independent() {
    let packets = encode(2, &sine(2, 1000.0, 0.5));
    let decoded = decode_all(2, &packets);
    let window = content_window(&decoded, 2);
    let left: Vec<f32> = window.iter().step_by(2).copied().collect();
    let right: Vec<f32> = window.iter().skip(1).step_by(2).copied().collect();
    assert_pitch_and_level(&left, 1000.0, 0.5);
    assert_pitch_and_level(&right, 2000.0, 0.25);
}

#[test]
fn a_packet_of_the_wrong_channel_count_is_an_error_not_silence() {
    let stereo_packets = encode(2, &sine(2, 1000.0, 0.5));
    let mono_packets = encode(1, &sine(1, 1000.0, 0.5));

    let mut mono = OpusDecoder::new(1).expect("a mono decoder");
    assert!(matches!(
        mono.decode_packet(&stereo_packets[0]),
        Err(AudioError::Decode(_))
    ));

    let mut stereo = OpusDecoder::new(2).expect("a stereo decoder");
    assert!(matches!(
        stereo.decode_packet(&mono_packets[0]),
        Err(AudioError::Decode(_))
    ));
}

#[test]
fn a_decoder_refuses_to_open_for_more_than_two_channels() {
    assert!(matches!(
        OpusDecoder::new(6),
        Err(AudioError::Unsupported(_))
    ));
    assert!(matches!(
        OpusDecoder::new(0),
        Err(AudioError::Unsupported(_))
    ));
}

#[test]
fn a_truncated_packet_is_an_error_never_a_panic() {
    let packets = encode(1, &sine(1, 1000.0, 0.5));
    let full = &packets[3];
    let mut decoder = OpusDecoder::new(1).expect("a mono decoder");

    // An empty packet is not an Opus packet at all.
    assert!(matches!(
        decoder.decode_packet(&[]),
        Err(AudioError::Decode(_))
    ));
    // A packet whose own frame table declares more data than it carries —
    // what losing the tail of a page does to a code-3 packet — must be
    // refused.
    let mut overlong = vec![(full[0] & 0xf8) | 0x03, 0x82, 100, 100];
    overlong.extend(std::iter::repeat_n(0x55u8, 30));
    assert!(matches!(
        decoder.decode_packet(&overlong),
        Err(AudioError::Decode(_))
    ));
    // A *single-frame* packet truncated to fewer bytes is a different valid
    // packet as far as RFC 6716 §3.1 is concerned — measured: every prefix of
    // this 379-byte packet down to 1 byte decodes, because a code-0 frame's
    // length is defined as the rest of the packet. The format itself cannot
    // refuse it; losing the tail of a stream is refused by the container
    // before such a packet is ever decoded (the truncated-page test). Here it
    // must still be bounded and panic-free, never a hang:
    for cut in [1usize, 2, 4, 8, full.len() / 2, full.len() - 1] {
        let frames = decoder
            .decode_packet(&full[..cut])
            .expect("a truncated single-frame packet is still decodable");
        assert_eq!(
            frames.len(),
            960,
            "a truncation must decode one bounded frame, not run away"
        );
    }
}

#[test]
fn an_ogg_opus_stream_decodes_through_symphonia() {
    let packets = encode(1, &sine(1, 1000.0, 0.5));
    let file = ogg_opus_file(1, &packets, pre_skip(1));

    let (channels, decoded) =
        without_hang_or_panic("the full Ogg Opus stream", move || decode_ogg_opus(&file))
            .expect("the stream decodes");
    assert_eq!(channels, 1);
    assert!(
        decoded.len() > FIXTURE_FRAMES - 1_000 && decoded.len() < FIXTURE_FRAMES + 1_000,
        "decoded {} samples for 1 s of input",
        decoded.len()
    );
    assert_pitch_and_level(content_window(&decoded, 1), 1000.0, 0.5);
}

#[test]
fn a_truncated_ogg_page_is_an_error_never_a_panic_or_a_hang() {
    let packets = encode(1, &sine(1, 1000.0, 0.5));
    let file = ogg_opus_file(1, &packets, pre_skip(1));

    // Cut into the last audio page: the page header is intact, its payload is
    // not.
    let cut_tail = file[..file.len() - 37].to_vec();
    let result = without_hang_or_panic("the tail-truncated stream", move || {
        decode_ogg_opus(&cut_tail)
    });
    result.expect_err("a truncated page must be refused");

    // Cut into the identification page: nothing is even detectable.
    let cut_head = file[..10].to_vec();
    let result = without_hang_or_panic("the head-truncated stream", move || {
        decode_ogg_opus(&cut_head)
    });
    result.expect_err("a stream with no readable head must be refused");
}

#[test]
fn decodable_answers_true_for_opus_and_false_for_everything_else() {
    assert!(decodable(CODEC_ID_OPUS));
    for id in [
        CODEC_ID_VORBIS,
        CODEC_ID_MP3,
        CODEC_ID_AAC,
        CODEC_ID_FLAC,
        CODEC_ID_PCM_S16LE,
    ] {
        assert!(!decodable(id), "codec id {id:?} must not answer true yet");
    }
}
