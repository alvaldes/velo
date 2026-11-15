//! The `yt-dlp` seam (issue #70, S6): one external process per call, bounded by
//! a deadline it cannot outlive, off the event loop, and never a hang the
//! interface waits on.
//!
//! Every case here runs a fake program written into a scratch directory: no
//! test in this file reaches the network, and none of them needs `yt-dlp`
//! installed.

#![cfg(unix)]

use std::path::{Path, PathBuf};

use velo::youtube::search;

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

/// One flat-playlist line as `yt-dlp --dump-json` writes it: a subset of the
/// real object, with the fields velo reads and a few it must ignore.
fn line(id: &str, title: &str, extra: &str) -> String {
    format!(
        r#"{{"_type":"url","ie_key":"Youtube","id":"{id}","url":"https://example.invalid/{id}","title":"{title}","description":null{extra}}}"#
    )
}

#[test]
fn results_come_back_with_the_body_a_row_shows() {
    let out = format!(
        "{}\n{}\n",
        line("aaa", "First", r#","duration":213.0,"uploader":"Someone""#),
        line("bbb", "Second", r#","duration":7,"channel":"A channel""#),
    );

    let found = search::parse(&out);

    assert_eq!(found.len(), 2);
    assert_eq!(found[0].id, "aaa");
    assert_eq!(found[0].title, "First");
    assert_eq!(found[0].uploader.as_deref(), Some("Someone"));
    assert_eq!(found[0].duration, Some(213), "a float duration rounds down");
    assert_eq!(found[1].uploader.as_deref(), Some("A channel"));
    assert_eq!(found[1].duration, Some(7));
}

#[test]
fn a_broadcast_without_a_length_still_comes_back() {
    let out = line(
        "live",
        "Streaming now",
        r#","duration":null,"uploader":"Them""#,
    );

    let found = search::parse(&format!("{out}\n"));

    assert_eq!(found.len(), 1);
    assert_eq!(found[0].duration, None, "no length is said, never invented");
}

#[test]
fn a_broadcast_marked_live_comes_back_live_and_a_recorded_one_does_not() {
    let out = format!(
        "{}\n{}\n{}\n",
        line(
            "live1",
            "Streaming now",
            r#","duration":null,"live_status":"is_live","uploader":"Them""#
        ),
        line(
            "rec1",
            "Recorded",
            r#","duration":42,"live_status":"not_live""#
        ),
        line("rec2", "No such field", ""),
    );

    let found = search::parse(&out);

    assert_eq!(found.len(), 3);
    assert!(
        found[0].live,
        "`is_live` in the line's live_status means live"
    );
    assert_eq!(found[0].duration, None, "a live row never carries a length");
    assert!(
        !found[1].live,
        "`not_live` names a recorded video, and it keeps its length"
    );
    assert_eq!(found[1].duration, Some(42));
    assert!(!found[2].live, "a line without live_status is not live");
}

#[test]
fn a_line_nobody_can_use_is_skipped_and_the_rest_survive() {
    let out = format!(
        "{}\nnot json at all\n{}\n{}\n",
        line("aaa", "First", ""),
        r#"{"_type":"url","title":"no id here"}"#,
        line("ccc", "Third", ""),
    );

    let found = search::parse(&out);

    let ids: Vec<&str> = found.iter().map(|found| found.id.as_str()).collect();
    assert_eq!(ids, ["aaa", "ccc"], "a broken line never costs the others");
}

#[test]
fn an_empty_answer_is_no_results_and_not_a_failure() {
    assert_eq!(search::parse("").len(), 0);
    assert_eq!(search::parse("\n\n").len(), 0);
}

#[test]
fn the_query_is_one_argument_after_the_option_terminator() {
    let args: Vec<String> = search::arguments("AC/DC; rm -rf ~")
        .into_iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();

    let terminator = args.iter().position(|arg| arg == "--").expect("a `--`");
    assert_eq!(
        &args[terminator + 1..],
        ["ytsearch10:AC/DC; rm -rf ~"],
        "the query is one argument and the tool cannot read it as an option"
    );
    assert!(args.contains(&"--flat-playlist".to_owned()));
    assert!(args.contains(&"--dump-json".to_owned()));
    assert!(
        args.iter().any(|arg| arg == "10"),
        "ten results, as the search string says: {args:?}"
    );
    let leading = search::arguments("-not-an-option").join(&std::ffi::OsString::from(" "));
    assert!(
        leading
            .to_string_lossy()
            .contains("-- ytsearch10:-not-an-option"),
        "a leading dash is a query: {leading:?}"
    );
}

#[test]
fn the_search_runs_the_tool_and_reads_its_lines() {
    let dir = scratch("search");
    let answers = format!(
        "{}\n{}\n",
        line("aaa", "First", ""),
        line("bbb", "Second", "")
    );
    let program = fake(&dir, &format!("#!/bin/sh\ncat <<'JSON'\n{answers}JSON\n"));

    let found = search::run(&program, "anything").expect("the tool answers");

    assert_eq!(found.len(), 2);
    assert_eq!(found[1].title, "Second");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- describe: the one video a URL or a bare id names -----------------------

/// One video description as `--dump-single-json` answers it: the same
/// members the search reads, and several it must ignore.
const VIDEO: &str = concat!(
    r#"{"id":"abc123def45","title":"Dawn","uploader":"Chill Channel","duration":212.0,"#,
    r#""live_status":null,"webpage_url":"https://example.invalid/watch","thumbnails":[1,2,3]}"#
);

/// A broadcast: no length, and `live_status` naming it live.
const LIVE: &str = concat!(
    r#"{"id":"live1234567","title":"Streaming now","channel":"Evening","duration":null,"#,
    r#""live_status":"is_live"}"#
);

#[test]
fn describe_runs_one_extraction_and_reads_the_fields_a_row_shows() {
    let dir = scratch("describe");
    let program = fake(&dir, &format!("#!/bin/sh\ncat <<'JSON'\n{VIDEO}\nJSON\n"));

    let found =
        search::describe(&program, "https://youtu.be/abc123def45").expect("the URL describes");

    assert_eq!(found.id, "abc123def45");
    assert_eq!(found.title, "Dawn");
    assert_eq!(found.uploader.as_deref(), Some("Chill Channel"));
    assert_eq!(found.duration, Some(212), "a float duration rounds down");
    assert!(!found.live);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn describe_names_the_channel_when_there_is_no_uploader_and_marks_live() {
    let dir = scratch("describe-live");
    let program = fake(&dir, &format!("#!/bin/sh\ncat <<'JSON'\n{LIVE}\nJSON\n"));

    let found = search::describe(&program, "live1234567").expect("the bare id describes");

    assert_eq!(found.id, "live1234567");
    assert_eq!(found.uploader.as_deref(), Some("Evening"));
    assert_eq!(found.duration, None, "a live row never carries a length");
    assert!(found.live, "`is_live` in the description means live");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn describe_passes_the_canonical_watch_url_as_one_argument() {
    let args: Vec<String> = search::describe_arguments("abc123def45")
        .into_iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();

    let terminator = args.iter().position(|arg| arg == "--").expect("a `--`");
    assert_eq!(
        &args[terminator + 1..],
        ["https://www.youtube.com/watch?v=abc123def45"],
        "the id rides one watch URL the tool cannot read as an option"
    );
    assert!(args.contains(&"--dump-single-json".to_owned()));
    assert!(args.contains(&"--no-playlist".to_owned()));
}

#[test]
fn describe_refuses_an_answer_it_cannot_read_and_text_that_is_not_a_thing() {
    let dir = scratch("describe-bad");
    let program = fake(&dir, "#!/bin/sh\nprintf 'not json at all'\n");

    assert!(
        search::describe(&program, "https://youtu.be/abc123def45").is_err(),
        "an unusable answer is a failure, never a guessed row"
    );
    assert!(
        search::describe(&program, "a search phrase").is_err(),
        "text that is no YouTube thing never reaches the tool"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
