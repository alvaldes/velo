//! The stream seam for `velo`'s audio path (T5a, specs S2/S3).
//!
//! A live station is a non-file `MediaSource`: not seekable, no extension to
//! hint with, no declared duration. These tests pin that `TrackSource` decodes
//! such a source identically to the same audio opened as a file, through a
//! real ICY wrapper — the fixture bytes carry interleaved `StreamTitle`
//! metadata that must never leak into the decoded samples.

use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use symphonia::core::io::ReadOnlySource;
use velo::audio::TrackSource;
use velo::radio::icy::IcyReader;

const METADATA_INTERVAL: usize = 8192;

/// The ICY title embedded in the wrapped fixture.
const STREAM_TITLE: &str = "Nina Simone - Feeling Good";

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/silence-then-tone.mp3")
}

/// One ICY metadata block: a length byte followed by `len * 16` bytes.
fn metadata_block(payload: &[u8]) -> Vec<u8> {
    assert_eq!(payload.len() % 16, 0, "ICY payload is padded to 16 bytes");
    let mut block = vec![u8::try_from(payload.len() / 16).expect("test payload fits one byte")];
    block.extend_from_slice(payload);
    block
}

/// Wraps `audio` in ICY metadata: every [`METADATA_INTERVAL`] audio bytes a
/// `StreamTitle` block (first) or an empty `L = 0` block (the rest).
fn icy_wrap(audio: &[u8]) -> Vec<u8> {
    let title = format!("StreamTitle='{STREAM_TITLE}';");
    let mut padded = title.into_bytes();
    while padded.len() % 16 != 0 {
        padded.push(0);
    }
    let title_block = metadata_block(&padded);
    let empty_block = metadata_block(&[]);

    let mut wrapped = Vec::with_capacity(audio.len() + 16 * (audio.len() / METADATA_INTERVAL + 1));
    for chunk in audio.chunks(METADATA_INTERVAL) {
        wrapped.extend_from_slice(chunk);
        wrapped.extend_from_slice(&title_block);
        for _ in 1..audio.len().div_ceil(METADATA_INTERVAL).max(1) {
            wrapped.extend_from_slice(&empty_block);
        }
    }
    wrapped
}

fn open_stream(wrapped: Vec<u8>) -> TrackSource {
    let demux = IcyReader::new(Cursor::new(wrapped), Some(METADATA_INTERVAL));
    let source = ReadOnlySource::new(demux);
    TrackSource::open_stream(Box::new(source)).expect("the stream opens")
}

/// Reads a whole track source to its end, one block at a time.
fn drain(source: &mut TrackSource) -> (Vec<f32>, usize) {
    let mut samples = Vec::new();
    let mut blocks = 0;
    while let Some(block) = source.read_block().expect("decoding succeeds") {
        samples.extend_from_slice(&block);
        blocks += 1;
    }
    (samples, blocks)
}

/// A read through the ICY wrapper reaches the demux's end (drives `title()`).
fn drain_demux(wrapped: &[u8]) -> IcyReader<Cursor<Vec<u8>>> {
    let mut demux = IcyReader::new(Cursor::new(wrapped.to_vec()), Some(METADATA_INTERVAL));
    let mut sink = Vec::new();
    demux.read_to_end(&mut sink).expect("the demux reads");
    demux
}

/// The priming symphonia declares for the fixture's MP3 (its LAME tag): the
/// delay and padding the file path trims (gapless on) and the live path keeps
/// (gapless off, S3). Read through symphonia itself, so the numbers are the
/// decoder's own declaration and never hard-coded.
fn file_priming(path: &Path) -> (u64, u64) {
    use symphonia::core::formats::{FormatOptions, TrackType};
    use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
    use symphonia::core::meta::MetadataOptions;

    let src = std::fs::File::open(path).expect("the fixture is readable");
    let mss = MediaSourceStream::new(Box::new(src), MediaSourceStreamOptions::default());
    let mut hint = symphonia::core::formats::probe::Hint::new();
    hint.with_extension("mp3");
    let mut reader = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .expect("the fixture probes");
    let mut delay = None;
    let mut padding = None;
    for _ in 0..100 {
        let track = reader
            .default_track(TrackType::Audio)
            .expect("an audio track");
        delay = track.delay;
        padding = track.padding;
        if delay.is_some() && padding.is_some() {
            break;
        }
        if reader.next_packet().is_err() {
            break;
        }
    }
    (
        u64::from(delay.unwrap_or(0)),
        u64::from(padding.unwrap_or(0)),
    )
}

#[test]
fn icy_wrapped_stream_decodes_identically_to_the_file() {
    let plain_bytes = std::fs::read(fixture()).expect("the fixture is readable");
    let wrapped = icy_wrap(&plain_bytes);

    let mut file = TrackSource::open(&fixture()).expect("the file opens");
    let mut stream = open_stream(wrapped);

    assert_eq!(stream.spec(), file.spec(), "same sample format");

    let (file_samples, _file_blocks) = drain(&mut file);
    let (stream_samples, _stream_blocks) = drain(&mut stream);

    // The file path trims the LAME priming its tag declares (gapless on); the
    // live path keeps every decoded frame (gapless off). The stream is
    // therefore the file's samples shifted by the declared delay, with the
    // declared padding after it — and nothing else may differ.
    let (delay, padding) = file_priming(&fixture());
    let channels = usize::from(file.spec().channels);
    let head = usize::try_from(delay).expect("priming fits usize") * channels;
    let extra = usize::try_from(delay + padding).expect("priming fits usize") * channels;
    assert_eq!(
        stream_samples.len(),
        file_samples.len() + extra,
        "the live path keeps exactly the priming the file path trims"
    );
    assert_eq!(
        &stream_samples[head..][..file_samples.len()],
        file_samples,
        "no metadata byte may leak into the audio"
    );
}

#[test]
fn a_stream_has_no_duration() {
    let plain_bytes = std::fs::read(fixture()).expect("the fixture is readable");
    let mut stream = open_stream(icy_wrap(&plain_bytes));
    drain(&mut stream);
    assert!(
        stream.duration().is_none(),
        "a live stream never declares a duration"
    );
}

#[test]
fn the_demux_reports_the_embedded_stream_title() {
    let plain_bytes = std::fs::read(fixture()).expect("the fixture is readable");
    let demux = drain_demux(&icy_wrap(&plain_bytes));
    assert_eq!(demux.title(), Some(STREAM_TITLE));
}

#[test]
fn an_empty_source_fails_cleanly_never_panics() {
    let empty = IcyReader::new(Cursor::new(Vec::new()), Some(METADATA_INTERVAL));
    let result = TrackSource::open_stream(Box::new(ReadOnlySource::new(empty)));
    assert!(result.is_err(), "an empty source cannot open");
}
