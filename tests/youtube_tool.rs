//! The `yt-dlp` seam (issue #70, S6): one external process per call, bounded by
//! a deadline it cannot outlive, off the event loop, and never a hang the
//! interface waits on.
//!
//! Every case here runs a fake program written into a scratch directory: no
//! test in this file reaches the network, and none of them needs `yt-dlp`
//! installed.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use velo::youtube::tool::{Deadline, ToolError, run};

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

/// Whether `pid` is gone: a kill that finds nothing answers `ESRCH`.
fn gone(pid: i32) -> bool {
    let Some(pid) = rustix::process::Pid::from_raw(pid) else {
        return true;
    };
    rustix::process::kill_process(pid, rustix::process::Signal::Kill).is_err()
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
    let result = run(&program, &[], Deadline::new(Duration::from_millis(300)));
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
    let result = run(&program, &[], Deadline::new(Duration::from_millis(300)));
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
