//! Wrapper playlist tests (spec S7): PLS and M3U resolve to their first
//! playable entry, HLS is its own error, and a wrapper that yields nothing
//! playable never masquerades as "unsupported format".
//!
//! All fixtures are string literals in this file: no network, no files
//! outside the repo, no new dependencies.

use velo::radio::lists::{ListsError, unwrap_station};

/// The real wrapper Radio Browser hands out for cliamp's NCS Chill station
/// (120 bytes, LF, ASCII): one entry, blank lines, a trailing version.
const NCS_CHILL_PLS: &str = "\
[playlist]
NumberOfEntries=1

File1=https://radio.cliamp.stream/ncs-chill/stream
Title1=NCS Chill
Length1=-1

Version=2
";

const NCS_CHILL_URL: &str = "https://radio.cliamp.stream/ncs-chill/stream";

#[test]
fn pls_with_lf_lines_returns_the_first_entry() {
    assert_eq!(
        unwrap_station(NCS_CHILL_PLS.as_bytes()),
        Ok(NCS_CHILL_URL.to_string())
    );
}

#[test]
fn pls_with_crlf_lines_returns_the_first_entry() {
    let body = NCS_CHILL_PLS.replace('\n', "\r\n");
    assert_eq!(
        unwrap_station(body.as_bytes()),
        Ok(NCS_CHILL_URL.to_string())
    );
}

#[test]
fn pls_entries_sorted_numerically_not_by_file_order() {
    // File3 is written first; File1 must still win.
    let body = "\
[playlist]
NumberOfEntries=3

File3=https://example.org/third
Title3=Third
Length3=-1

File1=https://example.org/first
Title1=First
Length1=-1

File2=https://example.org/second
Title2=Second
Length2=-1
";
    assert_eq!(
        unwrap_station(body.as_bytes()),
        Ok("https://example.org/first".to_string())
    );
}

#[test]
fn pls_keys_and_header_are_case_insensitive() {
    let body = "\
[PLAYLIST]
numberofentries=1

file1=https://example.org/stream
title1=Some Station
length1=-1
";
    assert_eq!(
        unwrap_station(body.as_bytes()),
        Ok("https://example.org/stream".to_string())
    );
}

#[test]
fn pls_without_a_playable_entry_is_no_stream() {
    let body = "\
[playlist]
NumberOfEntries=1
File1=relative/path/stream.pls
Title1=Broken
Length1=-1
";
    assert_eq!(unwrap_station(body.as_bytes()), Err(ListsError::NoStream));
}

#[test]
fn m3u_extinf_pairs_return_the_url() {
    let body = "\
#EXTM3U
#EXTINF:-1,NCS Chill
https://radio.cliamp.stream/ncs-chill/stream
#EXTINF:-1,Other
https://example.org/other
";
    assert_eq!(
        unwrap_station(body.as_bytes()),
        Ok(NCS_CHILL_URL.to_string())
    );
}

#[test]
fn m3u_tolerates_bom_comments_and_blank_lines() {
    let body = "\u{feff}\
#EXTM3U
# written by hand

#EXTINF:-1,NCS Chill
https://radio.cliamp.stream/ncs-chill/stream
";
    assert_eq!(
        unwrap_station(body.as_bytes()),
        Ok(NCS_CHILL_URL.to_string())
    );
}

#[test]
fn m3u_skips_non_http_entries_before_a_valid_one() {
    let body = "\
#EXTM3U
#EXTINF:-1,Ftp source
ftp://example.org/stream
#EXTINF:-1,Good source
https://example.org/good
";
    assert_eq!(
        unwrap_station(body.as_bytes()),
        Ok("https://example.org/good".to_string())
    );
}

#[test]
fn m3u_with_only_a_relative_path_is_no_stream() {
    let body = "\
#EXTM3U
#EXTINF:-1,Local file
music/episode-01.mp3
";
    assert_eq!(unwrap_station(body.as_bytes()), Err(ListsError::NoStream));
}

#[test]
fn hls_body_is_reported_as_hls_never_as_a_segment_url() {
    let body = "\
#EXTM3U
#EXT-X-STREAM-INF:BANDWIDTH=128000,CODECS=\"mp4a.40.2\"
https://example.org/live/playlist.m3u8
#EXT-X-TARGETDURATION:10
";
    assert_eq!(unwrap_station(body.as_bytes()), Err(ListsError::Hls));
}

#[test]
fn a_body_that_is_neither_format_is_not_a_playlist() {
    assert_eq!(
        unwrap_station(b"<html><body>Not found</body></html>"),
        Err(ListsError::NotAPlaylist)
    );
}

#[test]
fn an_empty_body_is_not_a_playlist() {
    assert_eq!(unwrap_station(b""), Err(ListsError::NotAPlaylist));
}

#[test]
fn invalid_utf8_does_not_panic() {
    // A PLS whose URL carries invalid bytes: must yield NoStream or a URL,
    // never a panic.
    let body = b"[playlist]\nNumberOfEntries=1\nFile1=\xff\xfe-not-a-url\n";
    let result = unwrap_station(body);
    assert!(matches!(result, Ok(_) | Err(ListsError::NoStream)));
}

// ---- every entry, for `velo playlist import` (issue #9) ---------------------
//
// The whole parse `unwrap_station` starts from, exposed without the
// station-only filter: local paths and relative entries come through, in
// order, because a saved playlist is paths. The station verdicts — which
// format, HLS — carry over unchanged.

use velo::radio::lists::entries;

#[test]
fn entries_of_a_pls_come_back_sorted_numerically_with_nothing_filtered() {
    let body = "\
[playlist]\n\
NumberOfEntries=3\n\
File3=/music/third.flac\n\
File1=music/first.flac\n\
File2=http://example.org/second\n\
";
    assert_eq!(
        entries(body.as_bytes()).unwrap(),
        [
            String::from("music/first.flac"),
            String::from("http://example.org/second"),
            String::from("/music/third.flac"),
        ],
        "File1 before File2 before File3, however the file wrote them; a local path is an entry too"
    );
}

#[test]
fn entries_of_an_m3u_come_back_in_file_order_comments_and_blanks_skipped() {
    let body = "\
#EXTM3U\n\
#EXTINF:-1,Local\n\
music/episode-01.mp3\n\
\n\
#EXTINF:-1,Remote\n\
https://example.org/stream\n\
";
    assert_eq!(
        entries(body.as_bytes()).unwrap(),
        [
            String::from("music/episode-01.mp3"),
            String::from("https://example.org/stream"),
        ]
    );
}

#[test]
fn entries_make_the_same_verdicts_unwrap_station_makes() {
    assert_eq!(
        entries(b"<html>Not found</html>"),
        Err(ListsError::NotAPlaylist)
    );
    assert_eq!(entries(b""), Err(ListsError::NotAPlaylist));
    assert_eq!(
        entries(b"#EXTM3U\n#EXT-X-TARGETDURATION:10\n"),
        Err(ListsError::Hls)
    );
}

#[test]
fn entries_of_an_m3u_carrying_only_comments_come_back_empty() {
    // Empty is the importer's "nothing to save" case, not a parse error.
    assert_eq!(entries(b"#EXTM3U\n# only comments\n"), Ok(Vec::new()));
}
