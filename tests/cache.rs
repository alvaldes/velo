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

use velo::audio::output::{PathResolver, drain_headless_resolving};
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
}

impl FakeResolver {
    fn serving(id: &str, path: PathBuf) -> Self {
        Self {
            files: vec![(id.to_owned(), path)],
        }
    }
}

impl PathResolver for FakeResolver {
    fn resolve(&self, path: &Path) -> Result<PathBuf, AudioError> {
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
        PathBuf::from("subsonic://music.example.com/a"),
        PathBuf::from("subsonic://music.example.com/b"),
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
        PathBuf::from("subsonic://music.example.com/dead"),
        local.clone(),
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
