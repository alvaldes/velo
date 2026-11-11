//! A remote URL as a seekable media source (remote-streaming T1): the range
//! transport seam, the probe that learns the total length, chunked reads,
//! seeks that re-request their range (S2, S3), and the cache fill behind the
//! reads (D1).
//!
//! No test here touches the network or an audio device: the transport is a
//! fixture that serves slices of one canned body and records every range it
//! was asked for, and the cache is a throwaway temporary directory passed in
//! as an argument.

use std::fs;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use symphonia::core::io::MediaSource;
use velo::audio::remote::{RangeAnswer, RangeTransport, RemoteSource, WINDOW_BYTES};
use velo::subsonic::Error;
use velo::subsonic::cache::Cache;
use velo::subsonic::client::parse_content_range;

/// One stream URL every test shares; the fixture answers it regardless.
const URL: &str = "https://music.example.com/rest/stream?id=t1&format=raw";

/// A body where byte `i` holds `i % 251`, so a wrong offset can never hide
/// behind a constant.
fn body(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| u8::try_from(i % 251).unwrap_or(0))
        .collect()
}

/// A unique temporary directory for one test.
fn temp_dir(name: &str) -> PathBuf {
    let dir =
        Path::new(&std::env::temp_dir()).join(format!("velo-remote-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// The `.part-…` files currently in a cache directory: what the prune rules
/// keep counting while a fill is under way or abandoned.
fn part_files(dir: &Path) -> Vec<PathBuf> {
    let mut parts: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(".part-"))
        })
        .collect();
    parts.sort();
    parts
}

/// The fixture transport: serves slices of one canned body as `206` answers
/// carrying the body's total length, or — when `honor_ranges` is false — the
/// typed no-ranges answer, the way a server that ignores `Range` does.
/// Every requested range is recorded, in order.
struct Fixture {
    body: Vec<u8>,
    honor_ranges: bool,
    requests: Mutex<Vec<(u64, Option<u64>)>>,
}

impl Fixture {
    fn ranging(len: usize) -> Arc<Self> {
        Arc::new(Self {
            body: body(len),
            honor_ranges: true,
            requests: Mutex::new(Vec::new()),
        })
    }

    fn ignoring_ranges(len: usize) -> Arc<Self> {
        Arc::new(Self {
            body: body(len),
            honor_ranges: false,
            requests: Mutex::new(Vec::new()),
        })
    }

    fn requested(&self) -> Vec<(u64, Option<u64>)> {
        self.requests.lock().unwrap().clone()
    }
}

impl RangeTransport for Fixture {
    fn range(&self, _url: &str, from: u64, to: Option<u64>) -> Result<RangeAnswer, Error> {
        self.requests.lock().unwrap().push((from, to));
        if !self.honor_ranges {
            return Ok(RangeAnswer::NoRanges);
        }
        let total = self.body.len() as u64;
        // `to` is inclusive; the slice is not.
        let end = to.map_or(total, |to| (to + 1).min(total));
        let from = usize::try_from(from).expect("fixture bodies are small");
        let end = usize::try_from(end).expect("fixture bodies are small");
        let bytes = self.body[from..end].to_vec();
        Ok(RangeAnswer::Partial { bytes, total })
    }
}

/// Reads the whole source through `buf_len`-sized reads, the way a decoder's
/// `MediaSourceStream` does.
fn drain(source: &mut RemoteSource, buf_len: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = vec![0_u8; buf_len];
    loop {
        let n = source.read(&mut buf).expect("reads work");
        if n == 0 {
            return out;
        }
        out.extend_from_slice(&buf[..n]);
    }
}

/// Reads exactly `count` bytes, the way a probe of a container's head does.
fn read_exact_bytes(source: &mut RemoteSource, count: usize) -> Vec<u8> {
    let mut out = vec![0_u8; count];
    let mut read = 0;
    while read < count {
        let n = source
            .read(&mut out[read..])
            .expect("reads work before the end");
        assert!(
            n > 0,
            "the body ended {read} bytes into a {count}-byte read"
        );
        read += n;
    }
    out
}

// ------------------------------------------------------------------ the probe

/// The one probe request establishes the body's total length, and the source
/// reports it — that is what makes the decoder able to state a duration (S2).
#[test]
fn the_probe_learns_the_total_length_and_the_source_is_seekable() {
    let fixture = Fixture::ranging(100_000);
    let source = RemoteSource::new(Arc::clone(&fixture), URL).expect("the probe answers");

    assert_eq!(
        source.byte_len(),
        Some(100_000),
        "the probe learned the total"
    );
    assert!(source.is_seekable(), "a range source is seekable (S2)");
    assert_eq!(
        fixture.requested(),
        vec![(0, Some(WINDOW_BYTES - 1))],
        "the probe is one small range from byte 0"
    );
}

// -------------------------------------------------------------- sequential reads

/// Sequential reads crossing window boundaries return the whole body in
/// order: the windows are consecutive ranges, fetched one after the other,
/// and their bytes join into exactly the body (S1).
#[test]
fn sequential_reads_crossing_window_boundaries_return_the_whole_body_in_order() {
    // Three windows and a part one: every boundary is crossed.
    let len = usize::try_from(WINDOW_BYTES).expect("small window") * 3 + 3_711;
    let fixture = Fixture::ranging(len);
    let mut source = RemoteSource::new(Arc::clone(&fixture), URL).expect("the probe answers");

    // A read size that does not line up with the window, so every read
    // sequence crosses each boundary mid-buffer.
    let drained = drain(&mut source, 7_003);

    assert_eq!(drained, body(len), "the whole body, in order");
    let froms: Vec<u64> = fixture.requested().iter().map(|(from, _)| *from).collect();
    assert_eq!(
        froms,
        (0..4).map(|n| n * WINDOW_BYTES).collect::<Vec<u64>>(),
        "the windows are consecutive ranges from byte 0"
    );

    // At the end of the body the source is at EOF, not stuck.
    let mut buf = [0_u8; 16];
    assert_eq!(source.read(&mut buf).expect("eof reads"), 0);
}

// --------------------------------------------------------------------- seeks

/// A forwards and a backwards seek each issue the range the position asks
/// for: a seek is a new range request at the seek point, never a resolve to
/// the end of the track (S3).
#[test]
fn a_seek_forwards_and_backwards_issues_the_range_the_position_asks_for() {
    let fixture = Fixture::ranging(100_000);
    let mut source = RemoteSource::new(Arc::clone(&fixture), URL).expect("the probe answers");

    // Forwards, past the probed window: its bytes are already in hand, so
    // the first position that must fetch is one beyond it.
    source
        .seek(std::io::SeekFrom::Start(70_000))
        .expect("the seek resolves");
    let mut buf = [0_u8; 10];
    let n = source.read(&mut buf).expect("the read after the seek");
    assert_eq!(n, 10, "the window holds the whole read");
    assert_eq!(&buf, &body(100_000)[70_000..70_010]);

    // Backwards, to a position outside the current window.
    source
        .seek(std::io::SeekFrom::Start(20_000))
        .expect("the seek resolves");
    let mut buf = [0_u8; 10];
    let n = source.read(&mut buf).expect("the read after the seek");
    assert_eq!(n, 10, "the window holds the whole read");
    assert_eq!(&buf, &body(100_000)[20_000..20_010]);

    let requested = fixture.requested();
    let froms: Vec<u64> = requested.iter().map(|(from, _)| *from).collect();
    assert_eq!(
        froms,
        vec![0, 70_000, 20_000],
        "each seek fetched exactly the range its position asks for: {requested:?}"
    );
}

/// A symphonia reader positions itself from the end for a container's
/// trailer: `SeekFrom::End` must resolve against the probed total (S3).
#[test]
fn a_seek_from_the_end_reads_the_trailer_range() {
    let fixture = Fixture::ranging(100_000);
    let mut source = RemoteSource::new(Arc::clone(&fixture), URL).expect("the probe answers");

    source
        .seek(std::io::SeekFrom::End(-5))
        .expect("the seek from the end resolves");
    let mut buf = [0_u8; 5];
    let n = source.read(&mut buf).expect("the trailer read");
    assert_eq!(n, 5, "the window holds the whole read");

    assert_eq!(&buf, &body(100_000)[99_995..]);
}

// ------------------------------------------------------------- the typed answer

/// A server that answers `200` — ignoring the `Range` header — gives the
/// typed no-ranges answer, never a silent whole-body read (S4's seam).
#[test]
fn a_200_answer_is_the_typed_no_ranges_answer() {
    let fixture = Fixture::ignoring_ranges(1_000);
    let answer = fixture
        .range(URL, 0, Some(9))
        .expect("the request goes out");
    assert_eq!(answer, RangeAnswer::NoRanges);

    // Without ranges there is no media source to build: the constructor says
    // so, and the fallback (T3) decides what happens next.
    let error = RemoteSource::new(fixture, URL).unwrap_err();
    assert!(
        error.to_string().contains("range"),
        "the error names the missing ranges: {error}"
    );
}

/// A partial answer without a parsable total is the same typed answer.
#[test]
fn a_partial_answer_without_a_total_is_no_ranges() {
    assert_eq!(parse_content_range("bytes 0-99/*"), None);
    assert_eq!(parse_content_range("bytes 0-99/1000"), Some((0, 99, 1000)));
    assert_eq!(parse_content_range("bytes */1000"), None);
    assert_eq!(parse_content_range("text/html"), None);
}

// --------------------------------------------------------------- the cache fill

/// While the source reads sequentially from byte 0 the same bytes are
/// appended to the cache's `.part` file; delivering the whole body renames
/// it into place exactly as `Cache::store` does, so a later play is a cache
/// hit (D1).
#[test]
fn the_cache_file_is_written_and_renamed_exactly_at_completion() {
    let dir = temp_dir("fill");
    let cache = Cache::new(dir.clone());
    let final_path = cache.path_for(URL);
    let len = usize::try_from(WINDOW_BYTES).expect("small window") * 2 + 500;
    let fixture = Fixture::ranging(len);
    let mut source = RemoteSource::with_cache_fill(Arc::clone(&fixture), URL, &cache)
        .expect("the probe answers");

    // Half the body read, the body is not complete: nothing may carry the
    // final name yet.
    let half = read_exact_bytes(&mut source, len / 2);
    assert!(
        !final_path.exists(),
        "a half-delivered body never gets the final name"
    );

    let drained = [half, drain(&mut source, 7_003)].concat();
    assert_eq!(drained, body(len));

    assert!(
        final_path.exists(),
        "completion renamed the file into place"
    );
    assert_eq!(fs::read(&final_path).unwrap(), body(len));
    assert!(
        part_files(&dir).is_empty(),
        "the .part file did not survive the rename"
    );
}

/// A seek that skips ahead before the body is complete stops the fill: the
/// half file never gets the final name, and the `.part` file stays behind
/// for the prune rules to keep counting (D1).
#[test]
fn a_seek_that_skips_ahead_abandons_the_fill_and_never_names_the_half_file() {
    let dir = temp_dir("fill-abandon");
    let cache = Cache::new(dir.clone());
    let final_path = cache.path_for(URL);
    let fixture = Fixture::ranging(100_000);
    let mut source = RemoteSource::with_cache_fill(Arc::clone(&fixture), URL, &cache)
        .expect("the probe answers");

    // The probed window is appended; a seek past it would leave a gap.
    let read = read_exact_bytes(&mut source, 10);
    assert_eq!(read, body(100_000)[..10]);
    source
        .seek(std::io::SeekFrom::Start(70_000))
        .expect("the seek resolves");
    let read = read_exact_bytes(&mut source, 10);
    assert_eq!(read, body(100_000)[70_000..70_010]);

    assert!(
        !final_path.exists(),
        "the abandoned fill never gets the final name"
    );
    let parts = part_files(&dir);
    assert_eq!(
        parts.len(),
        1,
        "the abandoned .part file stays for the prune rules to count"
    );
}

/// A backwards seek opens no gap: the fill survives it and completes with
/// the body exactly once — no duplicated prefix under the final name.
#[test]
fn a_backwards_seek_keeps_the_fill_and_completes_it_without_duplicates() {
    let dir = temp_dir("fill-backwards");
    let cache = Cache::new(dir.clone());
    let final_path = cache.path_for(URL);
    let fixture = Fixture::ranging(100_000);
    let mut source = RemoteSource::with_cache_fill(Arc::clone(&fixture), URL, &cache)
        .expect("the probe answers");

    let read = read_exact_bytes(&mut source, 10);
    assert_eq!(read, body(100_000)[..10]);
    source
        .seek(std::io::SeekFrom::Start(5))
        .expect("the seek resolves");
    let drained = drain(&mut source, 7_003);

    assert_eq!(
        drained,
        body(100_000)[5..],
        "the reads continue from the seek point"
    );
    assert!(final_path.exists(), "the gapless fill completed");
    assert_eq!(
        fs::read(&final_path).unwrap(),
        body(100_000),
        "the cached body holds every byte exactly once"
    );
    assert!(
        part_files(&dir).is_empty(),
        "no .part file survives the completion"
    );
}
