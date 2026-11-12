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
use std::io::Read;
use std::io::Seek as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use symphonia::core::io::MediaSource;
use velo::audio::TrackSource;
use velo::audio::output::{Entry, PathResolver, SharedQueue, drain_headless_with_preopen};
use velo::audio::remote::{RangeAnswer, RangeTransport, RemoteOpen, RemoteSource, WINDOW_BYTES};
use velo::config::ReplayGainMode;
use velo::subsonic::Error;
use velo::subsonic::cache::{Cache, RemoteResolver};
use velo::subsonic::client::{Transport, parse_content_range};

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

/// A temporary directory no other call shares: `local_samples` is called by
/// several tests at once, and [`temp_dir`]'s name-plus-pid would have them
/// writing — and deleting, since it starts with a `remove_dir_all` — the same
/// reference file. The reader then probes a half-written file and symphonia
/// answers "no suitable format reader found": that is the flake that failed CI
/// on `main` while the very same commit passed on the tag's own run.
fn unshared_dir(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static CALLS: AtomicU64 = AtomicU64::new(0);
    let nth = CALLS.fetch_add(1, Ordering::Relaxed);
    let dir = Path::new(&std::env::temp_dir())
        .join(format!("velo-remote-{name}-{}-{nth}", std::process::id()));
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
    /// Every request fails, the way an unreachable server does.
    fail: bool,
    /// Ranges (by their `from`) whose first request fails and whose retry
    /// succeeds — a connection that drops once and recovers.
    fail_first_at: Vec<u64>,
    /// Ranges (by their `from`) whose every request fails — a connection
    /// that stays down.
    fail_always_at: Vec<u64>,
    requests: Mutex<Vec<(u64, Option<u64>)>>,
}

impl Fixture {
    fn ranging(len: usize) -> Arc<Self> {
        Arc::new(Self {
            body: body(len),
            honor_ranges: true,
            fail: false,
            fail_first_at: Vec::new(),
            fail_always_at: Vec::new(),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn ignoring_ranges(len: usize) -> Arc<Self> {
        Arc::new(Self {
            body: body(len),
            honor_ranges: false,
            fail: false,
            fail_first_at: Vec::new(),
            fail_always_at: Vec::new(),
            requests: Mutex::new(Vec::new()),
        })
    }

    /// A transport whose every request fails, the way a connection to a
    /// server that is down does.
    fn failing() -> Arc<Self> {
        Arc::new(Self {
            body: Vec::new(),
            honor_ranges: true,
            fail: true,
            fail_first_at: Vec::new(),
            fail_always_at: Vec::new(),
            requests: Mutex::new(Vec::new()),
        })
    }

    /// A transport serving one real body — a decodable track's bytes — so
    /// the audio path can open and decode what it reads.
    fn over(body: Vec<u8>) -> Arc<Self> {
        Arc::new(Self {
            body,
            honor_ranges: true,
            fail: false,
            fail_first_at: Vec::new(),
            fail_always_at: Vec::new(),
            requests: Mutex::new(Vec::new()),
        })
    }

    /// A transport whose first request for one range fails and whose retry
    /// of the very same range succeeds — a connection that drops once
    /// mid-track and recovers (T4).
    fn dropping_once(len: usize, from: u64) -> Arc<Self> {
        Arc::new(Self {
            body: body(len),
            honor_ranges: true,
            fail: false,
            fail_first_at: vec![from],
            fail_always_at: Vec::new(),
            requests: Mutex::new(Vec::new()),
        })
    }

    /// A transport whose every request for one range fails — a connection
    /// that stays down mid-track (T4).
    fn down_at(len: usize, from: u64) -> Arc<Self> {
        Arc::new(Self {
            body: body(len),
            honor_ranges: true,
            fail: false,
            fail_first_at: Vec::new(),
            fail_always_at: vec![from],
            requests: Mutex::new(Vec::new()),
        })
    }

    fn requested(&self) -> Vec<(u64, Option<u64>)> {
        self.requests.lock().unwrap().clone()
    }
}

impl RangeTransport for Fixture {
    fn range(&self, _url: &str, from: u64, to: Option<u64>) -> Result<RangeAnswer, Error> {
        let mut requests = self.requests.lock().unwrap();
        let earlier = requests.iter().filter(|(asked, _)| *asked == from).count();
        requests.push((from, to));
        drop(requests);
        let failing = self.fail
            || self.fail_always_at.contains(&from)
            || (self.fail_first_at.contains(&from) && earlier == 0);
        if failing {
            return Err(Error::Unreachable {
                detail: "the fixture is down".to_owned(),
            });
        }
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

// ---------------------------------------- mid-track failure: one retry (T4, S5)

/// A fetch that fails mid-track is retried exactly once, for the very same
/// window, and the retry answers the read — the station rule's shape (S5):
/// a range request is stateless, so the second attempt asks what the first
/// asked.
#[test]
fn a_failed_fetch_is_retried_once_for_the_same_window_and_answers_the_read() {
    let len = usize::try_from(WINDOW_BYTES).expect("small window") * 3 + 3_711;
    let fixture = Fixture::dropping_once(len, WINDOW_BYTES);
    let mut source = RemoteSource::new(Arc::clone(&fixture), URL).expect("the probe answers");

    let drained = drain(&mut source, 7_003);

    assert_eq!(drained, body(len), "the retry answered the read");
    let requested = fixture.requested();
    assert_eq!(
        requested,
        vec![
            (0, Some(WINDOW_BYTES - 1)),
            (WINDOW_BYTES, Some(WINDOW_BYTES * 2 - 1)),
            (WINDOW_BYTES, Some(WINDOW_BYTES * 2 - 1)),
            (WINDOW_BYTES * 2, Some(WINDOW_BYTES * 3 - 1)),
            (WINDOW_BYTES * 3, Some(len as u64 - 1)),
        ],
        "the retry asked the very same range, exactly once more: {requested:?}"
    );
}

/// A second failure is the error, not a third request: the retry budget is
/// spent by the first failure, and the read fails as any failed fetch does
/// today (S5).
#[test]
fn a_second_failure_is_the_error_and_never_a_third_request() {
    let len = 100_000;
    let fixture = Fixture::down_at(len, WINDOW_BYTES);
    let mut source = RemoteSource::new(Arc::clone(&fixture), URL).expect("the probe answers");

    let mut buf = vec![0_u8; 7_003];
    let mut drained: u64 = 0;
    loop {
        match source.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => drained += n as u64,
        }
    }

    assert_eq!(
        drained, WINDOW_BYTES,
        "the reads stop at the window that stays down"
    );
    let attempts = fixture
        .requested()
        .into_iter()
        .filter(|(from, _)| *from == WINDOW_BYTES)
        .count();
    assert_eq!(
        attempts, 2,
        "exactly two attempts for the dead window, never a third"
    );
}

/// A retried window lands in the cache fill exactly once: a fetch that
/// failed answered no bytes, so the fill's `next` never advances twice for
/// the same bytes, and the completed file is the body byte for byte (S5,
/// D1).
#[test]
fn a_retried_window_is_appended_to_the_fill_once() {
    let dir = temp_dir("fill-retry");
    let cache = Cache::new(dir.clone());
    let final_path = cache.path_for(URL);
    let len = usize::try_from(WINDOW_BYTES).expect("small window") * 2 + 500;
    let fixture = Fixture::dropping_once(len, WINDOW_BYTES);
    let mut source = RemoteSource::with_cache_fill(Arc::clone(&fixture), URL, &cache)
        .expect("the probe answers");

    let drained = drain(&mut source, 7_003);

    assert_eq!(drained, body(len));
    assert!(final_path.exists(), "the retried fill still completed");
    assert_eq!(
        fs::read(&final_path).unwrap(),
        body(len),
        "the cached body holds every byte exactly once, the retried window included"
    );
    assert!(
        part_files(&dir).is_empty(),
        "no .part file survives the completion"
    );
}

/// A failure followed by a seek asks the window the read now wants, never
/// the abandoned one again: the retry belongs to one fetch, and a seek
/// replaces the window it was for (S5, S3).
#[test]
fn a_failure_followed_by_a_seek_asks_the_new_window_not_the_old() {
    let len = 100_000;
    let fixture = Fixture::down_at(len, WINDOW_BYTES);
    let mut source = RemoteSource::new(Arc::clone(&fixture), URL).expect("the probe answers");

    // Reads into the dead window fail — its retry already spent.
    let mut buf = [0_u8; 16];
    loop {
        if source.read(&mut buf).is_err() {
            break;
        }
    }
    // The seek moves on; the next read asks its own window.
    source
        .seek(std::io::SeekFrom::Start(70_000))
        .expect("the seek resolves");
    let n = source
        .read(&mut buf)
        .expect("the read after the seek answers");
    assert_eq!(n, 16, "the new window holds the whole read");
    assert_eq!(&buf, &body(len)[70_000..70_016]);

    let requested = fixture.requested();
    assert_eq!(
        requested,
        vec![
            (0, Some(WINDOW_BYTES - 1)),
            // Both attempts of the dead window clamp at the body's end.
            (WINDOW_BYTES, Some(len as u64 - 1)),
            (WINDOW_BYTES, Some(len as u64 - 1)),
            (
                70_000,
                Some((70_000 + WINDOW_BYTES - 1).min(len as u64 - 1))
            ),
        ],
        "the read after the seek asked the new window, never the abandoned one again: {requested:?}"
    );
}

/// The wait sits between the failed fetch and its retry, and it is the
/// module's named 500 ms — never slept in tests: the seam records the
/// delay and how many requests had gone out when it fired (S5).
#[test]
fn the_retry_waits_500_ms_between_its_two_attempts() {
    let len = 100_000;
    let fixture = Fixture::dropping_once(len, WINDOW_BYTES);
    let waits: Arc<Mutex<Vec<(usize, std::time::Duration)>>> = Arc::new(Mutex::new(Vec::new()));
    let watcher = Arc::clone(&fixture);
    let recorded = Arc::clone(&waits);
    let wait = Arc::new(move |delay: std::time::Duration| {
        let asked = watcher.requested().len();
        recorded.lock().unwrap().push((asked, delay));
    }) as Arc<dyn Fn(std::time::Duration) + Send + Sync>;
    let mut source = RemoteSource::new(Arc::clone(&fixture), URL)
        .expect("the probe answers")
        .with_retry_wait(wait);

    drain(&mut source, 7_003);

    assert_eq!(
        *waits.lock().unwrap(),
        vec![(2, std::time::Duration::from_millis(500))],
        "one 500 ms wait, after the failed attempt and before the retry"
    );
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

// ------------------------------------------------- the start's ask (T2, T3)

/// The probe's typed answer (T2): a ranged server gives the source the player
/// opens; a server without ranges answers the typed verdict, not an error —
/// the cache-on-play fallback is the start's decision to make (S4).
#[test]
fn the_probe_answers_how_the_url_plays() {
    let fixture = Fixture::ranging(10_000);
    assert!(
        matches!(
            velo::audio::remote::open(Arc::clone(&fixture), URL).expect("the probe answers"),
            RemoteOpen::Streams(_)
        ),
        "a ranged server streams"
    );

    let ignoring = Fixture::ignoring_ranges(10_000);
    assert!(
        matches!(
            velo::audio::remote::open(Arc::clone(&ignoring), URL)
                .expect("no ranges is not an error"),
            RemoteOpen::NoRanges
        ),
        "a server without ranges is a verdict, never an error"
    );
}

/// The fill rides on the streamed answer only: a `NoRanges` verdict attaches
/// none, so the fallback download and the fill never race onto one `.part`
/// file (S4).
#[test]
fn a_no_ranges_answer_begins_no_fill() {
    let dir = temp_dir("no-fill");
    let cache = Cache::new(dir.clone());
    let fixture = Fixture::ignoring_ranges(10_000);

    let answer =
        velo::audio::remote::open_with_fill(Arc::clone(&fixture), URL, &cache).expect("no error");
    assert!(matches!(answer, RemoteOpen::NoRanges));
    assert!(
        part_files(&dir).is_empty(),
        "a no-ranges answer began no fill"
    );
}

/// A whole-body `get` (the trait the fallback download uses), so the fixture
/// doubles as the remote-song resolver's own transport.
impl Transport for Fixture {
    fn get(&self, _url: &str) -> Result<Vec<u8>, Error> {
        Ok(self.body.clone())
    }
}

/// A track streamed once is a cache hit the second time (D1): the first play
/// streams and fills, `cached` answers true afterwards, and the second play
/// streams over an untouched cached file — no second fill, no `.part` file.
#[test]
fn a_track_played_twice_is_a_cache_hit_the_second_time() {
    let dir = temp_dir("twice");
    let cache = Cache::new(dir.clone());
    let len = usize::try_from(WINDOW_BYTES).expect("small window") * 2 + 500;
    let fixture = Fixture::ranging(len);
    let resolver = RemoteResolver::new(Arc::clone(&fixture), cache, None)
        .with_stream_transport(fixture.clone());
    let url_path = Path::new(URL);
    let final_path = cache_path(&dir, URL);

    assert!(
        !PathResolver::cached(&resolver, url_path),
        "the track is a miss to begin with"
    );
    let Some(mut source) = resolver
        .open_streaming(url_path)
        .expect("a ranged server streams")
    else {
        panic!("a ranged server streams");
    };
    drain_source(&mut *source);
    assert!(
        PathResolver::cached(&resolver, url_path),
        "the fill renamed the body into place: a replay is a cache hit"
    );
    assert_eq!(fs::read(&final_path).expect("the cached file"), body(len));

    let mut second = resolver
        .open_streaming(url_path)
        .expect("the second play asks again")
        .expect("a ranged server still streams");
    drain_source(&mut second);
    assert_eq!(
        fs::read(&final_path).expect("the cached file"),
        body(len),
        "the cached bytes stand, exactly once"
    );
    assert!(
        part_files(&dir).is_empty(),
        "the second play filled nothing — the track was already cached"
    );
}

/// A ranged URL streams through the resolver seam (T2): the open source rides
/// out of the ask, whether or not the cache already holds the body.
#[test]
fn a_ranged_url_streams_through_the_resolver() {
    let cache = Cache::new(temp_dir("ask-ranged"));
    let fixture = Fixture::ranging(10_000);
    let resolver = RemoteResolver::new(Arc::clone(&fixture), cache, None)
        .with_stream_transport(fixture.clone());

    let source = resolver
        .open_streaming(Path::new(URL))
        .expect("the ask answers");
    assert!(source.is_some(), "a ranged server streams");
}

/// A server without ranges answers the resolver seam `None` — the start falls
/// back to cache-on-play — and a failing range request is the failure it is,
/// never a fallback in disguise (S4).
#[test]
fn a_no_ranges_url_answers_the_fallback_and_a_failure_fails() {
    let cache = Cache::new(temp_dir("ask-fallback"));
    let ignoring = Fixture::ignoring_ranges(10_000);
    let resolver =
        RemoteResolver::new(Arc::clone(&ignoring), cache, None).with_stream_transport(ignoring);
    assert!(
        resolver
            .open_streaming(Path::new(URL))
            .expect("no ranges is not an error")
            .is_none(),
        "the no-ranges verdict is the cache-on-play fallback (S4)"
    );

    let cache = Cache::new(temp_dir("ask-failure"));
    let broken = Fixture::failing();
    let resolver =
        RemoteResolver::new(Arc::clone(&broken), cache, None).with_stream_transport(broken);
    assert!(
        resolver.open_streaming(Path::new(URL)).is_err(),
        "a failing request is a failure, never a silent fallback"
    );
}

/// Entries that are not a radio channel's song URL never stream: a
/// `subsonic://` row stays on cache-on-play for this slice, and a local file
/// is not a remote ask at all.
#[test]
fn rows_that_are_not_song_urls_never_stream() {
    let cache = Cache::new(temp_dir("ask-others"));
    let fixture = Fixture::ranging(10_000);
    let resolver =
        RemoteResolver::new(Arc::clone(&fixture), cache, None).with_stream_transport(fixture);

    assert!(
        resolver
            .open_streaming(Path::new("subsonic://home.server/42"))
            .expect("the ask answers")
            .is_none(),
        "a subsonic:// row stays on cache-on-play for this slice"
    );
    assert!(
        resolver
            .open_streaming(Path::new("/music/track.flac"))
            .expect("the ask answers")
            .is_none(),
        "a local file is not a remote ask"
    );
}

// --------------------------------------------- the audio path over the source

/// The hash path one URL takes in a cache directory — [`Cache::path_for`]'s
/// own spelling, read back from the outside.
fn cache_path(dir: &Path, url: &str) -> PathBuf {
    Cache::new(dir.to_path_buf()).path_for(url)
}

/// Reads a boxed source to its end, the way a decoder does.
fn drain_source(source: &mut dyn std::io::Read) {
    let mut buf = [0_u8; 4_096];
    loop {
        if source.read(&mut buf).expect("the reads work") == 0 {
            return;
        }
    }
}

/// The streamed turn over the headless drain: the fixture serves one WAV
/// body by range, the track opens through `open_remote`, and the drained
/// samples are exactly the local file's (T2, S1).
#[test]
fn a_streaming_first_turn_sounds_as_the_local_file_would() {
    let wav = fs::read(wav_fixture()).expect("the fixture reads");
    let dir = temp_dir("turn");
    let cache = Cache::new(dir.clone());
    let fixture = Fixture::over(wav.clone());
    let answer =
        velo::audio::remote::open_with_fill(Arc::clone(&fixture), URL, &cache).expect("the probe");
    let RemoteOpen::Streams(source) = answer else {
        panic!("a ranged server streams");
    };
    let opened = TrackSource::open_remote(Box::new(source)).expect("the track opens");
    let spec = opened.spec();

    let queue: SharedQueue = Arc::new(std::sync::Mutex::new(vec![Entry::File(PathBuf::from(URL))]));
    let mut heard = Vec::new();
    let mut failures = Vec::new();
    drain_headless_with_preopen(
        queue,
        0,
        0,
        spec,
        ReplayGainMode::default(),
        opened,
        &AtomicBool::new(false),
        &mut |block| heard.extend_from_slice(block),
        &mut |failure| failures.push(failure),
    )
    .expect("the sequence drains");

    assert!(
        failures.is_empty(),
        "the streaming turn is never skipped: {failures:?}"
    );
    assert_eq!(
        heard,
        local_samples(&wav, 0),
        "the streamed body decodes to exactly the local file's samples"
    );
    assert!(
        Cache::new(dir).path_for(URL).is_file(),
        "the fill completed behind the drained reads (D1)"
    );
}

/// A seek into a streamed track lands where the local file's seek lands (S3):
/// the rebuild's offset is applied to the carried source, never to the end of
/// the track and never to a prefix re-download.
#[test]
fn a_seek_into_a_streaming_track_lands_where_the_local_file_does() {
    let wav = fs::read(wav_fixture()).expect("the fixture reads");
    let dir = temp_dir("turn-seek");
    let cache = Cache::new(dir);
    let fixture = Fixture::over(wav.clone());
    let answer =
        velo::audio::remote::open_with_fill(Arc::clone(&fixture), URL, &cache).expect("the probe");
    let RemoteOpen::Streams(source) = answer else {
        panic!("a ranged server streams");
    };
    let opened = TrackSource::open_remote(Box::new(source)).expect("the track opens");
    let spec = opened.spec();

    let queue: SharedQueue = Arc::new(std::sync::Mutex::new(vec![Entry::File(PathBuf::from(URL))]));
    let mut heard = Vec::new();
    let mut failures = Vec::new();
    drain_headless_with_preopen(
        queue,
        0,
        1_000,
        spec,
        ReplayGainMode::default(),
        opened,
        &AtomicBool::new(false),
        &mut |block| heard.extend_from_slice(block),
        &mut |failure| failures.push(failure),
    )
    .expect("the sequence drains");

    assert!(
        failures.is_empty(),
        "the seeked streaming turn is never skipped: {failures:?}"
    );
    assert_eq!(
        heard,
        local_samples(&wav, 1_000),
        "the seek plays the same frames the local file's seek does"
    );
}

/// The WAV fixture's path, for the bodies the audio path decodes.
fn wav_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("mono-tone.wav")
}

/// The samples a local open of the WAV's own bytes answers, drained block by
/// block from `offset_frames` in — the reference the streaming turn is held to.
/// Each call writes its own directory (see [`unshared_dir`]): tests run in
/// parallel and the reference file must never be shared.
fn local_samples(wav: &[u8], offset_frames: u64) -> Vec<f32> {
    let dir = unshared_dir("local-reference");
    let path = dir.join("reference.wav");
    fs::write(&path, wav).expect("the reference writes");
    let mut source = if offset_frames == 0 {
        TrackSource::open(&path).expect("the reference opens")
    } else {
        TrackSource::open_at(&path, offset_frames).expect("the reference opens")
    };
    let mut samples = Vec::new();
    while let Some(block) = source.read_block().expect("the reference decodes") {
        samples.extend_from_slice(&block);
    }
    let _ = fs::remove_dir_all(&dir);
    samples
}

/// Two references never share a path (the CI flake above): the second call's
/// directory is a different one, so no call can delete or half-write another's
/// file.
#[test]
fn each_reference_gets_its_own_directory() {
    let first = unshared_dir("local-reference");
    let second = unshared_dir("local-reference");
    assert_ne!(
        first, second,
        "a shared reference path is what failed CI: one test deletes and half-writes what another is opening"
    );
    let _ = fs::remove_dir_all(&first);
    let _ = fs::remove_dir_all(&second);
}

/// And two references decoded at once both come out right: the exclusive path
/// is the whole point, so the concurrency the flake came from is exercised.
#[test]
fn two_references_at_once_both_decode() {
    let wav = fs::read(wav_fixture()).expect("the fixture reads");
    let first = {
        let wav = wav.clone();
        std::thread::spawn(move || local_samples(&wav, 1_000))
    };
    let second = std::thread::spawn(move || local_samples(&wav, 1_000));
    let first = first.join().expect("the first reference decodes");
    let second = second.join().expect("the second reference decodes");
    assert_eq!(
        first, second,
        "two references decode the same frames, each from its own file"
    );
}
