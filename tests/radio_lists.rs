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
