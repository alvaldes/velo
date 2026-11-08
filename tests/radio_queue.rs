//! A station sits in the queue and plays (T5c2a, specs S2/S3).
//!
//! Everything here runs headless through the producer's own loop: the
//! station opener is a fake over the ICY-wrapped MP3 fixture (the same
//! bytes `radio_seam` and `radio_stream` decode), the resolver is a
//! counting pass-through, and no test ever touches the network. The two
//! seams must never cross: a station's URL is opened, never resolved; a
//! file's path is resolved, never opened.

use std::io::{self, Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use symphonia::core::io::ReadOnlySource;
use velo::audio::output::{
    Entry, PathResolver, StationOpener, StationSource, TrackFailure, drain_headless_with_opener,
};
use velo::audio::{AudioError, TrackSource, TrackSpec};
use velo::config::ReplayGainMode;
use velo::radio::icy::IcyReader;
use velo::radio::stream::StreamError;

/// The ICY metadata interval of the wrapped fixture.
const METADATA_INTERVAL: usize = 8192;

/// The ICY title embedded in the wrapped fixture.
const STREAM_TITLE: &str = "Nina Simone - Feeling Good";

/// The queue's station, as the radio view would queue it.
const STATION_URL: &str = "http://station.example:8000/stream";
const STATION_NAME: &str = "Test Radio";

fn station_entry() -> Entry {
    Entry::Station {
        url: STATION_URL.to_owned(),
        name: STATION_NAME.to_owned(),
    }
}

fn spec() -> TrackSpec {
    TrackSpec {
        sample_rate: 44_100,
        channels: 1,
    }
}

/// Writes a minimal mono PCM16 WAV whose frame `n` carries sample value
/// `n % 3000 - 1500`, and returns the samples a decoder must produce.
fn write_indexed_wav(path: &Path, frames: usize) -> Vec<f32> {
    let channels: u16 = 1;
    let bits_per_sample: u16 = 16;
    let data_len = u32::try_from(frames * usize::from(channels) * 2).expect("test data fits");

    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&44_100u32.to_le_bytes());
    out.extend_from_slice(&(44_100 * u32::from(channels * bits_per_sample / 8)).to_le_bytes());
    out.extend_from_slice(&(channels * bits_per_sample / 8).to_le_bytes());
    out.extend_from_slice(&bits_per_sample.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    let mut written = Vec::with_capacity(frames);
    for frame in 0..frames {
        let value = i16::try_from(frame % 3_000).expect("small ramp") - 1_500;
        out.extend_from_slice(&value.to_le_bytes());
        written.push(f32::from(value) / 32_768.0);
    }
    std::fs::write(path, out).expect("write test wav");
    written
}

/// A unique scratch path per test, removed by the caller.
fn scratch_path(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("velo-radio-queue-{tag}-{}.wav", std::process::id()))
}

/// One ICY metadata block: a length byte followed by `len * 16` bytes.
fn metadata_block(payload: &[u8]) -> Vec<u8> {
    assert_eq!(payload.len() % 16, 0, "ICY payload is padded to 16 bytes");
    let mut block = vec![u8::try_from(payload.len() / 16).expect("test payload fits one byte")];
    block.extend_from_slice(payload);
    block
}

/// Wraps the fixture's audio in ICY metadata: a `StreamTitle` block after the
/// first [`METADATA_INTERVAL`] audio bytes, empty blocks after the rest.
fn icy_fixture() -> Vec<u8> {
    let audio = std::fs::read(fixture()).expect("the fixture is readable");
    let title = format!("StreamTitle='{STREAM_TITLE}';");
    let mut padded = title.into_bytes();
    while padded.len() % 16 != 0 {
        padded.push(0);
    }
    let title_block = metadata_block(&padded);
    let empty_block = metadata_block(&[]);

    let mut wrapped = Vec::with_capacity(audio.len() + 16 * (audio.len() / METADATA_INTERVAL + 1));
    for (index, chunk) in audio.chunks(METADATA_INTERVAL).enumerate() {
        wrapped.extend_from_slice(chunk);
        wrapped.extend_from_slice(if index == 0 {
            &title_block
        } else {
            &empty_block
        });
    }
    wrapped
}

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/silence-then-tone.mp3")
}

/// Mirrors the title out of the demux on every read, the way the production
/// stream's `Observed` does: the decoder consumes the bytes, so the title is
/// readable only while they pass through.
struct Observed {
    demux: IcyReader<Cursor<Vec<u8>>>,
    last_seq: u64,
    title: Arc<Mutex<Option<String>>>,
}

impl Read for Observed {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.demux.read(buf)?;
        let seq = self.demux.title_seq();
        if seq != self.last_seq {
            self.last_seq = seq;
            *self.title.lock().expect("title state") = self.demux.title().map(str::to_owned);
        }
        Ok(n)
    }
}

/// The fake opened station: the fixture decoded exactly as the real stream
/// decodes it, plus the title the demux publishes.
struct FakeStation {
    track: TrackSource,
    title: Arc<Mutex<Option<String>>>,
}

impl StationSource for FakeStation {
    fn read_block(&mut self) -> Result<Option<Vec<f32>>, AudioError> {
        self.track.read_block()
    }

    fn spec(&self) -> TrackSpec {
        self.track.spec()
    }

    fn duration(&self) -> Option<Duration> {
        self.track.duration()
    }

    fn title(&self) -> Option<String> {
        self.title.lock().expect("title state").clone()
    }
}

/// The injected opener: records every URL, decodes the ICY-wrapped fixture
/// for every one of them, and fails the one URL it was told to fail.
struct FakeOpener {
    fixture: Vec<u8>,
    fail_on: Option<&'static str>,
    opened: Mutex<Vec<String>>,
}

impl FakeOpener {
    fn new(fail_on: Option<&'static str>) -> Self {
        Self {
            fixture: icy_fixture(),
            fail_on,
            opened: Mutex::new(Vec::new()),
        }
    }

    fn opened(&self) -> Vec<String> {
        self.opened.lock().expect("opened").clone()
    }

    fn fake_station(&self) -> Result<Box<dyn StationSource>, StreamError> {
        let title = Arc::new(Mutex::new(None));
        let observed = Observed {
            demux: IcyReader::new(Cursor::new(self.fixture.clone()), Some(METADATA_INTERVAL)),
            last_seq: 0,
            title: Arc::clone(&title),
        };
        let track =
            TrackSource::open_stream(Box::new(ReadOnlySource::new(observed))).map_err(|error| {
                StreamError::NotAudio {
                    detail: error.to_string(),
                }
            })?;
        Ok(Box::new(FakeStation { track, title }))
    }
}

impl StationOpener for FakeOpener {
    fn open(&self, url: &str) -> Result<Box<dyn StationSource>, StreamError> {
        self.opened.lock().expect("opened").push(url.to_owned());
        if self.fail_on == Some(url) {
            return Err(StreamError::BadStatus { status: 404 });
        }
        self.fake_station()
    }
}

/// A pass-through resolver that records every path it is asked about.
struct CountingResolver(Mutex<Vec<PathBuf>>);

impl CountingResolver {
    fn new() -> Self {
        Self(Mutex::new(Vec::new()))
    }

    fn paths(&self) -> Vec<PathBuf> {
        self.0.lock().expect("resolved").clone()
    }
}

impl PathResolver for CountingResolver {
    fn resolve(&self, path: &Path) -> Result<PathBuf, AudioError> {
        self.0.lock().expect("resolved").push(path.to_path_buf());
        Ok(path.to_path_buf())
    }
}

/// Drains `queue` headless with the injected seams, collecting samples,
/// failures and published progress.
fn drain(
    queue: Vec<Entry>,
    opener: &Arc<FakeOpener>,
    resolver: Option<&Arc<CountingResolver>>,
    offset_frames: u64,
    failures: &mut Vec<TrackFailure>,
    progress: &mut Vec<(Option<Duration>, Option<String>)>,
) -> Vec<f32> {
    let mut samples = Vec::new();
    let stop = AtomicBool::new(false);
    drain_headless_with_opener(
        Arc::new(Mutex::new(queue)),
        0,
        offset_frames,
        spec(),
        ReplayGainMode::default(),
        resolver.map(|resolver| Arc::clone(resolver) as Arc<dyn PathResolver>),
        Some(Arc::clone(opener) as Arc<dyn StationOpener>),
        &stop,
        &mut |block: &[f32]| samples.extend_from_slice(block),
        &mut |failure: TrackFailure| failures.push(failure),
        &mut |duration: Option<Duration>, title: Option<&str>| {
            progress.push((duration, title.map(str::to_owned)));
        },
    )
    .expect("the sequence runs");
    samples
}

/// The station's own samples, decoded straight through a fresh opener — the
/// reference every queue drain is compared against.
fn reference_station_samples() -> Vec<f32> {
    let mut source = FakeOpener::new(None)
        .open(STATION_URL)
        .expect("the fake station opens");
    let mut out = Vec::new();
    while let Some(block) = source.read_block().expect("decodes") {
        out.extend_from_slice(&block);
    }
    out
}

fn file_samples(path: &Path) -> Vec<f32> {
    let mut source = TrackSource::open(path).expect("the file opens");
    let mut out = Vec::new();
    while let Some(block) = source.read_block().expect("decodes") {
        out.extend_from_slice(&block);
    }
    out
}

// ---- the queue plays a station, then a file ---------------------------------

#[test]
fn a_queue_of_station_then_file_plays_both_through_their_own_seams() {
    let file = scratch_path("station-then-file");
    write_indexed_wav(&file, 500);
    let opener = Arc::new(FakeOpener::new(None));
    let resolver = Arc::new(CountingResolver::new());

    let mut failures = Vec::new();
    let mut progress = Vec::new();
    let samples = drain(
        vec![station_entry(), Entry::File(file.clone())],
        &opener,
        Some(&resolver),
        0,
        &mut failures,
        &mut progress,
    );

    assert!(failures.is_empty(), "nothing failed: {failures:?}");

    // The station's samples, then the file's — exactly, with nothing in
    // between, nothing overlapped and nothing repeated at the join.
    let expected = [reference_station_samples(), file_samples(&file)].concat();
    let _ = std::fs::remove_file(&file);
    assert_eq!(samples, expected, "station first, then the file, cleanly");

    // The seams never cross: the station was opened, never resolved; the
    // file was resolved, never opened.
    assert_eq!(opener.opened(), vec![STATION_URL.to_owned()]);
    assert_eq!(resolver.paths(), vec![file]);
}

#[test]
fn a_queue_of_file_then_station_plays_both_through_their_own_seams() {
    let file = scratch_path("file-then-station");
    write_indexed_wav(&file, 500);
    let opener = Arc::new(FakeOpener::new(None));
    let resolver = Arc::new(CountingResolver::new());

    let mut failures = Vec::new();
    let mut progress = Vec::new();
    let samples = drain(
        vec![Entry::File(file.clone()), station_entry()],
        &opener,
        Some(&resolver),
        0,
        &mut failures,
        &mut progress,
    );

    assert!(failures.is_empty(), "nothing failed: {failures:?}");
    let expected = [file_samples(&file), reference_station_samples()].concat();
    let _ = std::fs::remove_file(&file);
    assert_eq!(
        samples, expected,
        "the station follows the file as a new turn, never as a gapless join"
    );
    assert_eq!(opener.opened(), vec![STATION_URL.to_owned()]);
    assert_eq!(resolver.paths(), vec![file]);
}

// ---- seek (S3: a station has nothing to seek) --------------------------------

#[test]
fn a_seek_offset_is_a_no_op_for_a_station_and_seeks_a_file() {
    let opener = Arc::new(FakeOpener::new(None));
    let mut failures = Vec::new();
    let mut progress = Vec::new();

    // The same call — the same `offset_frames` — on a station queue and on a
    // file queue.
    let with_offset = drain(
        vec![station_entry()],
        &opener,
        None,
        200,
        &mut failures,
        &mut progress,
    );
    let from_start = drain(
        vec![station_entry()],
        &opener,
        None,
        0,
        &mut failures,
        &mut progress,
    );
    assert_eq!(
        with_offset, from_start,
        "a station plays from its beginning whatever offset the seek path passes"
    );

    let file = scratch_path("seek-file");
    let whole = write_indexed_wav(&file, 1_000);
    let seeked = drain(
        vec![Entry::File(file.clone())],
        &opener,
        None,
        200,
        &mut failures,
        &mut progress,
    );
    let _ = std::fs::remove_file(&file);

    assert_eq!(
        seeked,
        whole[200..],
        "the same offset seeks a file, exactly as before"
    );
}

// ---- one attempt, then the queue moves on (the retry policy is T5c2b) --------

#[test]
fn a_station_whose_opener_fails_announces_the_station_and_the_next_entry_plays() {
    let good_a = scratch_path("fail-a");
    let good_b = scratch_path("fail-b");
    write_indexed_wav(&good_a, 400);
    write_indexed_wav(&good_b, 600);
    let broken_url = "http://broken.example:8000/stream";
    let opener = Arc::new(FakeOpener::new(Some(broken_url)));
    let resolver = Arc::new(CountingResolver::new());

    let mut failures = Vec::new();
    let mut progress = Vec::new();
    let samples = drain(
        vec![
            Entry::File(good_a.clone()),
            Entry::Station {
                url: broken_url.to_owned(),
                name: "Broken FM".to_owned(),
            },
            Entry::File(good_b.clone()),
        ],
        &opener,
        Some(&resolver),
        0,
        &mut failures,
        &mut progress,
    );

    // Exactly one failure, named for the station — the name a user
    // recognises, never the URL — and announced, not fatal.
    assert_eq!(failures.len(), 1, "exactly one entry failed: {failures:?}");
    assert_eq!(failures[0].label, "Broken FM");
    assert!(
        failures[0].message.contains("404"),
        "the failure says why: {}",
        failures[0].message
    );
    let printed = format!("{} {}", failures[0].label, failures[0].message);
    assert!(
        !printed.contains("broken.example") && !printed.contains("http"),
        "the URL is never shown: {printed}"
    );

    // One attempt: the failing URL was opened once, not retried here.
    assert_eq!(opener.opened(), vec![broken_url.to_owned()]);

    // And the queue moved on: both files were heard.
    let expected = [file_samples(&good_a), file_samples(&good_b)].concat();
    let _ = std::fs::remove_file(&good_a);
    let _ = std::fs::remove_file(&good_b);
    assert_eq!(samples, expected);
}

// ---- progress: no duration, and the stream's own title ------------------------

#[test]
fn a_station_reports_no_duration_and_the_icy_title_reaches_the_publication() {
    let opener = Arc::new(FakeOpener::new(None));
    let mut failures = Vec::new();
    let mut progress = Vec::new();
    let _samples = drain(
        vec![station_entry()],
        &opener,
        None,
        0,
        &mut failures,
        &mut progress,
    );

    assert_eq!(failures, [] as [TrackFailure; 0]);
    assert!(
        !progress.is_empty(),
        "the turn published its progress at least once"
    );
    assert!(
        progress.iter().all(|(duration, _)| duration.is_none()),
        "a live station never declares a duration: {progress:?}"
    );

    let titles: Vec<&str> = progress
        .iter()
        .filter_map(|(_, title)| title.as_deref())
        .collect();
    assert_eq!(
        titles,
        vec![STREAM_TITLE],
        "the fixture's ICY title reached the publication exactly once"
    );
}

#[test]
fn the_injected_station_source_never_declares_a_duration() {
    let source = FakeOpener::new(None)
        .open(STATION_URL)
        .expect("the fake station opens");
    assert!(
        source.duration().is_none(),
        "the seam guarantees no duration before a station is played"
    );
}
