//! AAC encoder priming in MP4/M4A (T16).
//!
//! An AAC encoder emits its first `delay` frames as encoder warm-up and its
//! last `padding` frames as flush padding. Containers declare both: MP3 via the
//! LAME tag (symphonia reads it and trims when the decoder's `gapless` option
//! is on), MP4 via
//! an `elst` edit whose `media_time` is the delay, or an explicit `iTunSMPB`
//! tag. symphonia's isomp4 reader does neither (measured on 0.5.5 and again
//! on 0.6.1): the priming reaches the decoded stream, where an M4A join hears
//! it as a click or a smear at the seam.
//!
//! These tests pin, against the committed fixtures:
//!
//! * what symphonia itself reports for `delay`/`padding` (the measurement that
//!   justified this task — asserted so a symphonia upgrade that starts trimming
//!   is caught rather than silently double-trimmed by us), and
//! * that [`velo::audio::TrackSource`] delivers exactly the container's content
//!   frames: priming dropped from the head, padding from the tail, seeks
//!   addressed in content frames, durations reported in content frames.
//!
//! `tests/fixtures/priming-itunsmpb.m4a` is `tiny.m4a` with one byte-patched
//! addition: a freeform `----:com.apple.iTunes:iTunSMPB` tag inside
//! `moov/udta/meta/ilst` claiming delay 512 and padding 100 (tiny.m4a's real
//! priming is 1024, from its `elst`). It exists so the iTunSMPB path and its
//! precedence over `elst` are provable without an encoder; reproduce it by
//! inserting that atom into `tiny.m4a` and adding its size to `ilst`, `meta`,
//! `udta` and `moov`.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;

use velo::audio::TrackSource;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// What symphonia itself produces for a file, without any of `velo`'s code in
/// between: the ground truth every test here compares against.
struct RawDecode {
    /// The audio track's `delay` as symphonia reports it (0.6 moved this from
    /// the codec parameters onto the track).
    delay: Option<u32>,
    /// The audio track's `padding` as symphonia reports it.
    padding: Option<u32>,
    /// The audio track's `num_frames` as symphonia reports it.
    n_frames: Option<u64>,
    /// Every decoded frame, interleaved.
    samples: Vec<f32>,
    channels: usize,
}

/// Decodes a whole file with symphonia, exactly as `TrackSource::open` would,
/// and reports what symphonia declared and produced.
fn raw_decode(path: &Path) -> RawDecode {
    let file = File::open(path).expect("the fixture opens");
    let stream = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());
    let mut hint = Hint::new();
    hint.with_extension(
        path.extension()
            .and_then(std::ffi::OsStr::to_str)
            .expect("the fixture has an extension"),
    );

    let probed = symphonia::default::get_probe()
        .probe(
            &hint,
            stream,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .expect("the fixture probes");

    let mut reader = probed;
    let track = reader
        .default_track(TrackType::Audio)
        .expect("the fixture has a default track");
    let params = track
        .codec_params
        .as_ref()
        .and_then(symphonia::core::codecs::CodecParameters::audio)
        .expect("the fixture has an audio track")
        .clone();
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .expect("the fixture decodes");
    let track_id = track.id;
    // 0.6 reports priming and length on the track, not the codec parameters.
    let (delay, padding, n_frames) = (track.delay, track.padding, track.num_frames);

    let mut samples = Vec::new();
    let mut channels = 0usize;
    loop {
        let packet = match reader.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(error) => panic!("unexpected decode error: {error}"),
        };
        if packet.track_id != track_id {
            continue;
        }
        let decoded_packet = decoder.decode(&packet).expect("a packet decodes");
        channels = decoded_packet.spec().channels().count();
        let mut buffer = Vec::with_capacity(decoded_packet.samples_interleaved());
        decoded_packet.copy_to_vec_interleaved::<f32>(&mut buffer);
        samples.extend_from_slice(&buffer);
    }

    RawDecode {
        delay,
        padding,
        n_frames,
        samples,
        channels,
    }
}

/// Drains a whole file through `TrackSource`, as the player would.
fn drained(path: &Path) -> Vec<f32> {
    let mut source = TrackSource::open(path).expect("the fixture opens");
    let mut samples = Vec::new();
    while let Some(block) = source.read_block().expect("decoding must not fail") {
        samples.extend_from_slice(&block);
    }
    samples
}

/// Frames in the fixture's raw decoded stream.
fn decoded_frames(raw: &RawDecode) -> u64 {
    (raw.samples.len() / raw.channels) as u64
}

/// The raw stream is decoded by the same symphonia decoder both paths share, so
/// the comparison is exact; the tolerance only absorbs float noise.
const SAMPLE_TOLERANCE: f32 = 1e-6;

fn assert_same_audio(got: &[f32], want: &[f32], context: &str) {
    assert_eq!(
        got.len(),
        want.len(),
        "{context}: sample count differs ({} vs {})",
        got.len(),
        want.len()
    );
    let worst = got
        .iter()
        .zip(want)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0_f32, f32::max);
    assert!(
        worst < SAMPLE_TOLERANCE,
        "{context}: worst sample differs by {worst}"
    );
}

// ---------------------------------------------------------------------------
// The measurement (step 1), pinned.
// ---------------------------------------------------------------------------

#[test]
fn measurement_symphonia_reports_no_priming_for_isomp4() {
    let raw = raw_decode(&fixture("tiny.m4a"));

    // Verbatim answer of the measurement, pinned so a symphonia upgrade that
    // starts trimming is caught here instead of double-trimmed in TrackSource:
    //   track.delay      = None
    //   track.padding    = None
    //   track.num_frames = 14254 = 13 x 1024 + 942 (the stts table, raw
    //   frames including priming; the elst edit says playback starts at 1024)
    //   decoded frames   = 14254, i.e. num_frames: the priming stays in
    // Measured verbatim on symphonia 0.5.5 and re-measured verbatim on 0.6.1:
    // the isomp4 reader still populates neither, so velo's own window stays.
    assert!(
        raw.delay.is_none(),
        "symphonia populates delay for isomp4?!"
    );
    assert!(
        raw.padding.is_none(),
        "symphonia populates padding for isomp4?!"
    );
    assert_eq!(raw.n_frames, Some(14_254), "n_frames is the raw stts count");
    // The decoded stream is one AAC packet LONGER than n_frames: the container
    // says the last packet holds 942 frames, the decoder emits 1024, and
    // nothing trims either end.
    assert_eq!(
        decoded_frames(&raw),
        14_336,
        "decoded sample count = 14 x 1024: nothing trimmed, last packet over-produced"
    );
}

#[test]
fn the_declared_length_is_the_content_length() {
    // tiny.m4a was encoded by Lavf63.1.102 at 44100 Hz: 0.3 s of audio, one
    // 1024-frame AAC priming edit and no padding (its `elst` media_time is
    // 1024, its `mdhd` duration is 14254 = 1024 + 13230). The committed file
    // carries an elst and no iTunSMPB. The reported duration must be the
    // declared 0.3 s, not the 323 ms the raw frame count would suggest.
    assert_eq!(
        TrackSource::open(&fixture("tiny.m4a"))
            .expect("the fixture opens")
            .duration(),
        Some(Duration::from_millis(300)),
        "the fixture's declared length is 13230 content frames = 300 ms"
    );
}

// ---------------------------------------------------------------------------
// The trim (steps 2-4).
// ---------------------------------------------------------------------------

#[test]
fn the_priming_never_reaches_the_player() {
    let path = fixture("tiny.m4a");
    let raw = raw_decode(&path);

    // The content frames are raw[1024..14254]: everything the encoder inserted
    // as warm-up is dropped, and so is the 82-frame over-production of the
    // last packet (14336 decoded - 14254 declared).
    let start = 1024 * raw.channels;
    let expected = &raw.samples[start..14_254 * raw.channels];
    let got = drained(&path);
    assert_eq!(
        got.len(),
        expected.len(),
        "decoded length is n_frames minus the priming, not the raw count"
    );
    assert_same_audio(&got, expected, "the drained stream");
}

#[test]
fn duration_reports_content_frames_not_raw_frames() {
    let path = fixture("tiny.m4a");
    let source = TrackSource::open(&path).expect("the fixture opens");
    // 13230 content frames at 44100 Hz = exactly 300 ms. The raw count (14254)
    // would report 323 ms and overstate every M4A in a queue by its priming.
    assert_eq!(source.duration(), Some(Duration::from_millis(300)));
}

#[test]
fn a_gapless_join_of_the_same_m4a_has_no_seam() {
    let path = fixture("tiny.m4a");
    let raw = raw_decode(&path);
    let content_frames = 13_230usize;

    let (spec, joined) = velo::audio::drain_gapless(&[path.clone(), path]).expect("two M4As join");
    assert_eq!(spec.sample_rate, 44_100);
    assert_eq!(spec.channels, 2);

    // Exactly two content copies: no priming at the head of either copy, no
    // padding at either tail.
    assert_eq!(
        joined.len() / usize::from(spec.channels),
        2 * content_frames,
        "the join carries exactly 2 x {content_frames} frames"
    );

    // And it is the two content streams concatenated: the boundary samples are
    // the true end of the music and the true start of the next copy, with no
    // priming smear in between.
    let content = &raw.samples[1024 * raw.channels..14_254 * raw.channels];
    let expected: Vec<f32> = [content, content].concat();
    assert_same_audio(&joined, &expected, "the joined stream");

    let join = content_frames * usize::from(spec.channels);
    assert!(
        joined[join - 1].abs() > SAMPLE_TOLERANCE || joined[join].abs() > SAMPLE_TOLERANCE,
        "at least one side of the seam carries audio: no silent insert"
    );
}

#[test]
fn open_at_lands_on_the_requested_content_frame() {
    let path = fixture("tiny.m4a");
    let raw = raw_decode(&path);
    let offset: usize = 500; // inside the first content second

    let mut source = TrackSource::open_at(&path, u64::try_from(offset).expect("small"))
        .expect("the seeked track opens");

    // Duration is still content-relative, with the whole priming removed.
    assert_eq!(
        source.duration(),
        Some(Duration::from_millis(300)),
        "a seek never changes the reported length"
    );

    // The first sample delivered is content frame 500 = raw frame 1524.
    let mut got = Vec::new();
    while let Some(block) = source.read_block().expect("decoding must not fail") {
        got.extend_from_slice(&block);
    }
    let start = (1024 + offset) * raw.channels;
    let expected = &raw.samples[start..14_254 * raw.channels];
    assert_eq!(
        got.len() / raw.channels,
        13_230 - offset,
        "the seeked stream holds the content frames from the offset on"
    );
    // AAC is lossy across a seek boundary: after `decoder.reset()` the first
    // packet decodes without the previous frame's MDCT overlap state, so a
    // bounded region around the landing differs from a continuous decode (the
    // same guarantee the MP3 seek test accepts). Measure past it: from the
    // second packet on the seeked stream is sample-exact.
    let warm_up = 1024; // one AAC packet
    assert_same_audio(
        &got[warm_up * raw.channels..],
        &expected[warm_up * raw.channels..],
        "the seeked stream past the overlap",
    );
    let landing = &got[..warm_up * raw.channels];
    let wanted = &expected[..warm_up * raw.channels];
    let worst = landing
        .iter()
        .zip(wanted)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0_f32, f32::max);
    assert!(
        worst < 0.1,
        "even inside the overlap the seek lands on the music, not on priming: worst {worst}"
    );
}

#[test]
fn open_at_past_the_end_clamps_to_silence() {
    let path = fixture("tiny.m4a");
    let mut source =
        TrackSource::open_at(&path, 1_000_000).expect("a past-end offset clamps, it never errors");
    assert!(
        source
            .read_block()
            .expect("decoding must not fail")
            .is_none(),
        "a seek past the content end has nothing left to play"
    );
}

// ---------------------------------------------------------------------------
// iTunSMPB: the explicit tag wins over the edit list.
// ---------------------------------------------------------------------------

#[test]
fn itunsmpb_delay_and_padding_are_applied_over_the_elst() {
    let path = fixture("priming-itunsmpb.m4a");
    let raw = raw_decode(&path);

    // The tag declares delay 512 / padding 100 where the elst says 1024 / 0:
    // the explicit tag wins, so the content window is [512, 14254 - 100).
    let got = drained(&path);
    let expected = &raw.samples[512 * raw.channels..(14_254 - 100) * raw.channels];
    assert_eq!(got.len(), expected.len(), "both trims applied, once");
    assert_same_audio(&got, expected, "the iTunSMPB-trimmed stream");
}

#[test]
fn itunsmpb_duration_matches_the_declared_window() {
    let path = fixture("priming-itunsmpb.m4a");
    let source = TrackSource::open(&path).expect("the fixture opens");
    // 14254 - 512 - 100 = 13642 frames = 309.34... ms at 44100 Hz.
    assert_eq!(
        source.duration(),
        Some(Duration::from_millis(13_642 * 1000 / 44_100)),
        "duration = n_frames minus the iTunSMPB delay and padding"
    );
}

// ---------------------------------------------------------------------------
// The trim is MP4-only: other formats keep symphonia's own behaviour.
// ---------------------------------------------------------------------------

#[test]
fn an_mp3_still_relies_on_symphonias_own_trim() {
    // The MP3 fixture's LAME tag is honoured by symphonia itself (delay/padding
    // populated by the reader, packets trimmed by the decoder when its
    // `gapless` option is on). velo must not
    // add a second trim: the MP3 decodes to exactly its LAME-declared length.
    let path = fixture("silence-then-tone.mp3");
    let raw = raw_decode(&path);
    let got = drained(&path);

    let n_frames = raw.n_frames.expect("the LAME tag declares a length");
    assert_eq!(
        got.len() / raw.channels,
        usize::try_from(n_frames).expect("small"),
        "the MP3 decodes to exactly its LAME-declared length"
    );
    assert!(
        raw.delay.is_some() && raw.padding.is_some(),
        "symphonia populates the LAME tag for MP3; velo adds nothing"
    );
}

// --- measurement probe (run by hand: `cargo test --test aac_priming --
// --ignored --nocapture probe`) ---------------------------------------------

/// FNV-1a over the little-endian bytes of a drained stream: the cheap
/// fingerprint the byte-identical regression check compares before and after
/// any change to the priming path.
/// FNV-1a over the samples **as 16-bit PCM**, which is what the device gets.
///
/// The exact `f32` bits are not comparable across platforms: x86 and arm64 take
/// different paths through the decoder, so the same file yields samples that
/// differ in the last bits and hash differently. Rounding to `i16` first keeps
/// the pin meaningful (a trim change or a re-addressed window moves whole
/// samples, not their last bit) and makes it portable.
fn fnv1a_quantised(samples: &[f32]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for sample in samples {
        // Clamped to ±1 and rounded before the cast, so the value is inside
        // `i16` by construction; the lint cannot see through `clamp`.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "clamped to +/-1 and rounded, so it fits i16"
        )]
        let quantised = (sample.clamp(-1.0, 1.0) * 32_767.0).round() as i16;
        for byte in quantised.to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

#[expect(
    dead_code,
    reason = "kept for byte-level debugging of a platform shift"
)]
fn fnv1a(samples: &[f32]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for sample in samples {
        for byte in sample.to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

fn probe_rms_db(samples: &[f32]) -> f64 {
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

fn probe_peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0_f32, |acc, s| acc.max(s.abs()))
}

fn probe_compare(
    label: &str,
    continuous: &[f32],
    seeked: &[f32],
    offset_frames: usize,
    channels: usize,
) {
    let expected = &continuous[offset_frames * channels..];
    println!(
        "{label}: seeked {} frames, continuous from offset {} frames ({} samples)",
        seeked.len() / channels,
        offset_frames,
        expected.len()
    );
    if seeked.len() != expected.len() {
        println!(
            "{label}: LENGTH MISMATCH: seeked {} vs expected {} samples",
            seeked.len(),
            expected.len()
        );
    }
    let common = seeked.len().min(expected.len());
    let diffs: Vec<f32> = (0..common).map(|i| seeked[i] - expected[i]).collect();
    let first_bad = diffs.iter().position(|d| d.abs() > 1e-3).unwrap_or(common);
    let bad_frames = diffs
        .chunks(channels)
        .filter(|frame| frame.iter().any(|d| d.abs() > 1e-3))
        .count();
    let last_bad = diffs
        .iter()
        .rposition(|d| d.abs() > 1e-3)
        .map_or(first_bad, |last| last / channels);
    println!(
        "{label}: first differing frame {first_bad}, last {last_bad}, {bad_frames} frames differ \
         (>1e-3) of {}",
        common / channels
    );
    if bad_frames > 0 {
        let wrong = &diffs[(first_bad * channels)..((last_bad + 1) * channels)];
        let wanted = &expected[(first_bad * channels)..((last_bad + 1) * channels)];
        println!(
            "{label}: wrong region: diff RMS {:.2} dB, diff peak {:.4}; the content that should be \
             there measures RMS {:.2} dB, peak {:.4}",
            probe_rms_db(wrong),
            probe_peak(wrong),
            probe_rms_db(wanted),
            probe_peak(wanted)
        );
    }
}

/// The seek shape of the symphonia-decoded paths: does an MP3 or an AAC seek
/// emit decoder warm-up after `open_at`'s `decoder.reset()`, the same way the
/// Opus path does?
#[test]
#[ignore = "measurement probe: cargo test --test aac_priming -- --ignored --nocapture probe_mp3_aac_seek"]
fn probe_mp3_aac_seek_shape() {
    for name in ["silence-then-tone.mp3", "tiny.m4a", "priming-itunsmpb.m4a"] {
        let path = fixture(name);
        let source = TrackSource::open(&path).expect("open");
        let spec = source.spec();
        let channels = usize::from(spec.channels);
        let continuous = drained(&path);
        println!(
            "{name}: {} Hz, {} ch, {} content frames",
            spec.sample_rate,
            spec.channels,
            continuous.len() / channels
        );
        let length = continuous.len() / channels;
        for fraction in [4usize, 2] {
            let offset = length / fraction;
            let seeked = drained_at(&path, u64::try_from(offset).expect("small"));
            probe_compare(
                &format!("{name} seek to {offset}"),
                &continuous,
                &seeked,
                offset,
                channels,
            );
        }
    }
}

/// Drains a track from a seeked open, as the player's rebuild does.
fn drained_at(path: &Path, offset_frames: u64) -> Vec<f32> {
    let mut source = TrackSource::open_at(path, offset_frames).expect("the seeked track opens");
    let mut samples = Vec::new();
    while let Some(block) = source.read_block().expect("decoding must not fail") {
        samples.extend_from_slice(&block);
    }
    samples
}

/// The baseline of `open` on every fixture: sample count plus FNV-1a over the
/// drained samples **as 16-bit PCM**, measured before the seek warm-up change
/// touched the priming path. The priming work (T16, T14b, L50) is fragile and
/// `open` must not move:
///
/// - the **count** is the trim detector — a double trim loses exactly the
///   declared priming (312 samples for the Opus pre-skip, 1105 + 551 for this
///   MP3), and the count changes the moment that happens;
/// - the **quantised hash** is the content detector — a re-addressed window or a
///   decoder that starts trimming earlier shifts whole samples.
///
/// It is deliberately *not* the `f32` bits. Those differ between x86 and arm64
/// (different paths through the decoder), which made this pin fail on the Linux
/// runner for a file that had not changed. Rounding to `i16` — what the device
/// actually receives — keeps the pin strict where it matters and portable.
///
/// A deliberate change re-measures with `cargo test --test aac_priming --
/// --ignored --nocapture capture_open_baseline` and updates the constants.
#[test]
fn open_stays_byte_identical_on_every_fixture() {
    let pinned = [
        ("mono-tone.wav", 11_025usize, 0xd46d_3f8a_4948_5e79u64),
        ("silence-then-tone.mp3", 88_200, 0xfe55_0df1_58d3_22e2),
        ("tiny.m4a", 26_460, 0x8ac7_cfff_ce85_b8e1),
        ("priming-itunsmpb.m4a", 27_284, 0x5a5b_e073_b8df_5c69),
    ];
    for (name, length, hash) in pinned {
        let samples = drained(&fixture(name));
        assert_eq!(
            (samples.len(), fnv1a_quantised(&samples)),
            (length, hash),
            "{name}: the drained stream moved"
        );
    }
}

/// Re-measures the baseline the pin above asserts.
#[test]
#[ignore = "baseline capture: cargo test --test aac_priming -- --ignored --nocapture capture_open_baseline"]
fn capture_open_baseline() {
    for name in [
        "mono-tone.wav",
        "silence-then-tone.mp3",
        "tiny.m4a",
        "priming-itunsmpb.m4a",
    ] {
        let samples = drained(&fixture(name));
        println!(
            "{name}: len {} fnv {:016x}",
            samples.len(),
            fnv1a_quantised(&samples)
        );
    }
}
