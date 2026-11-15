//! The `youtube://` resolve (issue #70, S4 as revised by L9; live broadcasts
//! play, L10): the stream is the chain's own — `yt-dlp -f bestaudio -o -`
//! piped into `ffmpeg -i - -f mp3 -` — opened when the entry plays, never a
//! URL this tree fetches. The URL route this file once pinned is deleted
//! with the code it tested: measured over five consecutive extractions,
//! every decodable format answers `403` to a ranged GET, so no transport
//! test can survive and none should.
//!
//! Every case here runs fake programs written into a scratch directory and
//! streams a real fixture: the chain itself is exercised — the downloader's
//! stdout is wired to the transcoder's stdin, exactly as the real chain
//! wires it. No test in this file reaches the network, and none of them
//! needs `yt-dlp` or `ffmpeg` installed.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use velo::audio::TrackSource;
use velo::audio::output::PathResolver;
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
fn fake(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    std::fs::create_dir_all(dir).expect("the case's directory creates");
    let path = dir.join(name);
    std::fs::write(&path, body).expect("the fake program is written");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("the fake program turns executable");
    path
}

/// The MP3 fixture the fake chain streams: the same shape the real chain has
/// — a sequential, decodable MP3 on the transcoder's stdout.
fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("silence-then-tone.mp3")
}

/// A fake chain shaped like the real one: the downloader counts its own runs
/// in a file beside it, then runs its body; the transcoder runs its body —
/// `cat` passes its stdin through, which is how the chain itself is
/// exercised, stdin wired to the downloader's stdout, no pipe of velo's in
/// the middle.
fn fake_chain(dir: &Path, yt_body: &str, ff_body: &str) -> (PathBuf, PathBuf, PathBuf) {
    let pipes = dir.join("pipes");
    let yt = fake(
        dir,
        "yt-dlp",
        &format!(
            "#!/bin/sh\necho run >> {pipes}\n{yt_body}\n",
            pipes = sh_quote(&pipes.display().to_string()),
        )
        .replace("{fixture}", &sh_quote(&fixture().display().to_string())),
    );
    let ff = fake(dir, "ffmpeg", &format!("#!/bin/sh\n{ff_body}\n"));
    (yt, ff, pipes)
}

/// A shell word, single-quoted: nothing a path or a JSON line carries can
/// break out of it except a quote itself, and none of these contain one.
fn sh_quote(word: &str) -> String {
    format!("'{word}'")
}

/// How many times the fake downloader ran: a resolve that spawns a second
/// invocation is an ask the play never owed.
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
/// streamed chain is held to.
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
    let resolver = Resolver::new(
        PathBuf::from("/nowhere/yt-dlp"),
        PathBuf::from("/nowhere/ffmpeg"),
    );
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
fn the_chain_asks_for_the_stream_the_tool_delivers_itself() {
    let args: Vec<String> = resolve::arguments("abc")
        .into_iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();

    assert_eq!(
        args.last().map(String::as_str),
        Some("https://www.youtube.com/watch?v=abc"),
        "the watch URL is the last argument, after the options that end with the stream"
    );
    assert!(args.contains(&"--no-playlist".to_owned()), "one video only");
    assert!(
        args.contains(&"-f".to_owned()) && args.contains(&"bestaudio".to_owned()),
        "the tool streams the best audio it can get, whatever container it is in"
    );
    assert!(
        args.contains(&"-o".to_owned()) && args.contains(&"-".to_owned()),
        "the stream goes to stdout, where the transcoder reads it"
    );
    assert!(
        !args.iter().any(|argument| argument.contains("get-url")),
        "the URL route is gone: no retry of a 403 fixes it"
    );
    assert!(
        !args
            .iter()
            .any(|argument| argument.contains("dump-single-json")),
        "the chain is one downloader call, and it answers bytes, not JSON"
    );
    assert!(
        !args.contains(&"-x".to_owned()) && !args.contains(&"--audio-format".to_owned()),
        "no post-processing by the tool: a live broadcast cannot be post-processed, and the \
         transcoder does the transcode (L10)"
    );
}

#[test]
fn the_transcoder_transcodes_whatever_arrives_to_mp3_on_stdout() {
    let args: Vec<String> = resolve::ffmpeg_arguments()
        .into_iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();

    assert_eq!(
        args,
        vec![
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            "-",
            "-f",
            "mp3",
            "-",
        ],
        "the measured shape: stdin in, MP3 out, quiet except for the one line a failure quotes"
    );
}

#[test]
fn an_open_streaming_decodes_the_fixture_through_the_chain_and_is_not_seekable() {
    let dir = scratch("chain-decodes");
    let (yt, ff, pipes) = fake_chain(&dir, "cat {fixture}", "cat");
    let resolver = Resolver::new(yt, ff);

    let source = resolver
        .open_streaming(&pseudo_path("abc"))
        .expect("the resolve answers")
        .expect("a source");
    assert!(!source.is_seekable(), "a pipe has no position to seek to");
    assert_eq!(source.byte_len(), None, "a pipe has no end to know");

    let mut track = TrackSource::open_remote(source).expect("the chain decodes");
    let mut streamed = Vec::new();
    while let Some(block) = track.read_block().expect("the chain decodes") {
        streamed.extend_from_slice(&block);
    }

    assert_eq!(
        streamed,
        reference(),
        "the chain decodes to exactly the fixture's own samples — the downloader's stdout is \
         the transcoder's stdin, and the decoder takes a non-seekable source"
    );
    assert_eq!(
        pipe_runs(&pipes),
        1,
        "exactly one yt-dlp invocation: the play's own, no pre-flight extraction"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dropping_the_source_ends_both_children_and_the_grandchild() {
    let dir = scratch("drop-ends-chain");
    // The downloader ignores TERM — only a SIGKILL ends it — holds a
    // grandchild beside it, and writes more than the pre-buffer's floor so
    // the source is handed over while everything still runs. The
    // transcoder records its own pid and then blocks on the downloader's
    // open pipe.
    let (yt, ff, _pipes) = fake_chain(
        &dir,
        "trap '' TERM\nd=\"$(dirname \"$0\")\"\nprintf '%s' \"$$\" > \"$d/pid\"\n\
         sh -c 'sleep 60' & printf '%s' \"$!\" > \"$d/grand\"\nhead -c 300000 /dev/zero\nsleep 60",
        "printf '%s' \"$$\" > \"$(dirname \"$0\")/ffpid\"\ncat",
    );
    let resolver = Resolver::new(yt, ff);

    let source = resolver
        .open_streaming(&pseudo_path("abc"))
        .expect("the resolve answers")
        .expect("a source");
    let yt_pid: i32 = wait_for_file(&dir.join("pid"))
        .trim()
        .parse()
        .expect("the downloader reports its pid");
    let grand_pid: i32 = wait_for_file(&dir.join("grand"))
        .trim()
        .parse()
        .expect("the grandchild reports its pid");
    let ff_pid: i32 = wait_for_file(&dir.join("ffpid"))
        .trim()
        .parse()
        .expect("the transcoder reports its pid");
    assert!(
        !gone(yt_pid) && !gone(grand_pid) && !gone(ff_pid),
        "downloader {yt_pid}, grandchild {grand_pid} and transcoder {ff_pid} run while the \
         source is open"
    );

    drop(source);
    wait_until_gone(yt_pid);
    wait_until_gone(grand_pid);
    wait_until_gone(ff_pid);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_chain_that_dies_at_once_answers_the_tool_s_own_sentence_not_a_decode_error() {
    let dir = scratch("chain-fails");
    let (yt, ff, pipes) = fake_chain(
        &dir,
        "printf 'ERROR: [youtube] abc: Video unavailable\\n' >&2\nexit 1",
        "cat",
    );
    let resolver = Resolver::new(yt, ff);

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
    assert_eq!(pipe_runs(&pipes), 1, "the downloader is what failed");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_missing_transcoder_says_how_to_install_ffmpeg_not_the_downloader() {
    let dir = scratch("missing-transcoder");
    let (yt, _ff, _pipes) = fake_chain(&dir, "cat {fixture}", "cat");
    let resolver = Resolver::new(yt, PathBuf::from("/nowhere/ffmpeg"));

    let error = resolver
        .open_streaming(&pseudo_path("abc"))
        .err()
        .expect("nothing plays without the transcoder");

    let sentence = error.to_string();
    assert!(
        sentence.contains("ffmpeg is not installed"),
        "the missing tool is named: {sentence}"
    );
    assert!(
        !sentence.contains("yt-dlp is not installed"),
        "the tool the machine has is not named as missing: {sentence}"
    );
}

#[test]
fn a_refusal_never_reads_as_a_file_read() {
    let dir = scratch("refusal-wording");
    // The tool's own failure must not wear the "cannot read the file"
    // prefix a resolve refusal must never carry.
    let (yt, ff, _pipes) = fake_chain(
        &dir,
        "printf 'ERROR: Video unavailable\\n' >&2\nexit 1",
        "cat",
    );
    let error = Resolver::new(yt, ff)
        .open_streaming(&pseudo_path("abc"))
        .err()
        .expect("nothing to play");
    assert!(
        !error.to_string().starts_with("cannot read the file:"),
        "a resolve failure is a refusal, not a file read: {error}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_row_that_says_youtube_and_carries_no_id_is_refused_before_any_process() {
    // The programs are nowhere: if the refusal came after the spawn it would
    // answer NotInstalled instead of the broken-row sentence.
    let resolver = Resolver::new(
        PathBuf::from("/nowhere/yt-dlp"),
        PathBuf::from("/nowhere/ffmpeg"),
    );

    let error = resolver
        .open_streaming(&PathBuf::from("youtube://"))
        .err()
        .expect("a broken row");

    assert!(
        error.to_string().contains("is not a YouTube entry"),
        "the broken-row sentence, never NotInstalled: {error}"
    );
}
