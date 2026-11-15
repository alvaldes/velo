//! The `yt-dlp` seam (issue #70, S6): one external process per call, bounded by
//! a deadline it cannot outlive, off the event loop, and never a hang the
//! interface waits on.
//!
//! Every case here runs a fake program written into a scratch directory: no
//! test in this file reaches the network, and none of them needs `yt-dlp`
//! installed.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use velo::audio::output::PathResolver;
use velo::audio::remote::{RangeAnswer, RangeTransport};
use velo::subsonic::Error;
use velo::youtube::pseudo_path;
use velo::youtube::resolve::{self, Resolver};

/// This case's scratch directory, recreated empty so a failed earlier run can
/// never satisfy a later assertion.
fn scratch(case: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("velo-youtube-{case}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the scratch directory creates");
    dir
}

/// A fake program: executable, and small enough to read at the call site.
fn fake(dir: &Path, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = dir.join("yt-dlp");
    std::fs::write(&path, body).expect("the fake program is written");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("the fake program turns executable");
    path
}

/// A transport that records every URL it was asked about and serves one small
/// ranged body, the way `tests/remote_stream.rs`'s fixture does — never a
/// network.
struct Seen {
    urls: Mutex<Vec<String>>,
}

impl RangeTransport for Seen {
    fn range(&self, url: &str, _from: u64, _to: Option<u64>) -> Result<RangeAnswer, Error> {
        self.urls
            .lock()
            .expect("the record unlocks")
            .push(url.to_owned());
        Ok(RangeAnswer::Partial {
            bytes: vec![0x00, 0x01, 0x02],
            total: 3,
        })
    }
}

#[test]
fn a_youtube_row_is_never_cached_and_never_a_file() {
    let resolver = Resolver::new(PathBuf::from("/nowhere/yt-dlp"));
    let row = pseudo_path("dQw4w9WgXcQ");
    let file = PathBuf::from("/music/a.flac");

    assert!(
        !resolver.cached(&row),
        "a resolved URL expires: never reachable"
    );
    assert!(resolver.cached(&file), "a local file is already reachable");
    assert_eq!(
        resolver.resolve(&file).expect("passes through"),
        file.clone()
    );
    let error = resolver.resolve(&row).expect_err("never a file");
    assert!(
        error.to_string().contains("never a file"),
        "the sentence says a YouTube entry is not one: {error}"
    );
}

#[test]
fn the_resolve_asks_for_one_url_in_a_format_the_decoder_can_read() {
    let args: Vec<String> = resolve::arguments("abc")
        .into_iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();

    assert_eq!(
        args.last().map(String::as_str),
        Some("https://www.youtube.com/watch?v=abc"),
        "the watch URL is the last argument"
    );
    assert!(args.contains(&"--no-playlist".to_owned()), "one video only");
    let format_at = args
        .iter()
        .position(|argument| argument == "--format")
        .expect("a format selector")
        + 1;
    assert_eq!(
        args[format_at], "bestaudio[ext=m4a]/bestaudio[ext=mp3]/bestaudio",
        "m4a first — the decoder has no Matroska demuxer"
    );
}

#[test]
fn the_stream_is_opened_with_the_url_the_tool_printed() {
    let dir = scratch("resolve");
    let program = fake(
        &dir,
        "#!/bin/sh\nprintf 'https://media.example/audio.m4a\\n'\n",
    );
    let seen = Arc::new(Seen {
        urls: Mutex::new(Vec::new()),
    });
    let resolver =
        Resolver::new(program).with_transport(Arc::clone(&seen) as Arc<dyn RangeTransport>);

    let source = resolver
        .open_streaming(&pseudo_path("abc"))
        .expect("the resolve answers")
        .expect("a ranged source");

    assert_eq!(
        seen.urls.lock().expect("the record unlocks").as_slice(),
        ["https://media.example/audio.m4a"],
        "the tool's line is what the transport was asked for"
    );
    drop(source);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failing_resolve_answers_the_tools_own_reason() {
    let dir = scratch("resolve-fail");
    let program = fake(
        &dir,
        "#!/bin/sh\nprintf 'ERROR: Video unavailable\\n' >&2\nexit 1\n",
    );
    let seen = Arc::new(Seen {
        urls: Mutex::new(Vec::new()),
    });
    let resolver =
        Resolver::new(program).with_transport(Arc::clone(&seen) as Arc<dyn RangeTransport>);

    let error = resolver
        .open_streaming(&pseudo_path("abc"))
        .err()
        .expect("nothing to play");

    assert!(
        error.to_string().contains("Video unavailable"),
        "the reason a user can act on survives the wrapper: {error}"
    );
    assert!(
        seen.urls.lock().expect("the record unlocks").is_empty(),
        "a failed resolve never reaches the transport"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_row_that_says_youtube_and_carries_no_id_is_refused_before_any_process() {
    // The program is nowhere: if the refusal came after the spawn it would
    // answer NotInstalled instead of the broken-row sentence.
    let resolver = Resolver::new(PathBuf::from("/nowhere/yt-dlp"));

    let error = resolver
        .open_streaming(&PathBuf::from("youtube://"))
        .err()
        .expect("a broken row");

    assert!(
        error.to_string().contains("is not a YouTube entry"),
        "the broken-row sentence, never NotInstalled: {error}"
    );
}
