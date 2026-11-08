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
