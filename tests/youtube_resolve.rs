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
use velo::youtube::resolve::{self, LIVE_SENTENCE, Resolver};

/// One extraction as `yt-dlp --dump-single-json` writes it: one object, a
/// `formats[]` with the members velo reads and a few it must ignore.
fn extraction(formats: &str) -> String {
    format!(r#"{{"id":"abc","title":"T","duration":212.0,"formats":[{formats}]}}"#)
}

/// One `formats[]` entry.
fn format(ext: &str, protocol: &str, abr: &str, url: &str) -> String {
    // The codec follows the container, as the tool's own answer does: an `m4a`
    // carries AAC, a `webm` carries opus. A fixture that claimed AAC-LC inside
    // a webm would let the picker pass a stream nothing here can decode.
    let acodec = match ext {
        "m4a" => "mp4a.40.2",
        "mp3" => "mp3",
        _ => "opus",
    };
    format!(
        r#"{{"format_id":"x","ext":"{ext}","vcodec":"none","acodec":"{acodec}","protocol":"{protocol}","abr":{abr},"url":"{url}","http_headers":{{"User-Agent":"x"}}}}"#
    )
}

/// One format with its codec named: `YouTube` serves AAC-LC and HE-AAC both as
/// `m4a`, and lists the HE-AAC one first.
fn coded(ext: &str, acodec: &str, abr: &str, url: &str) -> String {
    format!(
        r#"{{"format_id":"x","ext":"{ext}","vcodec":"none","acodec":"{acodec}","protocol":"https","abr":{abr},"url":"{url}"}}"#
    )
}

/// A fake program whose answer is `stdout`'s text, read through a heredoc so
/// no shell quoting touches the JSON.
fn fake_answer(dir: &Path, answer: &str) -> PathBuf {
    fake(dir, &format!("#!/bin/sh\ncat <<'JSON'\n{answer}\nJSON\n"))
}

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
fn the_resolve_asks_for_the_full_extraction_in_a_format_the_decoder_can_read() {
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
    assert!(
        args.contains(&"--dump-single-json".to_owned()),
        "the full extraction, not the bare `--get-url` URL"
    );
    assert!(
        !args.contains(&"--get-url".to_owned()),
        "the bare URL route is gone: a plain ranged GET answers it 403"
    );
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
fn a_recorded_answer_resolves_to_the_m4a_url_never_the_manifest() {
    let dir = scratch("three-formats");
    let answer = extraction(
        &[
            format(
                "m4a",
                "m3u8_native",
                "null",
                "https://manifest.example/audio.m4a",
            ),
            format("m4a", "https", "128", "https://media.example/audio.m4a"),
            format("mp3", "https", "96", "https://media.example/audio.mp3"),
        ]
        .join(", "),
    );
    let program = fake_answer(&dir, &answer);
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
        "the plain-HTTP m4a is what the transport was asked for, never the manifest"
    );
    drop(source);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn without_an_m4a_the_mp3_plays_and_without_either_the_highest_abr_does() {
    let dir = scratch("preference");
    let answer = extraction(
        &[
            format("webm", "https", "50", "https://media.example/low.webm"),
            format("mp3", "https", "96", "https://media.example/audio.mp3"),
            format("webm", "https", "160", "https://media.example/high.webm"),
        ]
        .join(", "),
    );
    let program = fake_answer(&dir, &answer);
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
        ["https://media.example/audio.mp3"],
        "the mp3 outranks the webm formats even at a higher abr"
    );
    drop(source);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_answer_with_only_manifests_is_refused_before_any_transport_ask() {
    let dir = scratch("manifests-only");
    let answer = extraction(
        &[
            format(
                "m4a",
                "m3u8_native",
                "128",
                "https://manifest.example/one.m3u8",
            ),
            format("webm", "m3u8", "96", "https://manifest.example/two.m3u8"),
        ]
        .join(", "),
    );
    let program = fake_answer(&dir, &answer);
    let seen = Arc::new(Seen {
        urls: Mutex::new(Vec::new()),
    });
    let resolver =
        Resolver::new(program).with_transport(Arc::clone(&seen) as Arc<dyn RangeTransport>);

    let error = resolver
        .open_streaming(&pseudo_path("abc"))
        .err()
        .expect("nothing playable");

    assert_eq!(
        error.to_string(),
        LIVE_SENTENCE,
        "the refusal says what it is, in its own words: {error}"
    );
    assert!(
        seen.urls.lock().expect("the record unlocks").is_empty(),
        "the refusal happens before any HTTP request: the transport saw nothing"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_refusal_never_reads_as_a_file_read() {
    let dir = scratch("refusal-wording");
    let answer = extraction(&format(
        "m4a",
        "m3u8_native",
        "128",
        "https://manifest.example/a.m3u8",
    ));
    let cases = [
        // The live/manifest refusal...
        fake_answer(&dir, &answer),
        // ...and the tool's own failure: both must not wear the prefix.
        fake(
            &dir,
            "#!/bin/sh\nprintf 'ERROR: Video unavailable\n' >&2\nexit 1\n",
        ),
    ];
    for program in &cases {
        let seen = Arc::new(Seen {
            urls: Mutex::new(Vec::new()),
        });
        let resolver = Resolver::new(program.clone())
            .with_transport(Arc::clone(&seen) as Arc<dyn RangeTransport>);
        let error = resolver
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
fn the_stream_is_opened_with_the_url_the_extraction_carries() {
    let dir = scratch("resolve");
    let program = fake_answer(
        &dir,
        &extraction(&format(
            "m4a",
            "https",
            "128",
            "https://media.example/audio.m4a",
        )),
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

#[test]
fn he_aac_listed_first_never_wins_over_the_aac_lc_that_actually_decodes() {
    // Measured against the real tool: `YouTube` lists itag 139 (HE-AAC,
    // `mp4a.40.5`, ~48 kbps) before itag 140 (AAC-LC, `mp4a.40.2`, ~129 kbps),
    // both as `m4a`. symphonia reads the second and refuses the first at decode
    // time ("aac too complex"), so picking by container alone plays nothing.
    let dir = scratch("he-aac-first");
    let answer = extraction(
        &[
            coded(
                "m4a",
                "mp4a.40.5",
                "48.783",
                "https://media.example/he-aac.m4a",
            ),
            coded(
                "m4a",
                "mp4a.40.2",
                "129.472",
                "https://media.example/aac-lc.m4a",
            ),
        ]
        .join(", "),
    );
    let program = fake_answer(&dir, &answer);
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
        ["https://media.example/aac-lc.m4a"],
        "the profile that decodes wins, whatever order the tool listed them in"
    );
    drop(source);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_answer_with_only_undecodable_audio_says_so_and_is_not_called_live() {
    let dir = scratch("undecodable");
    let answer = extraction(
        &[
            coded("m4a", "mp4a.40.5", "48", "https://media.example/he-aac.m4a"),
            coded("webm", "opus", "160", "https://media.example/audio.webm"),
        ]
        .join(", "),
    );
    let program = fake_answer(&dir, &answer);
    let seen = Arc::new(Seen {
        urls: Mutex::new(Vec::new()),
    });
    let resolver =
        Resolver::new(program).with_transport(Arc::clone(&seen) as Arc<dyn RangeTransport>);

    let Err(error) = resolver.open_streaming(&pseudo_path("abc")) else {
        panic!("nothing here decodes");
    };

    assert_eq!(
        error.to_string(),
        velo::youtube::resolve::UNDECODABLE_SENTENCE
    );
    assert!(
        !error.to_string().contains("live"),
        "a recorded video is not called live: {error}"
    );
    assert!(
        seen.urls.lock().expect("the record unlocks").is_empty(),
        "nothing is asked of the network when nothing can be decoded"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
