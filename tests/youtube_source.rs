//! The `yt-dlp` seam (issue #70, S6): one external process per call, bounded by
//! a deadline it cannot outlive, off the event loop, and never a hang the
//! interface waits on.
//!
//! Every case here runs a fake program written into a scratch directory: no
//! test in this file reaches the network, and none of them needs `yt-dlp`
//! installed.

#![cfg(unix)]

use std::path::{Path, PathBuf};

use velo::audio::output::PathResolver;
use velo::youtube::{
    Unconfigured, detect_in, input_video_id, is_pseudo_path, pseudo_path, video_id,
};

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

/// One `PATH` value holding exactly `dirs`.
fn path_of(dirs: &[&Path]) -> std::ffi::OsString {
    std::env::join_paths(dirs).expect("the path joins")
}

#[test]
fn a_program_on_the_path_is_found() {
    let dir = scratch("detect");
    let program = fake(&dir, "#!/bin/sh\nexit 0\n");

    assert_eq!(detect_in(&path_of(&[&dir])), Some(program));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_file_that_is_not_executable_is_not_a_tool() {
    let dir = scratch("detect-mode");
    std::fs::write(dir.join("yt-dlp"), "not a program").expect("the file is written");

    assert_eq!(detect_in(&path_of(&[&dir])), None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_empty_path_has_nothing_on_it() {
    assert_eq!(detect_in(std::ffi::OsStr::new("")), None);
}

#[test]
fn a_video_id_survives_the_round_trip() {
    let path = pseudo_path("dQw4w9WgXcQ");

    assert_eq!(path, PathBuf::from("youtube://dQw4w9WgXcQ"));
    assert!(is_pseudo_path(&path));
    assert_eq!(video_id(&path).expect("a video id"), Some("dQw4w9WgXcQ"));

    // The ids that are not plain letters: a dash and an underscore are ids too.
    let odd = pseudo_path("a-b_c123");
    assert_eq!(video_id(&odd).expect("a video id"), Some("a-b_c123"));
}

#[test]
fn another_source_row_is_not_ours() {
    let subsonic = PathBuf::from("subsonic://host/123");

    assert!(!is_pseudo_path(&subsonic));
    assert_eq!(video_id(&subsonic).expect("not ours"), None);
    assert_eq!(
        video_id(&PathBuf::from("/music/a.flac")).expect("not ours"),
        None
    );
}

#[test]
fn a_row_that_says_youtube_and_carries_no_usable_id_is_refused() {
    for broken in [
        "youtube://",
        "youtube://a&list=PL123",
        "youtube://../etc/passwd",
        "youtube://id with spaces",
    ] {
        let error = video_id(&PathBuf::from(broken)).expect_err(broken);
        assert!(
            error.to_string().contains("is not a YouTube entry"),
            "{broken}: {error}"
        );
    }
}

#[test]
fn a_local_file_passes_through_the_resolver_for_a_machine_without_the_tool() {
    let resolver = Unconfigured::new(Path::new("/nowhere/yt-dlp"));
    let file = PathBuf::from("/music/a.flac");

    assert_eq!(
        resolver.resolve(&file).expect("passes through"),
        file.clone()
    );
    assert!(resolver.cached(&file), "a local file is already reachable");
}

#[test]
fn a_youtube_row_says_how_to_get_the_tool() {
    let resolver = Unconfigured::new(Path::new("/nowhere/yt-dlp"));
    let row = pseudo_path("dQw4w9WgXcQ");

    assert!(
        !resolver.cached(&row),
        "a YouTube row is never reachable yet"
    );
    let error = resolver.resolve(&row).expect_err("nothing to play");
    assert!(
        error.to_string().contains("not installed"),
        "the sentence says how to get it: {error}"
    );
    // The wrapper above it is the one every unreachable source carries (the
    // Subsonic resolver answers the same way); what matters is that the
    // sentence a user can act on is in there, not flattened away.
    assert!(
        error.to_string().contains(resolver.message()),
        "the install sentence survives the wrapper: {error}"
    );
    assert_ne!(resolver.message(), "");
}

/// The id a pasted `YouTube` URL, the app's own identity, or a bare id names:
/// every shape a person actually pastes answers the same eleven characters,
/// and a URL carrying extra query parameters (`&list=`, `&t=`) keeps only
/// the video it names.
#[test]
fn a_pasted_youtube_thing_names_its_video_id() {
    for (text, id) in [
        ("https://www.youtube.com/watch?v=dQw4w9WgXcQ", "dQw4w9WgXcQ"),
        (
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ&list=PL123&t=42s",
            "dQw4w9WgXcQ",
        ),
        ("https://youtu.be/dQw4w9WgXcQ", "dQw4w9WgXcQ"),
        ("https://youtu.be/dQw4w9WgXcQ?t=30", "dQw4w9WgXcQ"),
        ("https://www.youtube.com/shorts/dQw4w9WgXcQ", "dQw4w9WgXcQ"),
        ("https://www.youtube.com/embed/dQw4w9WgXcQ", "dQw4w9WgXcQ"),
        ("https://www.youtube.com/live/dQw4w9WgXcQ", "dQw4w9WgXcQ"),
        ("youtube://dQw4w9WgXcQ", "dQw4w9WgXcQ"),
        ("dQw4w9WgXcQ", "dQw4w9WgXcQ"),
        ("youtube.com/watch?v=dQw4w9WgXcQ", "dQw4w9WgXcQ"),
        ("  https://youtu.be/dQw4w9WgXcQ  ", "dQw4w9WgXcQ"),
        // Scheme and host fold case — what a person actually pastes —
        // while the path and the id stay case-sensitive (an id is).
        ("HTTPS://youtu.be/dQw4w9WgXcQ", "dQw4w9WgXcQ"),
        ("Http://youtu.be/dQw4w9WgXcQ", "dQw4w9WgXcQ"),
        ("YouTube.com/watch?v=dQw4w9WgXcQ", "dQw4w9WgXcQ"),
        ("WWW.YouTube.com/shorts/dQw4w9WgXcQ", "dQw4w9WgXcQ"),
    ] {
        assert_eq!(input_video_id(text).as_deref(), Some(id), "{text}");
    }
}

/// Anything else is `None` — never a guess: another site, a search phrase,
/// a playlist, and an id of the wrong length all stay queries.
#[test]
fn anything_else_is_not_a_video_id() {
    for text in [
        "https://example.com/watch?v=dQw4w9WgXcQ",
        "dQw4w9WgXc",
        "dQw4w9WgXcQdQw4w9WgXcQ",
        "https://www.youtube.com/playlist?list=PL123",
        "youtube.com/watch?vi=dQw4w9WgXcQ",
        "https://youtu.be/",
        "a search phrase",
        "",
        // The path stays case-sensitive: an id is, and so is the route to it.
        "youtube.com/WATCH?v=dQw4w9WgXcQ",
        // Another scheme is not folded into the two that are accepted.
        "FTP://youtu.be/dQw4w9WgXcQ",
    ] {
        assert_eq!(input_video_id(text), None, "{text}");
    }
}
