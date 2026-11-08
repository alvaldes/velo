//! The HTTP live-stream opener for radio stations (T5b1, spec S2).
//!
//! A station URL must turn into something [`TrackSource`] can decode, with
//! `Icy-MetaData: 1` sent before the body so the server interleaves
//! `StreamTitle`, and with timeouts that are **per operation**: an endless
//! live body must never hit a total deadline, and a stalled socket must
//! give up within the injected bound. No test touches the real network:
//! every case speaks to a `std::net::TcpListener` on `127.0.0.1:0` that
//! answers with canned bytes, exactly like `tests/subsonic.rs`.

use std::io::{Cursor, Read as _, Write as _};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use symphonia::core::io::ReadOnlySource;
use velo::audio::TrackSource;
use velo::radio::stream::{self, StreamError, Timeouts};

/// The ICY title the fake servers embed.
const STREAM_TITLE: &str = "Fela Kuti - Zombie";

/// Same bound for every fast-fail assertion: slower means a hang, not a
/// test failure worth waiting for.
const HANG_BOUND: Duration = Duration::from_secs(5);

/// The injected timeouts under test: small enough that the stall cases fail
/// in milliseconds instead of seconds.
fn timeouts() -> Timeouts {
    Timeouts {
        connect: Duration::from_secs(2),
        response: Duration::from_millis(500),
        read: Duration::from_millis(300),
    }
}

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/silence-then-tone.mp3")
}

/// One ICY metadata block: a length byte followed by `len * 16` bytes.
fn metadata_block(payload: &[u8]) -> Vec<u8> {
    assert_eq!(payload.len() % 16, 0, "ICY payload is padded to 16 bytes");
    let mut block = vec![u8::try_from(payload.len() / 16).expect("test payload fits one byte")];
    block.extend_from_slice(payload);
    block
}

/// Wraps `audio` in ICY metadata the way `tests/radio_seam.rs` builds it:
/// every [`interval`] audio bytes a `StreamTitle` block (first) or an empty
/// `L = 0` block (the rest).
fn icy_wrap(audio: &[u8], interval: usize) -> Vec<u8> {
    let title = format!("StreamTitle='{STREAM_TITLE}';");
    let mut padded = title.into_bytes();
    while padded.len() % 16 != 0 {
        padded.push(0);
    }
    let title_block = metadata_block(&padded);
    let empty_block = metadata_block(&[]);

    let mut wrapped = Vec::with_capacity(audio.len() + 16 * (audio.len() / interval + 1));
    for chunk in audio.chunks(interval) {
        wrapped.extend_from_slice(chunk);
        wrapped.extend_from_slice(&title_block);
        for _ in 1..audio.len().div_ceil(interval).max(1) {
            wrapped.extend_from_slice(&empty_block);
        }
    }
    wrapped
}

/// A 200 head with `extra` header lines (each ending in `\r\n`).
fn ok_head(extra: &str, body_len: usize) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\n{extra}Content-Length: {body_len}\r\nConnection: close\r\n\r\n"
    )
}

/// Serves exactly one connection: reads the request, answers with
/// `head` + `body`, and hands the request bytes back through a channel so
/// a test can assert on what was sent.
fn serve(listener: TcpListener, head: String, body: Vec<u8>) -> mpsc::Receiver<Vec<u8>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut connection, _) = listener.accept().expect("a connection arrives");
        let mut request = vec![0_u8; 4096];
        let read = connection.read(&mut request).unwrap_or(0);
        let _ = sender.send(request[..read].to_vec());
        connection
            .write_all(head.as_bytes())
            .and_then(|()| connection.write_all(&body))
            .expect("the response is written");
    });
    receiver
}

/// Binds a listener, spawns its one-shot server and returns the station URL
/// plus the request the server received.
fn served(head: String, body: Vec<u8>) -> (String, mpsc::Receiver<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
    let url = format!("http://{}", listener.local_addr().expect("a local address"));
    let requests = serve(listener, head, body);
    (url, requests)
}

/// A server that never answers: it accepts, reads the request and parks.
fn served_silently() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
    let url = format!("http://{}", listener.local_addr().expect("a local address"));
    std::thread::spawn(move || {
        let (mut connection, _) = listener.accept().expect("a connection arrives");
        let mut request = [0_u8; 4096];
        let _ = connection.read(&mut request);
        loop {
            std::thread::park();
        }
    });
    url
}

/// A server that writes `sent` bytes of a body that declares `declared`
/// total bytes and then goes silent with the connection open.
fn served_partially(head: String, sent: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
    let url = format!("http://{}", listener.local_addr().expect("a local address"));
    std::thread::spawn(move || {
        let (mut connection, _) = listener.accept().expect("a connection arrives");
        let mut request = [0_u8; 4096];
        let _ = connection.read(&mut request);
        connection
            .write_all(head.as_bytes())
            .and_then(|()| connection.write_all(&sent))
            .expect("the partial response is written");
        loop {
            std::thread::park();
        }
    });
    url
}

/// Reads any track-like source to its end.
trait ReadBlocks {
    fn read_block(&mut self) -> Result<Option<Vec<f32>>, velo::audio::AudioError>;
}

impl ReadBlocks for TrackSource {
    fn read_block(&mut self) -> Result<Option<Vec<f32>>, velo::audio::AudioError> {
        TrackSource::read_block(self)
    }
}

impl ReadBlocks for velo::radio::stream::StationStream {
    fn read_block(&mut self) -> Result<Option<Vec<f32>>, velo::audio::AudioError> {
        velo::radio::stream::StationStream::read_block(self)
    }
}

/// Decodes a whole source to its end.
fn drain(source: &mut impl ReadBlocks) -> Vec<f32> {
    let mut samples = Vec::new();
    while let Some(block) = source.read_block().expect("decoding succeeds") {
        samples.extend_from_slice(&block);
    }
    samples
}

/// The same audio as the server sends, with no metadata, decoded straight
/// from memory — the expected sample stream of a correct demux.
fn reference(plain: &[u8]) -> Vec<f32> {
    let source = ReadOnlySource::new(Cursor::new(plain.to_vec()));
    let mut track = TrackSource::open_stream(Box::new(source)).expect("the reference opens");
    drain(&mut track)
}

// ------------------------------------------------------------------- playing

#[test]
fn an_icy_stream_plays_and_reports_the_title() {
    let plain = std::fs::read(fixture()).expect("the fixture is readable");
    let wrapped = icy_wrap(&plain, 8192);
    let (url, requests) = served(ok_head("icy-metaint: 8192\r\n", wrapped.len()), wrapped);

    let start = Instant::now();
    let mut station = stream::open(&url, timeouts()).expect("the station opens");
    let samples = drain(&mut station);

    assert_eq!(
        samples,
        reference(&plain),
        "the demux must deliver exactly the un-metadata'd audio"
    );
    assert_eq!(
        station.title().as_deref(),
        Some(STREAM_TITLE),
        "the title is reported"
    );
    assert_eq!(station.title_seq(), 1, "one title so far");
    assert!(
        start.elapsed() < HANG_BOUND,
        "opening a fast station must not take {:?}",
        start.elapsed()
    );

    let request = String::from_utf8_lossy(
        &requests
            .recv_timeout(Duration::from_secs(2))
            .expect("the server saw one request"),
    )
    .to_ascii_lowercase();
    // ureq normalises header names to lowercase on the wire; the header is
    // there either way.
    assert!(
        request.contains("icy-metadata: 1"),
        "the request must ask for metadata before the body, got: {request}"
    );
}

#[test]
fn a_stream_without_metaint_is_pure_passthrough() {
    let plain = std::fs::read(fixture()).expect("the fixture is readable");
    let (url, _requests) = served(ok_head("", plain.len()), plain.clone());

    let mut station = stream::open(&url, timeouts()).expect("the station opens");
    let samples = drain(&mut station);

    assert_eq!(samples, reference(&plain), "passthrough plays the audio");
    assert_eq!(station.title(), None, "no metaint, no title");
    assert_eq!(station.title_seq(), 0);
}

// ---------------------------------------------------------------- validation

#[test]
fn a_metaint_of_zero_or_beyond_64k_is_rejected_naming_the_value() {
    let plain = std::fs::read(fixture()).expect("the fixture is readable");
    for value in ["0", "65537", "not-a-number"] {
        let (url, _requests) =
            served(ok_head(&format!("icy-metaint: {value}\r\n"), plain.len()), plain.clone());
        let error = stream::open(&url, timeouts())
            .expect_err("an unusable metaint must be refused, not decoded");
        let StreamError::BadMetaint { value: named } = error else {
            panic!("expected BadMetaint, got: {error:?}");
        };
        assert!(
            named.contains(value),
            "the error must name the value {value}, got: {named}"
        );
    }
}

#[test]
fn a_non_200_status_is_an_error_naming_it() {
    let head = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    let (url, _requests) = served(head.to_owned(), Vec::new());

    let error = stream::open(&url, timeouts()).expect_err("404 is not a station");
    assert!(matches!(error, StreamError::BadStatus { status: 404 }), "got: {error:?}");
}

#[test]
fn a_body_that_is_not_audio_is_rejected() {
    let playlist = b"#EXTM3U\nhttp://example.com/stream.m3u8\n".to_vec();
    let (url, _requests) = served(ok_head("", playlist.len()), playlist);

    let error = stream::open(&url, timeouts()).expect_err("a playlist body is not audio");
    assert!(matches!(error, StreamError::NotAudio { .. }), "got: {error:?}");
}

// ------------------------------------------------------------------ timeouts

#[test]
fn a_server_that_sends_nothing_gives_up_within_the_response_bound() {
    let url = served_silently();

    let start = Instant::now();
    let result = stream::open(&url, timeouts());
    let elapsed = start.elapsed();

    assert!(result.is_err(), "a silent server must fail, not hang");
    assert!(
        elapsed < HANG_BOUND,
        "the open took {elapsed:?}; the injected response bound did not fire"
    );
}

#[test]
fn a_stalled_read_gives_up_within_the_read_bound() {
    let plain = std::fs::read(fixture()).expect("the fixture is readable");
    let wrapped = icy_wrap(&plain, 8192);
    // The head declares more than the server will ever send, so after the
    // buffered bytes are consumed the socket must be read again — and it
    // has nothing: the per-read bound, not a total deadline, must fire.
    let head = ok_head(
        "icy-metaint: 8192\r\n",
        wrapped.len() + 4096,
    );
    let url = served_partially(head, wrapped);

    let start = Instant::now();
    let mut station = stream::open(&url, timeouts()).expect("the buffered prefix opens");
    let mut played = 0;
    let outcome = loop {
        match station.read_block() {
            Ok(Some(block)) => {
                assert_ne!(block, [] as [f32; 0]);
                played += 1;
            }
            Ok(None) => break Ok(()),
            Err(error) => break Err(error),
        }
    };
    let elapsed = start.elapsed();

    assert!(outcome.is_err(), "a stalled body must end in an error, not a clean end");
    assert!(played > 0, "the buffered prefix must have played first");
    assert!(
        elapsed < HANG_BOUND,
        "the stall took {elapsed:?}; the injected read bound did not fire"
    );
}

#[test]
fn a_body_cut_mid_stream_never_panics_or_hangs() {
    let plain = std::fs::read(fixture()).expect("the fixture is readable");
    let wrapped = icy_wrap(&plain, 8192);
    let head = ok_head("icy-metaint: 8192\r\n", wrapped.len());
    let url = served_partially(head, wrapped[..wrapped.len() / 2].to_vec());

    let start = Instant::now();
    let mut station = stream::open(&url, timeouts()).expect("the prefix opens");
    let outcome = loop {
        match station.read_block() {
            Ok(Some(_)) => {}
            Ok(None) => break Ok(()),
            Err(error) => break Err(error),
        }
    };
    let elapsed = start.elapsed();

    // Either a clean end or an error is fine; a panic or a hang is not.
    let _ = outcome;
    assert!(
        elapsed < HANG_BOUND,
        "the cut took {elapsed:?}; the stream hung instead of ending"
    );
}
