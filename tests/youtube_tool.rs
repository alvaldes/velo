//! The `yt-dlp` seam (issue #70, S6): one external process per call, bounded by
//! a deadline it cannot outlive, off the event loop, and never a hang the
//! interface waits on — and, beside [`run`], the chain [`spawn`] the play
//! path reads through: `yt-dlp` piped into `ffmpeg`, both children in one
//! process group, whose drop ends the pair and anything they left behind.
//!
//! Every case here runs fake programs written into a scratch directory: no
//! test in this file reaches the network, and none of them needs `yt-dlp`
//! or `ffmpeg` installed.

#![cfg(unix)]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use velo::youtube::tool::{Deadline, PREBUFFER, Running, ToolError, run, spawn};

/// This case's scratch directory, recreated empty so a failed earlier run can
/// never satisfy a later assertion.
fn scratch(case: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("velo-youtube-{case}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the scratch directory creates");
    dir
}

/// A fake `yt-dlp`: executable, and small enough to read at the call site.
fn fake(dir: &Path, body: &str) -> PathBuf {
    fake_program(dir, "yt-dlp", body)
}

/// A fake `ffmpeg`: the chain's second stage, written the same way.
fn fake_ffmpeg(dir: &Path, body: &str) -> PathBuf {
    fake_program(dir, "ffmpeg", body)
}

/// A fake program with the given name: executable, and small enough to read
/// at the call site.
fn fake_program(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    std::fs::create_dir_all(dir).expect("the case's directory creates");
    let path = dir.join(name);
    std::fs::write(&path, body).expect("the fake program is written");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("the fake program turns executable");
    path
}

/// Whether `pid` is gone: a kill that finds nothing answers `ESRCH`.
fn gone(pid: i32) -> bool {
    let Some(pid) = rustix::process::Pid::from_raw(pid) else {
        return true;
    };
    rustix::process::kill_process(pid, rustix::process::Signal::Kill).is_err()
}

/// Waits for the fake program to write a small file, bounded: a spawn hands
/// the stream over while the child still runs, so a test that reads a file
/// the child writes must allow the child a moment to write it.
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

#[test]
fn what_the_program_prints_is_what_comes_back() {
    let dir = scratch("stdout");
    let program = fake(
        &dir,
        "#!/bin/sh\nprintf 'https://example.invalid/a.m4a\\n'\n",
    );

    let out = run(&program, &[], Deadline::new(Duration::from_secs(5)))
        .expect("the program answers in time");

    assert_eq!(out.trim(), "https://example.invalid/a.m4a");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn arguments_are_passed_as_arguments_and_never_as_a_shell_line() {
    let dir = scratch("argv");
    // Prints how many arguments it got and each of them verbatim, so a query
    // that a shell had re-split would come back in pieces.
    let program = fake(&dir, "#!/bin/sh\nprintf '%s\\n' \"$#\" \"$@\"\n");
    let query = "ytsearch10:AC/DC; rm -rf ~";

    let out = run(
        &program,
        &[
            "ytsearch10:AC/DC; rm -rf ~".into(),
            "a b".into(),
            "--".into(),
        ],
        Deadline::new(Duration::from_secs(5)),
    )
    .expect("the program answers in time");

    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines, vec!["3", query, "a b", "--"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failure_names_its_own_reason_and_not_a_wall_of_detail() {
    let dir = scratch("failed");
    let program = fake(
        &dir,
        "#!/bin/sh\nprintf 'ERROR: [youtube] abc: Video unavailable\\n' >&2\n\
         printf 'a second line nobody reads\\n' >&2\nexit 1\n",
    );

    match run(&program, &[], Deadline::new(Duration::from_secs(5))) {
        Err(ToolError::Failed { line, .. }) => {
            assert!(
                line.contains("Video unavailable"),
                "the reason is quoted: {line}"
            );
            assert!(
                !line.contains("second line"),
                "only the first line is quoted: {line}"
            );
        }
        other => panic!("expected a failure naming its reason, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_child_that_outlives_its_deadline_is_killed() {
    let dir = scratch("deadline");
    // Ignores TERM on purpose: only a SIGKILL ends it, so the assertion below
    // fails if the runner asks politely instead of killing.
    let program = fake(
        &dir,
        "#!/bin/sh\ntrap '' TERM\nprintf '%s' \"$$\" > \"$(dirname \"$0\")/pid\"\nsleep 60\n",
    );

    let started = Instant::now();
    let result = run(&program, &[], Deadline::new(Duration::from_secs(1)));
    let elapsed = started.elapsed();

    assert!(
        matches!(result, Err(ToolError::Deadline { .. })),
        "expected the deadline, got {result:?}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "the deadline took {elapsed:?}"
    );
    let pid: i32 = std::fs::read_to_string(dir.join("pid"))
        .expect("the program reports its pid")
        .trim()
        .parse()
        .expect("the pid is a number");
    assert!(gone(pid), "the child outlived its deadline (pid {pid})");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_grandchild_holding_the_pipe_cannot_hold_the_call() {
    let dir = scratch("grandchild");
    // `yt-dlp` runs ffmpeg like this: the parent exits, the grandchild keeps
    // stdout open, and a caller that only waits for the status never returns.
    let program = fake(&dir, "#!/bin/sh\nsh -c 'sleep 60' &\nexit 0\n");

    let started = Instant::now();
    let result = run(&program, &[], Deadline::new(Duration::from_secs(1)));
    let elapsed = started.elapsed();

    assert!(
        matches!(result, Err(ToolError::Deadline { .. })),
        "expected the deadline, got {result:?}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "the grandchild held the call for {elapsed:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_program_that_is_not_there_says_how_to_get_it() {
    let dir = scratch("missing");
    let missing = dir.join("yt-dlp");

    let result = run(&missing, &[], Deadline::new(Duration::from_secs(1)));
    match &result {
        Err(error @ ToolError::NotInstalled(path)) => {
            assert_eq!(path, &missing);
            assert!(
                error.to_string().contains("not installed"),
                "the sentence says it is missing: {error}"
            );
        }
        other => panic!("expected NotInstalled, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- the chain spawn (issue #70, S4 as revised: the tool holds the stream,
// and ffmpeg decodes whatever it is) ----

/// The spawn call every chain case makes: both fakes' stdout wired as the
/// real chain wires them — the downloader's stdout is the transcoder's
/// stdin, because no pipe of velo's sits in the middle.
fn spawn_chain(yt: &Path, ff: &Path, deadline: Deadline) -> Result<Running, ToolError> {
    spawn(yt, &[], ff, &[], deadline)
}

/// The first byte of a spawned stream, read with a bound so a chain that
/// never writes fails the test instead of hanging it.
fn first_byte(running: &mut Running) -> u8 {
    let until = Instant::now() + Duration::from_secs(2);
    let mut byte = [0u8; 1];
    loop {
        match running.read(&mut byte) {
            Ok(1) => return byte[0],
            Ok(_) => panic!("the stream closed before a byte"),
            Err(_) => {}
        }
        assert!(Instant::now() < until, "the stream never wrote a byte");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn spawn_hands_back_bytes_while_the_children_still_run() {
    let dir = scratch("spawn-stream");
    // The downloader writes more than the pre-buffer's floor and then holds
    // the pipe open — still running when the stream is handed over; the
    // transcoder passes its stdin through, so the fill completes from the
    // downloader's bytes alone. spawn hands the stream over, it does not
    // wait for the children's exit.
    let yt = fake(&dir, "#!/bin/sh\nhead -c 300000 /dev/zero\nsleep 5\n");
    let ff = fake_ffmpeg(&dir, "#!/bin/sh\ncat\n");

    let started = Instant::now();
    let mut running =
        spawn_chain(&yt, &ff, Deadline::new(Duration::from_secs(5))).expect("the chain spawns");
    let handed_over = started.elapsed();
    let first = first_byte(&mut running);

    let _ = first;
    assert!(
        handed_over < Duration::from_secs(3),
        "spawn handed the stream over in {handed_over:?}, before the children's exit"
    );
    drop(running);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_stream_shorter_than_the_fill_hands_over_everything_it_had() {
    let dir = scratch("spawn-short");
    let yt = fake(&dir, "#!/bin/sh\nprintf 'whole-stream'\n");
    let ff = fake_ffmpeg(&dir, "#!/bin/sh\ncat\n");

    let mut running = spawn_chain(&yt, &ff, Deadline::new(Duration::from_secs(5)))
        .expect("the chain's stream ends short of the fill, and that is a stream");
    let mut all = Vec::new();
    running.read_to_end(&mut all).expect("the stream reads");

    assert_eq!(
        all,
        b"whole-stream".to_vec(),
        "nothing was lost to the fill"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_downloader_that_dies_before_a_byte_names_its_own_reason() {
    let dir = scratch("spawn-yt-failed");
    let yt = fake(
        &dir,
        "#!/bin/sh\nprintf 'ERROR: [youtube] abc: Video unavailable\\n' >&2\nexit 1\n",
    );
    let ff = fake_ffmpeg(&dir, "#!/bin/sh\ncat\n");

    match spawn_chain(&yt, &ff, Deadline::new(Duration::from_secs(5))) {
        Err(ToolError::Failed {
            program,
            line,
            status,
        }) => {
            assert_eq!(program, "yt-dlp", "the failing tool is named");
            assert_eq!(status, Some(1));
            assert!(
                line.contains("Video unavailable"),
                "the tool's own reason is the error, never a decoder error: {line}"
            );
        }
        other => panic!("expected the tool's own failure, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_transcoder_that_dies_before_a_byte_names_its_own_reason() {
    let dir = scratch("spawn-ff-failed");
    // The downloader succeeds; the transcoder refuses its stdin — the
    // sentence is ffmpeg's own, not the downloader's.
    let yt = fake(&dir, "#!/bin/sh\nprintf 'tool-bytes'\n");
    let ff = fake_ffmpeg(
        &dir,
        "#!/bin/sh\nprintf 'ERROR: ffmpeg broke\\n' >&2\nexit 2\n",
    );

    match spawn_chain(&yt, &ff, Deadline::new(Duration::from_secs(5))) {
        Err(ToolError::Failed {
            program,
            line,
            status,
        }) => {
            assert_eq!(program, "ffmpeg", "the failing stage is named");
            assert_eq!(status, Some(2));
            assert!(
                line.contains("ffmpeg broke"),
                "the transcoder's own reason is the error: {line}"
            );
        }
        other => panic!("expected the transcoder's own failure, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_chain_that_cannot_start_is_not_installed_when_the_downloader_is_missing() {
    let dir = scratch("spawn-missing");
    let missing = dir.join("yt-dlp");
    let ff = fake_ffmpeg(&dir, "#!/bin/sh\ncat\n");

    match spawn_chain(&missing, &ff, Deadline::new(Duration::from_secs(1))) {
        Err(ref error @ ToolError::NotInstalled(ref path)) => {
            assert_eq!(path, &missing);
            assert!(
                error.to_string().contains("yt-dlp is not installed"),
                "the sentence says what is missing: {error}"
            );
        }
        other => panic!("expected NotInstalled, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_missing_transcoder_says_how_to_install_ffmpeg_not_the_downloader() {
    let dir = scratch("spawn-missing-ff");
    // The machine has the downloader; the sentence must not send it back to
    // install what it already has.
    let yt = fake(&dir, "#!/bin/sh\ncat /dev/null\n");
    let missing = dir.join("ffmpeg");

    match spawn_chain(&yt, &missing, Deadline::new(Duration::from_secs(1))) {
        Err(ref error @ ToolError::NotInstalled(ref path)) => {
            assert_eq!(path, &missing);
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
        other => panic!("expected NotInstalled, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_chain_that_stalls_names_the_stall_and_ends_the_children() {
    let dir = scratch("spawn-stalled");
    // The downloader holds the pipe open and writes nothing; the transcoder
    // waits on its stdin. No byte, no end: the fill misses its bound.
    let yt = fake(
        &dir,
        "#!/bin/sh\nprintf '%s' \"$$\" > \"$(dirname \"$0\")/pid\"\nsleep 60\n",
    );
    let ff = fake_ffmpeg(&dir, "#!/bin/sh\ncat\n");

    let started = Instant::now();
    let result = spawn_chain(&yt, &ff, Deadline::new(Duration::from_secs(1)));
    let elapsed = started.elapsed();

    match &result {
        Err(error @ ToolError::Stalled { program, .. }) => {
            assert_eq!(program, "ffmpeg", "the bytes awaited are the transcoder's");
            assert!(
                error
                    .to_string()
                    .contains("did not deliver its first 256 KiB"),
                "the miss is named for what it is, never an empty track: {error}"
            );
        }
        other => panic!("expected the stall, got {other:?}"),
    }
    assert!(
        elapsed < Duration::from_secs(5),
        "the bound took {elapsed:?}"
    );
    let pid: i32 = std::fs::read_to_string(dir.join("pid"))
        .expect("the downloader reports its pid")
        .trim()
        .parse()
        .expect("the pid is a number");
    assert!(gone(pid), "the stalled chain was killed (pid {pid})");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dropping_the_stream_kills_both_children_and_the_grandchild() {
    let dir = scratch("spawn-drop");
    // The downloader ignores TERM — only a SIGKILL ends it — holds a
    // grandchild beside it, and writes more than the pre-buffer's floor so
    // the stream is handed over while everything still runs. The
    // transcoder records its own pid and then blocks on the downloader's
    // open pipe.
    let yt = fake(
        &dir,
        "#!/bin/sh\ntrap '' TERM\nd=$(dirname \"$0\")\nprintf '%s' \"$$\" > \"$d/pid\"\nsh -c 'sleep 60' & printf '%s' \"$!\" > \"$d/grand\"\nhead -c 300000 /dev/zero\nsleep 60\n",
    );
    let ff = fake_ffmpeg(
        &dir,
        "#!/bin/sh\nprintf '%s' \"$$\" > \"$(dirname \"$0\")/ffpid\"\ncat\n",
    );

    let running =
        spawn_chain(&yt, &ff, Deadline::new(Duration::from_secs(5))).expect("the chain spawns");
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
         stream is open"
    );

    drop(running);
    wait_until_gone(yt_pid);
    wait_until_gone(grand_pid);
    wait_until_gone(ff_pid);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The pre-buffer's number is the code's own: the constant the fill reads
/// up to, so the sentence and the floor never drift apart.
#[test]
fn the_prebuffer_floor_is_256_kib() {
    assert_eq!(PREBUFFER, 256 * 1024);
}

/// A slow start that keeps flowing is not a stall: a stream slower than the
/// pre-buffer hands over what it has when its bound passes, so a link slower
/// than the fill still plays.
///
/// Not covered here, and named so nobody assumes it: a stream that produces a
/// few bytes and then blocks is still refused, because the reader thread is
/// blocked in `read` and cannot check the bound. Bounding that needs a
/// non-blocking pipe; it is a follow-up, not a claim of this test.
#[test]
fn a_stream_that_flows_slower_than_the_prebuffer_is_handed_over_with_what_it_has() {
    let dir = scratch("slow-start");
    // A byte every 100 ms: flowing, and nowhere near 256 KiB in 400 ms.
    let yt = fake(
        &dir,
        "#!/bin/sh
while :; do printf 'a'; sleep 0.1; done
",
    );
    let ff = fake_ffmpeg(
        &dir,
        "#!/bin/sh
cat
",
    );

    let mut running = spawn_chain(&yt, &ff, Deadline::new(Duration::from_secs(1)))
        .expect("a slow start that flows is not a refusal");

    let mut seen = 0_usize;
    let until = Instant::now() + Duration::from_secs(2);
    let mut byte = [0u8; 1];
    while seen < 2 && Instant::now() < until {
        if let Ok(1) = running.read(&mut byte) {
            seen += 1;
        }
    }
    assert!(seen >= 2, "the slow stream's bytes are the ones that play");

    drop(running);
    let _ = std::fs::remove_dir_all(&dir);
}
