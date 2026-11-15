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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use symphonia::core::io::MediaSource;
use velo::audio::output::{Entry, PathResolver, TrackFailure, drain_headless_with_carried_source};
use velo::audio::{AudioError, TrackSource, TrackSpec};
use velo::config::ReplayGainMode;
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

/// A fake chain whose downloader records its own argv, one argument per
/// line, beside its run count: the arguments are the assertion, and a seek
/// is told by its argv before anything else about it can be true.
fn recording_chain(dir: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let pipes = dir.join("pipes");
    let argv = dir.join("argv");
    let yt = fake(
        dir,
        "yt-dlp",
        &format!(
            "#!/bin/sh\necho run >> {pipes}\n\
             for a in \"$@\"; do printf '%s\\n' \"$a\" >> {argv}; done\n\
             cat {{fixture}}\n",
            pipes = sh_quote(&pipes.display().to_string()),
            argv = sh_quote(&argv.display().to_string()),
        )
        .replace("{fixture}", &sh_quote(&fixture().display().to_string())),
    );
    let ff = fake(dir, "ffmpeg", "#!/bin/sh\ncat\n");
    (yt, ff, pipes, argv)
}

/// The argv the fake downloader recorded, one argument per line.
fn recorded_argv(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .expect("the fake downloader recorded its argv")
        .lines()
        .map(String::from)
        .collect()
}

/// A resolver that implements only what the trait asks of every resolver:
/// its [`PathResolver::open_streaming_at`] is the trait's default — the
/// shape every composed resolver without an override runs.
struct Delegating(Resolver);

impl PathResolver for Delegating {
    fn resolve(&self, path: &Path) -> Result<PathBuf, AudioError> {
        self.0.resolve(path)
    }

    fn open_streaming(&self, path: &Path) -> Result<Option<Box<dyn MediaSource>>, AudioError> {
        self.0.open_streaming(path)
    }
}

/// A shell word, single-quoted: nothing a path or a JSON line carries can
/// break out of it except a quote itself, and none of these contain one.
fn sh_quote(word: &str) -> String {
    format!("'{word}'")
}

/// The seek fixtures' downloader: it honours `--download-sections
/// "*<sec>-"` the way the real tool is measured to — the stream starts at
/// that second — and records its argv, so both the ask and its answer are
/// assertions.
const SECTION_DOWNLOADER: &str = r#"#!/bin/sh
sec=0
prev=
for a in "$@"; do
  if [ "$prev" = "--download-sections" ]; then
    sec=${a#'*'}
    sec=${sec%-}
  fi
  prev=$a
done
for a in "$@"; do printf '%s\n' "$a" >> {argv}; done
if [ "$sec" -gt 0 ]; then
  tail -c +$((sec * 88200 + 1)) {pcm}
else
  cat {pcm}
fi
"#;

/// The seek fixtures' transcoder: a WAV header ahead of the body it passes
/// through, so what the chain delivers decodes at an exact sample rate and
/// every second names itself.
const PASSTHROUGH_TRANSCODER: &str = "#!/bin/sh\ncat {header} -\n";

/// Writes `seconds` of 44.1 kHz mono PCM16 where second `k` is one constant
/// sample value, `(k + 1) * 3000`: what decodes names the second it came
/// from, so "the stream starts at the offset" is a sample-level fact, never
/// a wording.
fn write_pcm(dir: &Path, seconds: u32) -> (PathBuf, Vec<i16>) {
    let mut bytes = Vec::new();
    let mut values = Vec::new();
    for second in 0..seconds {
        let value = i16::try_from((second + 1) * 3_000).expect("a small constant");
        for _ in 0..44_100 {
            bytes.extend_from_slice(&value.to_le_bytes());
            values.push(value);
        }
    }
    let path = dir.join("whole.pcm");
    std::fs::write(&path, &bytes).expect("the pcm body is written");
    (path, values)
}

/// Writes the 44-byte canonical WAV header for `frames` frames of 44.1 kHz
/// mono PCM16: what the fake transcoder writes ahead of the body it passes
/// through, so the chain's stream is a WAV the decoder reads whole.
fn write_wav_header(dir: &Path, frames: u32) -> PathBuf {
    let data_len = frames * 2;
    let mut header = Vec::with_capacity(44);
    header.extend_from_slice(b"RIFF");
    header.extend_from_slice(&(36 + data_len).to_le_bytes());
    header.extend_from_slice(b"WAVE");
    header.extend_from_slice(b"fmt ");
    header.extend_from_slice(&16u32.to_le_bytes());
    header.extend_from_slice(&1u16.to_le_bytes()); // PCM
    header.extend_from_slice(&1u16.to_le_bytes()); // mono
    header.extend_from_slice(&44_100u32.to_le_bytes());
    header.extend_from_slice(&88_200u32.to_le_bytes());
    header.extend_from_slice(&2u16.to_le_bytes());
    header.extend_from_slice(&16u16.to_le_bytes());
    header.extend_from_slice(b"data");
    header.extend_from_slice(&data_len.to_le_bytes());
    let path = dir.join("header.wav");
    std::fs::write(&path, &header).expect("the wav header is written");
    path
}

/// The seek fixtures' chain: a downloader that honours the section flag and
/// records its argv, and a transcoder that fronts the body with a WAV
/// header. Both programs are fakes; the chain wiring is real.
fn section_chain(dir: &Path) -> (Resolver, PathBuf, Vec<i16>) {
    let (pcm, values) = write_pcm(dir, 4);
    let header = write_wav_header(dir, 2 * 44_100);
    let argv = dir.join("argv");
    let yt = fake(
        dir,
        "yt-dlp",
        &SECTION_DOWNLOADER
            .replace("{pcm}", &sh_quote(&pcm.display().to_string()))
            .replace("{argv}", &sh_quote(&argv.display().to_string())),
    );
    let ff = fake(
        dir,
        "ffmpeg",
        &PASSTHROUGH_TRANSCODER.replace("{header}", &sh_quote(&header.display().to_string())),
    );
    (Resolver::new(yt, ff), argv, values)
}

/// The samples `source` decodes, whole.
fn decoded(source: Box<dyn MediaSource>) -> Vec<f32> {
    let mut track = TrackSource::open_remote(source).expect("the chain decodes");
    let mut samples = Vec::new();
    while let Some(block) = track.read_block().expect("the chain decodes") {
        samples.extend_from_slice(&block);
    }
    samples
}

/// The samples the pcm's seconds `from..` decode to: the reference the
/// offset stream is held to, sample for sample.
fn from_second(values: &[i16], from: u32) -> Vec<f32> {
    values[usize::try_from(from).expect("small") * 44_100..]
        .iter()
        .map(|value| f32::from(*value) / 32_768.0)
        .collect()
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

/// The seek is a fresh chain started at the offset (S4): its argv carries
/// `--download-sections` with the whole second asked for, the watch URL
/// stays the last argument, one invocation spawns, and the chain's stream
/// decodes.
#[test]
fn a_seek_starts_a_fresh_chain_whose_argv_carries_the_offset_and_whose_stream_decodes() {
    let dir = scratch("seek-chain");
    let (yt, ff, pipes, argv) = recording_chain(&dir);
    let resolver = Resolver::new(yt, ff);

    let source = resolver
        .open_streaming_at(&pseudo_path("abc"), Duration::from_secs(83))
        .expect("the seek's resolve answers")
        .expect("a source");

    let downloader_argv = recorded_argv(&argv);
    let flag = downloader_argv
        .iter()
        .position(|argument| argument == "--download-sections")
        .unwrap_or_else(|| {
            panic!("the seek's chain asks the tool for the offset: {downloader_argv:?}")
        });
    assert_eq!(
        downloader_argv.get(flag + 1).map(String::as_str),
        Some("*83-"),
        "the second asked for, whole seconds"
    );
    assert_eq!(
        downloader_argv.last().map(String::as_str),
        Some("https://www.youtube.com/watch?v=abc"),
        "the watch URL stays the last argument"
    );
    assert_eq!(pipe_runs(&pipes), 1, "one invocation: the seek's own chain");

    let mut track = TrackSource::open_remote(source).expect("the chain decodes");
    let mut streamed = Vec::new();
    while let Some(block) = track.read_block().expect("the chain decodes") {
        streamed.extend_from_slice(&block);
    }
    assert_eq!(
        streamed,
        reference(),
        "the fresh chain's stream decodes — the seek's audio, from the offset on"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Offset zero is the ordinary play, argv for argv: no
/// `--download-sections` at all, and the recorded argv is the public
/// [`resolve::arguments`] list byte for byte.
#[test]
fn an_offset_of_zero_adds_no_download_sections_at_all() {
    let dir = scratch("seek-zero-argv");
    let (yt, ff, _pipes, argv) = recording_chain(&dir);
    let resolver = Resolver::new(yt, ff);

    resolver
        .open_streaming(&pseudo_path("abc"))
        .expect("the ordinary play answers")
        .expect("a source");
    let ordinary = recorded_argv(&argv);

    std::fs::remove_file(&argv).expect("the first record is cleared");
    resolver
        .open_streaming_at(&pseudo_path("abc"), Duration::ZERO)
        .expect("the seek form at zero answers")
        .expect("a source");
    let zeroed = recorded_argv(&argv);

    assert_eq!(zeroed, ordinary, "offset zero is the ordinary play");
    assert!(
        !ordinary
            .iter()
            .any(|argument| argument == "--download-sections"),
        "the ordinary play never carries the flag: {ordinary:?}"
    );
    assert_eq!(
        ordinary,
        resolve::arguments("abc")
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<String>>(),
        "the recorded argv is the public arguments list, byte for byte"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The trait's default is what a composed resolver without an override
/// runs: it delegates to [`PathResolver::open_streaming`] — and that is
/// honest only when the source it answers has a position to put the offset
/// in. The chain's pipe has none: asked to start at an offset, the default
/// refuses with a sentence instead of handing over a stream that would play
/// from its start while the clock said the offset (issue #70, R3-002). At
/// offset zero the ask is the plain open, unchanged — no flag, one chain,
/// the delegate's own stream from its start. The resolver that honours the
/// offset says so by overriding, as [`Resolver`] does.
#[test]
fn the_default_open_streaming_at_refuses_the_seek_the_pipe_cannot_honour() {
    let dir = scratch("default-open-at");
    let (yt, ff, pipes, argv) = recording_chain(&dir);
    let composed = Delegating(Resolver::new(yt, ff));

    let error = composed
        .open_streaming_at(&pseudo_path("abc"), Duration::from_secs(90))
        .err()
        .expect("a seek the pipe cannot honour is refused");
    assert!(
        error.to_string().contains("cannot be seeked"),
        "the sentence names the reason: {error}"
    );
    // The ask opened the pipe it then refused: one chain, ended by the drop.
    assert_eq!(pipe_runs(&pipes), 1, "the ask opened, saw, and refused");

    // Offset zero is the ordinary play through the same default: the
    // delegate ran, no flag, argv as open_streaming's.
    let source = composed
        .open_streaming_at(&pseudo_path("abc"), Duration::ZERO)
        .expect("the default answers")
        .expect("a source, exactly as open_streaming answers");

    let downloader_argv = recorded_argv(&argv);
    assert!(
        !downloader_argv
            .iter()
            .any(|argument| argument == "--download-sections"),
        "the default at zero is the plain open: no flag, argv as open_streaming's, got {downloader_argv:?}"
    );
    assert_eq!(pipe_runs(&pipes), 2, "the delegate ran: the chain spawned");

    let mut track = TrackSource::open_remote(source).expect("the chain decodes");
    let mut streamed = Vec::new();
    while let Some(block) = track.read_block().expect("the chain decodes") {
        streamed.extend_from_slice(&block);
    }
    assert_eq!(
        streamed,
        reference(),
        "the delegate's own stream, from its start"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The seek's answer is audio, not wording: the fake downloader honours
/// the section flag the way the real tool is measured to, so the chain's
/// stream is the offset's own audio — the second asked for, never the
/// start — and its argv asked for exactly that second.
#[test]
fn a_seeked_chain_hands_the_offset_stream_not_the_start() {
    let dir = scratch("seek-stream-offset");
    let (resolver, argv, values) = section_chain(&dir);

    let source = resolver
        .open_streaming_at(&pseudo_path("abc"), Duration::from_secs(2))
        .expect("the seek's resolve answers")
        .expect("a source");

    let downloader_argv = recorded_argv(&argv);
    assert!(
        downloader_argv
            .windows(2)
            .any(|pair| pair[0] == "--download-sections" && pair[1] == "*2-"),
        "the chain asked the tool for the second asked for: {downloader_argv:?}"
    );
    assert_eq!(
        decoded(source),
        from_second(&values, 2),
        "the chain's stream is the offset's own audio — the second asked for, never the start"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The seek's landing, pinned at the sample level: the rebuild's carried
/// stream is the offset's own, and the producer plays it as handed over —
/// no silence (the pipe cannot be re-seeked) and no second zero (reopened
/// at the start). The argv and the carried source together are what make
/// "did not restart at zero" an observable fact, not a hope.
#[test]
fn a_seek_rebuild_plays_the_carried_offset_stream_without_reseeking_it() {
    let dir = scratch("seek-rebuild-samples");
    let (resolver, _argv, values) = section_chain(&dir);

    let source = resolver
        .open_streaming_at(&pseudo_path("abc"), Duration::from_secs(2))
        .expect("the seek's resolve answers")
        .expect("a source");

    // The rebuild at 2 s, the way the interface hands it over: the carried
    // source, the offset riding with it for the clock.
    let stop = AtomicBool::new(false);
    let mut samples = Vec::new();
    drain_headless_with_carried_source(
        Arc::new(Mutex::new(vec![Entry::Youtube {
            path: pseudo_path("abc"),
            title: String::from("Dawn"),
            uploader: None,
            duration: Some(4),
        }])),
        0,
        2 * u64::from(44_100u32),
        TrackSpec {
            sample_rate: 44_100,
            channels: 1,
        },
        ReplayGainMode::default(),
        source,
        &stop,
        &mut |block: &[f32]| samples.extend_from_slice(block),
        &mut |_failure: TrackFailure| {},
    )
    .expect("the sequence runs");

    assert_eq!(
        samples,
        from_second(&values, 2),
        "the seek's audio is the offset's own: silence would be the pipe re-seeked, second zero \
         would be a reopen at the start"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A seek must not leak the chain it replaces: the source being replaced is
/// dropped, and dropping it kills the group — downloader, transcoder and
/// anything they started. The stop a seek's rebuild performs is pinned here
/// as the producer's own turn ending over a chain whose children still run;
/// the drop itself is pinned by `dropping_the_source_ends_both_children_and
/// _the_grandchild`.
#[test]
fn the_chain_a_seek_replaces_is_gone_when_its_source_is_dropped() {
    let dir = scratch("seek-replaces-chain");
    let (pcm, _values) = write_pcm(&dir, 4);
    let header = write_wav_header(&dir, 4 * 44_100);
    let yt = fake(
        &dir,
        "yt-dlp",
        &"#!/bin/sh\nd=$(dirname \"$0\")\nprintf '%s' \"$$\" > \"$d/pid\"\n\
          sh -c 'sleep 60' & printf '%s' \"$!\" > \"$d/grand\"\ncat {pcm}\nsleep 60\n"
            .replace("{pcm}", &sh_quote(&pcm.display().to_string())),
    );
    let ff = fake(
        &dir,
        "ffmpeg",
        &"#!/bin/sh\nprintf '%s' \"$$\" > \"$(dirname \"$0\")/ffpid\"\ncat {header} -\n"
            .replace("{header}", &sh_quote(&header.display().to_string())),
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

    // The turn ends — a seek's stop is this, at the producer's own level —
    // and the source it played is dropped with it.
    let stop = AtomicBool::new(false);
    let mut samples = Vec::new();
    drain_headless_with_carried_source(
        Arc::new(Mutex::new(vec![Entry::Youtube {
            path: pseudo_path("abc"),
            title: String::from("Dawn"),
            uploader: None,
            duration: Some(4),
        }])),
        0,
        0,
        TrackSpec {
            sample_rate: 44_100,
            channels: 1,
        },
        ReplayGainMode::default(),
        source,
        &stop,
        &mut |block: &[f32]| {
            samples.extend_from_slice(block);
            if samples.len() >= 44_100 {
                stop.store(true, Ordering::Relaxed);
            }
        },
        &mut |_failure: TrackFailure| {},
    )
    .expect("the sequence runs");

    assert!(!samples.is_empty(), "the stream played before the stop");
    wait_until_gone(yt_pid);
    wait_until_gone(grand_pid);
    wait_until_gone(ff_pid);
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
