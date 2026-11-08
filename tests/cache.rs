//! The cache of remote tracks (T12c): the directory, the stable filename for
//! a stream URL, and the fetch that fills it.
//!
//! No test here touches the network: the fetch goes through a canned
//! [`Transport`], and the resolver through fakes that serve files or refuse.
//! The directory is always a throwaway temporary directory passed in as an
//! argument, so no test reads or writes the user's real cache and no test
//! ever touches an environment variable (`set_var` is `unsafe` in edition
//! 2024 and this repository forbids unsafe code).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use velo::audio::output::{Entry, PathResolver, drain_headless_resolving};
use velo::audio::{AudioError, TrackSpec};
use velo::subsonic::Error;
use velo::subsonic::cache::{Cache, SubsonicResolver, Unconfigured, cache_dir};
use velo::subsonic::client::{Auth, Client, Transport};

// ------------------------------------------------------------- infrastructure

/// A unique temporary directory for one test.
fn temp_dir(name: &str) -> PathBuf {
    let dir =
        Path::new(&std::env::temp_dir()).join(format!("velo-cache-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A transport that answers with the next canned body and counts every call.
struct Counting {
    bodies: Mutex<Vec<Vec<u8>>>,
    calls: AtomicUsize,
}

impl Counting {
    fn with(bodies: &[&[u8]]) -> Self {
        Self {
            bodies: Mutex::new(bodies.iter().map(|body| body.to_vec()).collect()),
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Transport for Counting {
    fn get(&self, url: &str) -> Result<Vec<u8>, Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert!(url.contains("/rest/stream?"), "the stream endpoint: {url}");
        assert!(url.contains("format=raw"), "no transcoding: {url}");
        let mut bodies = self.bodies.lock().unwrap();
        if bodies.is_empty() {
            return Err(Error::Unreachable {
                detail: "no canned body left".to_owned(),
            });
        }
        Ok(bodies.remove(0))
    }
}

// ------------------------------------------------------------------ filenames

#[test]
fn the_same_url_names_the_same_file_twice() {
    let cache = Cache::new(temp_dir("same"));
    let url = "https://music.example.com/rest/stream?id=ab-12&format=raw";
    assert_eq!(cache.path_for(url), cache.path_for(url));
}

#[test]
fn two_different_urls_never_share_a_file() {
    let cache = Cache::new(temp_dir("distinct"));
    let first = cache.path_for("https://music.example.com/rest/stream?id=ab-12&format=raw");
    let second = cache.path_for("https://music.example.com/rest/stream?id=cd-34&format=raw");
    let third = cache.path_for("https://elsewhere.example.com/rest/stream?id=ab-12&format=raw");
    assert_ne!(first, second);
    assert_ne!(first, third);
    assert_ne!(second, third);
}

// ---------------------------------------------------------------- the fetch

#[test]
fn a_miss_downloads_and_the_file_holds_the_bytes() {
    let cache = Cache::new(temp_dir("miss"));
    let path = cache
        .ensure(
            "https://music.example.com/rest/stream?id=ab&format=raw",
            || Ok(b"audio-bytes".to_vec()),
        )
        .unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"audio-bytes");
}

#[test]
fn a_hit_needs_no_network() {
    let cache = Cache::new(temp_dir("hit"));
    let url = "https://music.example.com/rest/stream?id=ab&format=raw";
    let path = cache
        .ensure(url, || Ok(b"audio-bytes".to_vec()))
        .expect("the first fetch fills the cache");
    let again = cache.ensure(url, || {
        panic!("the transport must not be called on a cache hit")
    });
    assert_eq!(again.unwrap(), path);
    assert_eq!(fs::read(&path).unwrap(), b"audio-bytes");
}

#[test]
fn a_failed_download_leaves_no_file_behind() {
    let dir = temp_dir("failed");
    let cache = Cache::new(dir.clone());
    let error = cache
        .ensure(
            "https://music.example.com/rest/stream?id=ab&format=raw",
            || {
                Err(Error::Unreachable {
                    detail: "connection refused".to_owned(),
                })
            },
        )
        .unwrap_err();
    assert!(error.to_string().contains("could not be reached"));
    assert!(
        fs::read_dir(&dir).unwrap().next().is_none(),
        "no partial file may survive a failed download"
    );
}

// ------------------------------------------------------------- the directory

#[test]
fn the_cache_directory_follows_the_environment() {
    let home: &std::ffi::OsStr = "/home/demo".as_ref();
    let xdg: &std::ffi::OsStr = "/xdg-cache".as_ref();
    assert_eq!(
        cache_dir(Some(xdg), Some(home)),
        Some(PathBuf::from("/xdg-cache/velo/subsonic"))
    );
    assert_eq!(
        cache_dir(None, Some(home)),
        Some(PathBuf::from("/home/demo/.cache/velo/subsonic"))
    );
    // An empty value counts as unset, the way `config_path` reads them.
    assert_eq!(
        cache_dir(Some("".as_ref()), Some(home)),
        Some(PathBuf::from("/home/demo/.cache/velo/subsonic"))
    );
}

// ---------------------------------------------------------------- the resolver

#[test]
fn a_remote_path_resolves_through_the_cache_once() {
    let dir = temp_dir("resolver");
    let transport = Counting::with(&[b"canned-audio"]);
    let client = Client::new(
        &transport,
        "https://music.example.com",
        "demo",
        Auth::Token {
            password: "demo".to_owned(),
        },
    );
    let resolver = SubsonicResolver::new(
        client,
        "subsonic:music.example.com".to_owned(),
        Cache::new(dir.clone()),
    );

    let first = resolver
        .resolve(Path::new("subsonic://music.example.com/s-1"))
        .expect("the first resolve downloads");
    let second = resolver
        .resolve(Path::new("subsonic://music.example.com/s-1"))
        .expect("the second resolve hits the cache");
    assert_eq!(first, second, "one track, one cached file");
    assert_eq!(transport.calls(), 1, "the hit needed no network");
    assert_eq!(fs::read(&first).unwrap(), b"canned-audio");
    assert!(
        first.starts_with(&dir),
        "the file lives in the cache directory"
    );
}

#[test]
fn a_local_file_passes_through_the_resolver_untouched() {
    // A queue can mix sources (T12b): a local file must reach the producer
    // exactly as it is, and never cost a request.
    let transport = Counting::with(&[]);
    let client = Client::new(
        &transport,
        "https://music.example.com",
        "demo",
        Auth::Token {
            password: "demo".to_owned(),
        },
    );
    let resolver = SubsonicResolver::new(
        client,
        "subsonic:music.example.com".to_owned(),
        Cache::new(temp_dir("passthrough")),
    );
    let local = Path::new("/music/Library/Artist/2020 - Home/01 - Here.wav");
    assert_eq!(
        resolver.resolve(local).unwrap(),
        local.to_path_buf(),
        "a local file is already the file to open"
    );
    assert_eq!(transport.calls(), 0, "no request for a local file");
}

#[test]
fn a_track_from_another_server_is_refused() {
    let transport = Counting::with(&[]);
    let client = Client::new(
        &transport,
        "https://music.example.com",
        "demo",
        Auth::Token {
            password: "demo".to_owned(),
        },
    );
    let resolver = SubsonicResolver::new(
        client,
        "subsonic:music.example.com".to_owned(),
        Cache::new(temp_dir("foreign")),
    );
    let error = resolver
        .resolve(Path::new("subsonic://elsewhere.example.com/s-1"))
        .unwrap_err();
    assert!(error.to_string().contains("elsewhere.example.com"));
    assert_eq!(transport.calls(), 0);
}

#[test]
fn a_malformed_pseudo_path_is_refused_not_opened() {
    let transport = Counting::with(&[]);
    let client = Client::new(
        &transport,
        "https://music.example.com",
        "demo",
        Auth::Token {
            password: "demo".to_owned(),
        },
    );
    let resolver = SubsonicResolver::new(
        client,
        "subsonic:music.example.com".to_owned(),
        Cache::new(temp_dir("malformed")),
    );
    // A `subsonic://` path without a song id is a broken row: refusing it says
    // so; passing it through would try to open the URL as a file.
    assert!(
        resolver
            .resolve(Path::new("subsonic://music.example.com"))
            .is_err()
    );
}

#[test]
fn an_unresolved_server_skips_its_tracks_and_keeps_local_ones() {
    let resolver = Unconfigured::new("no subsonic secret found — set api_key".to_owned());
    let error = resolver
        .resolve(Path::new("subsonic://music.example.com/s-1"))
        .unwrap_err();
    assert!(error.to_string().contains("no subsonic secret found"));
    let local = Path::new("/music/Library/here.wav");
    assert_eq!(resolver.resolve(local).unwrap(), local.to_path_buf());
}

// ------------------------------------------------------------- the eviction

/// Sets `path`'s modification time to `ago` seconds before now, so the tests
/// control the eviction order without sleeping.
fn backdate(path: &Path, seconds_ago: u64) {
    let modified = SystemTime::now()
        .checked_sub(Duration::from_secs(seconds_ago))
        .expect("a time before now");
    let file = fs::File::options()
        .write(true)
        .open(path)
        .unwrap_or_else(|error| panic!("cannot open {}: {error}", path.display()));
    file.set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap_or_else(|error| panic!("cannot backdate {}: {error}", path.display()));
}

/// Three cached files, oldest first, each 400 bytes, under a 1 000-byte cap:
/// storing one more must evict the two oldest in modification-time order and
/// leave the third and the fresh file in place (800 ≤ 1 000).
#[test]
fn pruning_removes_the_oldest_files_in_mtime_order_until_under_the_cap() {
    let dir = temp_dir("prune-order");
    let cache = Cache::new(dir).with_cap(1_000);
    let url = |id: &str| format!("https://music.example.com/rest/stream?id={id}&format=raw");
    let oldest = cache.path_for(&url("oldest"));
    let middle = cache.path_for(&url("middle"));
    let newer = cache.path_for(&url("newer"));
    for path in [&oldest, &middle, &newer] {
        fs::write(path, vec![0u8; 400]).unwrap();
    }
    backdate(&oldest, 30);
    backdate(&middle, 20);
    backdate(&newer, 10);

    let fresh = cache
        .ensure(&url("fresh"), || Ok(vec![0u8; 400]))
        .expect("the store succeeds");

    assert!(!oldest.exists(), "the oldest file is evicted first");
    assert!(
        !middle.exists(),
        "then the second-oldest, until under the cap"
    );
    assert!(
        newer.exists(),
        "a newer file survives: eviction is by mtime"
    );
    assert!(fresh.exists(), "the file just stored survives");
}

/// Nothing evicts from a cache that is already under its cap — not the old
/// files, not the file just stored.
#[test]
fn a_cache_under_its_cap_is_untouched() {
    let dir = temp_dir("prune-quiet");
    let cache = Cache::new(dir).with_cap(10_000);
    let url = "https://music.example.com/rest/stream?id=quiet&format=raw";
    let old = cache.path_for(url);
    fs::write(&old, vec![7u8; 500]).unwrap();
    backdate(&old, 60);

    let fresh = cache
        .ensure(
            "https://music.example.com/rest/stream?id=fresh&format=raw",
            || Ok(vec![0u8; 500]),
        )
        .expect("the store succeeds");

    assert!(old.exists());
    assert!(fresh.exists());
    assert_eq!(fs::read(&old).unwrap(), vec![7u8; 500]);
}

/// The one file that may never be evicted is the one just stored: eviction
/// that removed it would re-download it on the next play — a loop, not a
/// policy. It survives even when it alone is larger than the cap.
#[test]
fn the_file_just_stored_survives_even_when_it_alone_exceeds_the_cap() {
    let dir = temp_dir("prune-keep");
    let cache = Cache::new(dir).with_cap(100);
    let url = "https://music.example.com/rest/stream?id=old&format=raw";
    let old = cache.path_for(url);
    fs::write(&old, vec![0u8; 60]).unwrap();
    backdate(&old, 60);

    let fresh = cache
        .ensure(
            "https://music.example.com/rest/stream?id=big&format=raw",
            || Ok(vec![9u8; 500]),
        )
        .expect("the store succeeds");

    assert!(!old.exists(), "the older file is evicted");
    assert!(
        fresh.exists(),
        "the file just stored is kept even over the cap"
    );
    assert_eq!(fs::read(&fresh).unwrap(), vec![9u8; 500]);
}

/// Eviction walks one flat directory and removes files only from it: a file
/// beside the cache and a file inside a subdirectory of it must both survive
/// a prune that runs.
#[test]
fn pruning_never_touches_anything_outside_the_cache_directory() {
    let root = temp_dir("prune-scope");
    let cache_dir = root.join("velo").join("subsonic");
    let cache = Cache::new(cache_dir.clone()).with_cap(10);
    fs::create_dir_all(&cache_dir).unwrap();
    let sibling = root.join("velo").join("unrelated.txt");
    fs::write(&sibling, b"keep me").unwrap();
    let nested_dir = cache_dir.join("nested");
    fs::create_dir_all(&nested_dir).unwrap();
    let nested = nested_dir.join("nested-file");
    fs::write(&nested, b"keep me too").unwrap();

    let old = cache.path_for("https://music.example.com/rest/stream?id=old&format=raw");
    fs::write(&old, vec![0u8; 60]).unwrap();
    backdate(&old, 60);
    let fresh = cache
        .ensure(
            "https://music.example.com/rest/stream?id=big&format=raw",
            || Ok(vec![0u8; 500]),
        )
        .expect("the store succeeds");

    assert!(!old.exists(), "the eviction ran");
    assert!(fresh.exists());
    assert!(
        sibling.exists(),
        "nothing outside the cache is ever removed"
    );
    assert!(
        nested.exists(),
        "only files directly in the cache are evicted"
    );
}

// ------------------------------------------------------------------ cleaning

/// `--cache-clean`'s engine: every file directly inside the directory goes,
/// the count and the byte total are what was there, the directory survives,
/// and a subdirectory with its contents is left alone.
#[test]
fn clean_removes_every_file_in_the_directory_and_counts_what_it_freed() {
    let dir = temp_dir("clean");
    fs::write(dir.join("a"), vec![0u8; 100]).unwrap();
    fs::write(dir.join("b"), vec![0u8; 250]).unwrap();
    fs::create_dir_all(dir.join("sub")).unwrap();
    fs::write(dir.join("sub").join("c"), b"kept").unwrap();

    let (files, bytes) = velo::subsonic::cache::clean(&dir);

    assert_eq!(files, 2, "two files removed");
    assert_eq!(bytes, 350, "exactly the bytes that were there");
    assert!(dir.is_dir(), "the directory survives the cleaning");
    let left: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    assert_eq!(left, vec![dir.join("sub")], "only the subdirectory is left");
    assert!(dir.join("sub").join("c").exists());
}

/// Cleaning a cache that does not exist yet creates it — both caches must
/// exist and be empty when `--cache-clean` ends.
#[test]
fn clean_creates_a_missing_cache_directory() {
    let dir = temp_dir("clean-missing").join("velo").join("subsonic");
    let (files, bytes) = velo::subsonic::cache::clean(&dir);
    assert_eq!((files, bytes), (0, 0));
    assert!(dir.is_dir());
    assert!(fs::read_dir(&dir).unwrap().next().is_none());
}

// ------------------------------------------------------- the --cache-clean CLI

/// The binary cleans both caches in one run: per-cache counts and bytes on
/// stdout, exit 0, both directories still existing and empty, and the file a
/// user happened to keep beside them untouched.
#[test]
fn cache_clean_empties_both_caches_reports_what_it_freed_and_exits_zero() {
    let root = temp_dir("clean-binary");
    let config = temp_dir("clean-binary-config");
    let subsonic = root.join("velo").join("subsonic");
    let covers = root.join("velo").join("covers");
    fs::create_dir_all(&subsonic).unwrap();
    fs::create_dir_all(&covers).unwrap();
    fs::write(subsonic.join("one"), vec![0u8; 100]).unwrap();
    fs::write(subsonic.join("two"), vec![0u8; 200]).unwrap();
    fs::write(covers.join("img"), vec![0u8; 50]).unwrap();
    let sibling = root.join("velo").join("unrelated.txt");
    fs::write(&sibling, b"keep me").unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_velo"))
        .arg("--cache-clean")
        .env("XDG_CACHE_HOME", root.as_os_str())
        .env("XDG_CONFIG_HOME", config.as_os_str())
        .output()
        .expect("the binary runs");

    assert_eq!(output.status.code(), Some(0), "cleaning always succeeds");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("2 files"), "{stdout}");
    assert!(stdout.contains("300 B"), "{stdout}");
    assert!(stdout.contains("1 file"), "{stdout}");
    assert!(stdout.contains("50 B"), "{stdout}");
    assert!(stdout.contains("covers"), "{stdout}");
    assert!(stdout.contains("subsonic"), "{stdout}");
    for dir in [&subsonic, &covers] {
        assert!(dir.is_dir(), "{} still exists", dir.display());
        assert!(
            fs::read_dir(dir).unwrap().next().is_none(),
            "{} is empty",
            dir.display()
        );
    }
    assert!(
        sibling.exists(),
        "nothing outside the two caches is touched"
    );
}

// ------------------------------------------------- playback through the cache

/// Writes a minimal PCM16 WAV file so the tests need no external tooling.
fn write_wav(path: &Path, spec: TrackSpec, frames: &[i16]) {
    let bits_per_sample: u16 = 16;
    let block_align = spec.channels * bits_per_sample / 8;
    let byte_rate = spec.sample_rate * u32::from(block_align);
    let data_len = u32::try_from(frames.len() * 2).expect("test data fits in a RIFF chunk");

    let mut out = Vec::with_capacity(44 + frames.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&spec.channels.to_le_bytes());
    out.extend_from_slice(&spec.sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits_per_sample.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for sample in frames {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, out).expect("write test wav");
}

/// The `i16` PCM the test wrote, as the `f32` samples a decoder produces.
fn as_f32(samples: &[i16]) -> Vec<f32> {
    samples
        .iter()
        .map(|&sample| f32::from(sample) / 32_768.0)
        .collect()
}

const SPEC: TrackSpec = TrackSpec {
    sample_rate: 44_100,
    channels: 2,
};

/// A resolver that maps a song id to a file on disk: what the real one
/// becomes after the fetch, and what lets the playback tests run offline.
struct FakeResolver {
    files: Vec<(String, PathBuf)>,
    /// How many times the queue asked it to resolve, shared so a test can
    /// read it after the resolver has been handed to the drain.
    calls: Arc<AtomicUsize>,
}

impl FakeResolver {
    fn serving(id: &str, path: PathBuf) -> Self {
        Self {
            files: vec![(id.to_owned(), path)],
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl PathResolver for FakeResolver {
    fn resolve(&self, path: &Path) -> Result<PathBuf, AudioError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let text = path.to_str().expect("a pseudo path is utf-8");
        let (_, id) = text
            .strip_prefix("subsonic://")
            .expect("a pseudo path")
            .split_once('/')
            .expect("an id");
        self.files
            .iter()
            .find(|(key, _)| key == id)
            .map(|(_, file)| file.clone())
            .ok_or_else(|| AudioError::Unsupported(format!("no fixture for {id}")))
    }
}

/// Two remote entries drain exactly the two fixtures, in order, with nothing
/// between them: the producer resolves each entry when it reaches it and the
/// audio path plays the cached file as if the user had queued it.
#[test]
fn a_remote_queue_drains_exactly_the_fixtures_the_resolver_serves() {
    let dir = temp_dir("drain");
    let first = dir.join("first.wav");
    let second = dir.join("second.wav");
    let first_pcm: Vec<i16> = (0..2_000)
        .map(|frame| i16::try_from(frame % 700 + 1).expect("in range"))
        .collect();
    let second_pcm: Vec<i16> = (0..2_000)
        .map(|frame| i16::try_from(frame % 300 + 1_000).expect("in range"))
        .collect();
    write_wav(&first, SPEC, &first_pcm);
    write_wav(&second, SPEC, &second_pcm);

    let queue = Arc::new(Mutex::new(vec![
        Entry::File(PathBuf::from("subsonic://music.example.com/a")),
        Entry::File(PathBuf::from("subsonic://music.example.com/b")),
    ]));
    let mut resolver = FakeResolver::serving("a", first.clone());
    resolver.files.push(("b".to_owned(), second.clone()));

    let mut heard: Vec<f32> = Vec::new();
    let mut failures: Vec<velo::audio::output::TrackFailure> = Vec::new();
    drain_headless_resolving(
        queue,
        0,
        0,
        SPEC,
        velo::config::ReplayGainMode::default(),
        Some(Arc::new(resolver)),
        &std::sync::atomic::AtomicBool::new(false),
        &mut |block| heard.extend_from_slice(block),
        &mut |failure| failures.push(failure),
    )
    .expect("the sequence drains");

    let expected = [as_f32(&first_pcm), as_f32(&second_pcm)].concat();
    assert_eq!(heard, expected, "the cached fixtures, back to back");
    assert!(
        failures.is_empty(),
        "nothing had to be skipped: {failures:?}"
    );
}

/// A download that fails costs its own track and nothing else: the entry is
/// announced once, with the reason, and the next local track still plays.
#[test]
fn a_failed_download_is_announced_once_and_the_next_local_track_still_plays() {
    let dir = temp_dir("skip");
    let local = dir.join("local.wav");
    let local_pcm: Vec<i16> = (0..2_000)
        .map(|frame| i16::try_from(frame % 500 + 200).expect("in range"))
        .collect();
    write_wav(&local, SPEC, &local_pcm);

    let queue = Arc::new(Mutex::new(vec![
        Entry::File(PathBuf::from("subsonic://music.example.com/dead")),
        Entry::File(local.clone()),
    ]));

    let mut heard: Vec<f32> = Vec::new();
    let mut failures: Vec<velo::audio::output::TrackFailure> = Vec::new();
    drain_headless_resolving(
        queue,
        0,
        0,
        SPEC,
        velo::config::ReplayGainMode::default(),
        Some(Arc::new(Unconfigured::new(
            "the server could not be reached: connection refused".to_owned(),
        ))),
        &std::sync::atomic::AtomicBool::new(false),
        &mut |block| heard.extend_from_slice(block),
        &mut |failure| failures.push(failure),
    )
    .expect("a skip is not a sequence error");

    assert_eq!(heard, as_f32(&local_pcm), "the local track still plays");
    assert_eq!(failures.len(), 1, "the failed download is announced once");
    assert_eq!(
        failures[0].path,
        PathBuf::from("subsonic://music.example.com/dead"),
        "the announcement names the queue entry"
    );
    assert!(failures[0].message.contains("could not be reached"));
}

/// A file entry round-trips: the path a caller queues is stored as a file
/// entry and comes back as the same path, and when the producer reaches it
/// the resolver is consulted for it exactly once per start.
#[test]
fn a_file_entry_round_trips_through_the_queue_and_resolves_once_per_start() {
    let dir = temp_dir("entry");
    let file = dir.join("round.wav");
    let pcm: Vec<i16> = (0..1_000)
        .map(|frame| i16::try_from(frame % 400 + 30).expect("in range"))
        .collect();
    write_wav(&file, SPEC, &pcm);

    let entry = PathBuf::from("subsonic://music.example.com/round");
    let queue: Arc<Mutex<Vec<Entry>>> = Arc::new(Mutex::new(vec![Entry::File(entry.clone())]));
    assert_eq!(
        queue.lock().expect("queue").clone(),
        vec![Entry::File(entry.clone())],
        "the path became a file entry and stayed one"
    );

    let resolver = FakeResolver::serving("round", file.clone());
    let calls = Arc::clone(&resolver.calls);
    let mut heard: Vec<f32> = Vec::new();
    let mut failures: Vec<velo::audio::output::TrackFailure> = Vec::new();
    drain_headless_resolving(
        queue,
        0,
        0,
        SPEC,
        velo::config::ReplayGainMode::default(),
        Some(Arc::new(resolver)),
        &std::sync::atomic::AtomicBool::new(false),
        &mut |block| heard.extend_from_slice(block),
        &mut |failure| failures.push(failure),
    )
    .expect("the sequence drains");

    assert_eq!(
        calls.load(Ordering::Relaxed),
        1,
        "one resolution, at the start"
    );
    assert_eq!(heard, as_f32(&pcm), "the entry played the file it named");
    assert!(failures.is_empty(), "nothing was skipped: {failures:?}");
}
