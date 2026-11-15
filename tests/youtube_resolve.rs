//! The `youtube://` resolve (issue #70, S4 as revised by L9): the stream is
//! the tool's own pipe — `yt-dlp -x --audio-format mp3 -o -` — opened when
//! the entry plays, never a URL this tree fetches. The URL route this file
//! once pinned is deleted with the code it tested: measured over five
//! consecutive extractions, every decodable format answers `403` to a ranged
//! GET, so no transport test can survive and none should.
//!
//! Every case here runs a fake program written into a scratch directory and
//! streams a real fixture: no test in this file reaches the network, and
//! none of them needs `yt-dlp` installed.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use velo::audio::TrackSource;
use velo::audio::output::PathResolver;
use velo::youtube::pseudo_path;
use velo::youtube::resolve::{self, LIVE_SENTENCE, Resolver};

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

    std::fs::create_dir_all(dir).expect("the case's directory creates");
    let path = dir.join("yt-dlp");
    std::fs::write(&path, body).expect("the fake program is written");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("the fake program turns executable");
    path
}

/// The MP3 fixture the fake tool streams: the same shape the real pipe has —
/// a sequential, decodable MP3 on stdout.
fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("silence-then-tone.mp3")
}

/// One extraction as the real tool answers it: a recorded video — its
/// `live_status` says so, whatever its `formats[]` look like.
const EXTRACTION: &str = r#"{"id":"abc","duration":212.0,"live_status":"not_live"}"#;

/// One extraction as a live broadcast answers it: `live_status` is `is_live`.
const LIVE_EXTRACTION: &str = r#"{"id":"abc","live_status":"is_live"}"#;

/// A fake tool shaped like the real one: the extraction argument answers
/// `answer`, everything else is the pipe — it counts its own runs in a file
/// beside it and then runs `pipe_body`, so a test can prove the pipe was (or
/// was never) spawned.
fn fake_tool(dir: &Path, answer: &str, pipe_body: &str) -> (PathBuf, PathBuf) {
    let pipes = dir.join("pipes");
    let body = format!(
        "#!/bin/sh\ncase \"$*\" in\n  *dump-single-json*)\n    printf '%s' {answer}\n    ;;\n  \
         *)\n    echo run >> {pipes}\n{pipe_body}\n    ;;\nesac\n",
        answer = sh_quote(answer),
        pipes = sh_quote(&pipes.display().to_string()),
    )
    .replace("{fixture}", &sh_quote(&fixture().display().to_string()));
    let program = fake(dir, &body);
    (program, pipes)
}

/// A shell word, single-quoted: nothing a path or a JSON line carries can
/// break out of it except a quote itself, and none of these contain one.
fn sh_quote(word: &str) -> String {
    format!("'{word}'")
}

/// How many times the fake pipe ran: a resolve that spawns a second pipe is
/// an ask the play never owed.
fn pipe_runs(pipes: &Path) -> usize {
    std::fs::read_to_string(pipes).map_or(0, |content| content.lines().count())
}

/// Whether `pid` is gone: a kill that finds nothing answers `ESRCH`.
fn gone(pid: i32) -> bool {
    let Some(pid) = rustix::process::Pid::from_raw(pid) else {
        return true;
    };
    rustix::process::kill_process(pid, rustix::process::Signal::Kill).is_err()
}

/// Waits for the fake program to write a small file, bounded.
fn wait_for_file(path: &Path) -> String {
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        if let Ok(content) = std::fs::read_to_string(path) {
            return content;
        }
        assert!(
            Instant::now() < until,
            "the file never appeared: {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Waits until `gone` holds for `pid`, bounded: a kill is not instantaneous,
/// and the assertion is about the end state, not about scheduling.
fn wait_until_gone(pid: i32) {
    let until = Instant::now() + Duration::from_secs(2);
    while !gone(pid) {
        assert!(Instant::now() < until, "pid {pid} never died");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The samples the fixture decodes as a local file — the reference the
/// streamed pipe is held to.
fn reference() -> Vec<f32> {
    let mut source = TrackSource::open(&fixture()).expect("the fixture opens as a file");
    let mut samples = Vec::new();
    while let Some(block) = source.read_block().expect("the reference decodes") {
        samples.extend_from_slice(&block);
    }
    samples
}

#[test]
fn a_youtube_row_is_never_cached_and_never_a_file() {
    let resolver = Resolver::new(PathBuf::from("/nowhere/yt-dlp"));
    let row = pseudo_path("dQw4w9WgXcQ");
    let file = PathBuf::from("/music/a.flac");

    assert!(
        !resolver.cached(&row),
        "a resolved stream never waits on a cache: never reachable"
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
fn the_pipe_asks_for_the_stream_the_tool_delivers_itself() {
    let args: Vec<String> = resolve::arguments("abc")
        .into_iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();

    assert_eq!(
        args.last().map(String::as_str),
        Some("https://www.youtube.com/watch?v=abc"),
        "the watch URL is the last argument, after the -- that ends the options"
    );
    assert!(args.contains(&"--no-playlist".to_owned()), "one video only");
    assert!(
        args.contains(&"-x".to_owned())
            && args.contains(&"--audio-format".to_owned())
            && args.contains(&"mp3".to_owned()),
        "the tool extracts and transcodes to mp3: the one route that works"
    );
    assert!(
        args.contains(&"-o".to_owned()) && args.contains(&"-".to_owned()),
        "the stream goes to stdout, where velo reads it"
    );
    assert!(
        !args.iter().any(|argument| argument.contains("get-url")),
        "the URL route is gone: no retry of a 403 fixes it"
    );
    assert!(
        !args
            .iter()
            .any(|argument| argument.contains("dump-single-json")),
        "the pipe is one call, and it answers bytes, not JSON"
    );
}

#[test]
fn the_extraction_that_refuses_a_live_takes_no_url_and_selects_no_format() {
    let args: Vec<String> = resolve::extraction_arguments("abc")
        .into_iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();

    assert_eq!(
        args.last().map(String::as_str),
        Some("https://www.youtube.com/watch?v=abc"),
        "the watch URL is the last argument"
    );
    assert!(args.contains(&"--no-playlist".to_owned()), "one video only");
    assert!(
        args.contains(&"--dump-single-json".to_owned()),
        "the full extraction, for the live verdict alone"
    );
    assert!(
        !args.contains(&"--format".to_owned()) && !args.contains(&"-x".to_owned()),
        "no URL is taken and no format is selected: the stream comes from the pipe"
    );
}

#[test]
fn an_open_streaming_decodes_the_fixture_through_the_pipe_and_is_not_seekable() {
    let dir = scratch("pipe-decodes");
    let (program, pipes) = fake_tool(&dir, EXTRACTION, "cat {fixture}");
    let resolver = Resolver::new(program);

    let source = resolver
        .open_streaming(&pseudo_path("abc"))
        .expect("the resolve answers")
        .expect("a source");
    assert!(!source.is_seekable(), "a pipe has no position to seek to");
    assert_eq!(source.byte_len(), None, "a pipe has no end to know");

    let mut track = TrackSource::open_remote(source).expect("the pipe decodes");
    let mut streamed = Vec::new();
    while let Some(block) = track.read_block().expect("the pipe decodes") {
        streamed.extend_from_slice(&block);
    }

    assert_eq!(
        streamed,
        reference(),
        "the tool's pipe decodes to exactly the fixture's own samples — the decoder takes a \
         non-seekable source"
    );
    assert_eq!(pipe_runs(&pipes), 1, "exactly one pipe, the resolve's own");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dropping_the_source_ends_the_child() {
    let dir = scratch("drop-ends-child");
    let (program, _pipes) = fake_tool(
        &dir,
        EXTRACTION,
        "printf 'stream'\nd=\"$(dirname \"$0\")\"\nprintf '%s' \"$$\" > \"$d/pid\"\nsleep 60",
    );
    let resolver = Resolver::new(program);

    let source = resolver
        .open_streaming(&pseudo_path("abc"))
        .expect("the resolve answers")
        .expect("a source");
    let pid: i32 = wait_for_file(&dir.join("pid"))
        .trim()
        .parse()
        .expect("the pipe reports its pid");
    assert!(!gone(pid), "the child runs while the source is open");

    drop(source);
    wait_until_gone(pid);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_pipe_that_dies_at_once_answers_the_tool_s_own_sentence_not_a_decode_error() {
    let dir = scratch("pipe-fails");
    let (program, pipes) = fake_tool(
        &dir,
        EXTRACTION,
        "printf 'ERROR: [youtube] abc: Video unavailable\\n' >&2\nexit 1",
    );
    let resolver = Resolver::new(program);

    let error = resolver
        .open_streaming(&pseudo_path("abc"))
        .err()
        .expect("nothing to play");

    assert!(
        error.to_string().contains("yt-dlp could not answer"),
        "the tool's own sentence, in the register of every failure: {error}"
    );
    assert!(
        error.to_string().contains("Video unavailable"),
        "the reason a user can act on survives the wrapper: {error}"
    );
    assert!(
        !error.to_string().contains("could not read"),
        "a stream that never starts is never a decoder error: {error}"
    );
    assert_eq!(pipe_runs(&pipes), 1, "the pipe is what failed");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_live_answer_is_refused_by_name_before_any_pipe_is_spawned() {
    let dir = scratch("live-refused");
    let (program, pipes) = fake_tool(&dir, LIVE_EXTRACTION, "cat {fixture}");
    let resolver = Resolver::new(program);

    let error = resolver
        .open_streaming(&pseudo_path("abc"))
        .err()
        .expect("a live broadcast is refused");

    assert_eq!(
        error.to_string(),
        LIVE_SENTENCE,
        "the refusal says what it is, in its own words: {error}"
    );
    assert_eq!(
        pipe_runs(&pipes),
        0,
        "the refusal happens before any pipe is spawned"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A recorded video is never called live, whatever its `formats[]` look
/// like: the extraction's only verdict is `live_status`, so a recorded
/// video whose formats the pipe cannot use fails with the pipe's own
/// reason — never with a sentence about a broadcast the video is not.
#[test]
fn a_recorded_video_is_never_called_live_whatever_its_formats_look_like() {
    for status in ["not_live", "was_live"] {
        let dir = scratch(&format!("recorded-{status}"));
        let extraction = format!(
            r#"{{"id":"abc","duration":212.0,"live_status":"{status}","formats":[{{"vcodec":"none","acodec":"mp4a.40.2","protocol":"m3u8_native"}}]}}"#
        );
        let (program, pipes) = fake_tool(
            &dir,
            &extraction,
            "printf 'ERROR: [youtube] abc: requested format not available\\n' >&2\nexit 1",
        );
        let resolver = Resolver::new(program);

        let error = resolver
            .open_streaming(&pseudo_path("abc"))
            .err()
            .expect("nothing to play");

        assert!(
            !error.to_string().contains("live") && !error.to_string().contains("HLS"),
            "a recorded video (`live_status: {status}`) is never called live: {error}"
        );
        assert!(
            error.to_string().contains("requested format not available"),
            "the pipe's own reason is the failure: {error}"
        );
        assert_eq!(
            pipe_runs(&pipes),
            1,
            "the recorded verdict let the pipe run"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn a_refusal_never_reads_as_a_file_read() {
    let dir = scratch("refusal-wording");
    let cases = [
        // The live/manifest refusal...
        (
            "live",
            format!(
                "#!/bin/sh\ncase \"$*\" in\n  *dump-single-json*)\n    printf '%s' {answer}\n    \
                 ;;\n  *) cat /dev/null ;;\nesac\n",
                answer = sh_quote(LIVE_EXTRACTION),
            ),
        ),
        // ...and the tool's own failure: both must not wear the prefix.
        (
            "failed",
            "#!/bin/sh\nprintf 'ERROR: Video unavailable\\n' >&2\nexit 1\n".to_owned(),
        ),
    ];
    for (case, body) in &cases {
        let program = fake(&dir.join(case), body);
        let error = Resolver::new(program)
            .open_streaming(&pseudo_path("abc"))
            .err()
            .expect("nothing to play");
        assert!(
            !error.to_string().starts_with("cannot read the file:"),
            "a resolve failure is a refusal, not a file read: {error}"
        );
    }
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
