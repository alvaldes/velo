//! The HTTP live-stream opener for radio stations (T5b1, spec S2).
//!
//! A station URL must turn into something [`TrackSource`] can decode, with
//! `Icy-MetaData: 1` sent before the body so the server interleaves
//! `StreamTitle`, and with timeouts that are **per operation**: an endless
//! live body must never hit a total deadline, and a stalled socket must
//! give up within the injected bound. No test touches the real network:
//! every case speaks to a `std::net::TcpListener` on `127.0.0.1:0` that
//! answers with canned bytes, exactly like `tests/subsonic.rs`.

use std::collections::HashMap;
use std::io::{Cursor, Read as _, Write as _};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
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

/// The wrapper tests' timeouts: looser bounds than [`timeouts`], because
/// under a loaded test machine a 500 ms response bound can fire before the
/// local server thread is even scheduled, producing a spurious
/// `Unreachable` that masks the error variant actually under test (the
/// lesson from the T6b flake: never let a scheduler race decide the
/// outcome). The bodies here are a few KiB and the server always answers,
/// so the looser bounds are never approached — and every assertion still
/// sits far inside [`HANG_BOUND`].
fn wrapper_timeouts() -> Timeouts {
    Timeouts {
        connect: Duration::from_secs(2),
        response: Duration::from_secs(2),
        read: Duration::from_secs(1),
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
        let (url, _requests) = served(
            ok_head(&format!("icy-metaint: {value}\r\n"), plain.len()),
            plain.clone(),
        );
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
    assert!(
        matches!(error, StreamError::BadStatus { status: 404 }),
        "got: {error:?}"
    );
}

#[test]
fn a_body_that_is_neither_audio_nor_a_playlist_is_rejected() {
    // Once resolved wrappers (T5b2), a playlist body no longer lands here:
    // it resolves. What keeps its current behaviour is a body that is
    // neither audio nor a wrapper, so this test serves exactly that.
    let junk = b"this is neither audio nor a wrapper playlist\r\n".to_vec();
    let (url, _requests) = served(ok_head("", junk.len()), junk);

    let error = stream::open(&url, timeouts()).expect_err("junk is not audio");
    assert!(
        matches!(error, StreamError::NotAudio { .. }),
        "got: {error:?}"
    );
}

// ------------------------------------------------------------------ wrappers (T5b2)

/// The fixture audio as the server sends it, plus the plain bytes the
/// correct demux must reproduce.
fn audio_parts() -> (Vec<u8>, Vec<u8>) {
    let plain = std::fs::read(fixture()).expect("the fixture is readable");
    let wrapped = icy_wrap(&plain, 8192);
    (plain, wrapped)
}

/// A multi-route server that records the path of every request it serves,
/// so a test can prove which URLs were fetched, how often, and which were
/// never touched. One short-lived connection per request (`Connection:
/// close` in every head, like [`ok_head`]).
struct CountingServer {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
}

impl CountingServer {
    /// Binds a listener and serves the routes `build` returns; `build`
    /// receives the server's base URL so routes can point at each other.
    fn start(build: impl FnOnce(&str) -> Vec<(String, String, Vec<u8>)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
        let base = format!("http://{}", listener.local_addr().expect("a local address"));
        let routes: HashMap<String, (String, Vec<u8>)> = build(&base)
            .into_iter()
            .map(|(path, extra, body)| (path, (ok_head(&extra, body.len()), body)))
            .collect();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));

        let seen = Arc::clone(&requests);
        let done = Arc::clone(&stop);
        std::thread::spawn(move || {
            listener
                .set_nonblocking(true)
                .expect("the listener goes nonblocking");
            while !done.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut connection, _)) => {
                        // On macOS an accepted socket inherits the
                        // listener's non-blocking flag, so a read can
                        // return `WouldBlock` before the request bytes
                        // arrive — which must look like "wait", never like
                        // EOF. Back to blocking before reading.
                        connection
                            .set_nonblocking(false)
                            .expect("the connection blocks");
                        // Read until the request headers are complete: one
                        // `read` can return a partial request under load,
                        // and a truncated path would answer a spurious 404.
                        let mut request = Vec::with_capacity(4096);
                        let mut chunk = [0_u8; 1024];
                        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                            let read = connection.read(&mut chunk).unwrap_or(0);
                            if read == 0 || request.len() > 4096 {
                                break;
                            }
                            request.extend_from_slice(&chunk[..read]);
                        }
                        let request = String::from_utf8_lossy(&request).to_string();
                        let path = request
                            .lines()
                            .next()
                            .and_then(|line| line.split_whitespace().nth(1))
                            .unwrap_or("")
                            .to_string();
                        seen.lock()
                            .expect("the request log is uncontended")
                            .push(path.clone());
                        if let Some((head, body)) = routes
                            .get(&path)
                            .map(|(head, body)| (head.as_bytes().to_vec(), body.clone()))
                        {
                            connection
                                .write_all(&head)
                                .and_then(|()| connection.write_all(&body))
                                .expect("the route answer is written");
                        } else {
                            connection
                                .write_all(
                                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                                )
                                .expect("the 404 is written");
                        }
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        });

        Self {
            url: base,
            requests,
            stop,
        }
    }

    /// The full URL of one route on this server.
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.url)
    }

    /// The recorded request paths, in arrival order.
    fn paths(&self) -> Vec<String> {
        self.requests
            .lock()
            .expect("the request log is uncontended")
            .clone()
    }

    /// Waits until at least `n` requests have arrived.
    fn wait_for(&self, n: usize) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.paths().len() < n && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// A grace window before asserting a negative: any request that was
    /// going to happen has had every chance to arrive.
    fn settle() {
        std::thread::sleep(Duration::from_millis(300));
    }
}

impl Drop for CountingServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[test]
fn a_pls_wrapper_resolves_to_its_stream_and_plays() {
    let (plain, wrapped) = audio_parts();
    let server = CountingServer::start(|base| {
        vec![
            (
                "/station.pls".to_owned(),
                String::new(),
                format!("[playlist]\r\nNumberOfEntries=1\r\nFile1={base}/audio\r\n").into_bytes(),
            ),
            (
                "/audio".to_owned(),
                "icy-metaint: 8192\r\n".to_owned(),
                wrapped,
            ),
        ]
    });

    let mut station = stream::open(&server.url("/station.pls"), wrapper_timeouts())
        .expect("the wrapper resolves to real audio");
    let samples = drain(&mut station);

    assert_eq!(
        samples,
        reference(&plain),
        "the resolved stream decodes exactly the audio"
    );
    assert_eq!(
        station.title().as_deref(),
        Some(STREAM_TITLE),
        "the title arrives"
    );
    server.wait_for(2);
    assert_eq!(
        server.paths(),
        vec!["/station.pls".to_owned(), "/audio".to_owned()],
        "the resolved URL is requested exactly once, after the wrapper"
    );
}

#[test]
fn an_m3u_wrapper_resolves_to_its_stream_and_plays() {
    let (plain, wrapped) = audio_parts();
    let server = CountingServer::start(|base| {
        vec![
            (
                "/station.m3u".to_owned(),
                String::new(),
                format!("#EXTM3U\r\n#EXTINF:-1,Fela Kuti\r\n{base}/audio\r\n").into_bytes(),
            ),
            (
                "/audio".to_owned(),
                "icy-metaint: 8192\r\n".to_owned(),
                wrapped,
            ),
        ]
    });

    let mut station = stream::open(&server.url("/station.m3u"), wrapper_timeouts())
        .expect("the wrapper resolves to real audio");
    let samples = drain(&mut station);

    assert_eq!(
        samples,
        reference(&plain),
        "the resolved stream decodes exactly the audio"
    );
    assert_eq!(
        station.title().as_deref(),
        Some(STREAM_TITLE),
        "the title arrives"
    );
    server.wait_for(2);
    assert_eq!(
        server.paths(),
        vec!["/station.m3u".to_owned(), "/audio".to_owned()],
        "the resolved URL is requested exactly once, after the wrapper"
    );
}

#[test]
fn an_hls_manifest_fails_by_name_without_fetching_a_segment() {
    let server = CountingServer::start(|base| {
        vec![
            (
                "/stream.m3u8".to_owned(),
                String::new(),
                format!("#EXTM3U\r\n#EXT-X-STREAM-INF:BANDWIDTH=128000\r\n{base}/seg0.ts\r\n")
                    .into_bytes(),
            ),
            // Registered so a stray fetch answers 200 instead of 404 —
            // only the recorded paths can prove it never happens.
            ("/seg0.ts".to_owned(), String::new(), vec![0_u8; 64]),
        ]
    });

    let error = stream::open(&server.url("/stream.m3u8"), wrapper_timeouts())
        .expect_err("an HLS manifest cannot be played by this decoder");

    assert!(matches!(error, StreamError::Hls), "got: {error:?}");
    assert!(
        error.to_string().contains("HLS"),
        "the error names HLS: {error}"
    );
    CountingServer::settle();
    assert_eq!(
        server.paths(),
        vec!["/stream.m3u8".to_owned()],
        "the .ts segment must never be fetched"
    );
}

#[test]
fn a_wrapper_pointing_at_another_wrapper_is_a_bounded_error() {
    let start = Instant::now();
    let server = CountingServer::start(|base| {
        vec![
            (
                "/one.pls".to_owned(),
                String::new(),
                format!("[playlist]\r\nFile1={base}/two.m3u\r\n").into_bytes(),
            ),
            (
                "/two.m3u".to_owned(),
                String::new(),
                format!("#EXTM3U\r\n{base}/audio\r\n").into_bytes(),
            ),
            (
                "/audio".to_owned(),
                "icy-metaint: 8192\r\n".to_owned(),
                audio_parts().1,
            ),
        ]
    });

    let error = stream::open(&server.url("/one.pls"), wrapper_timeouts())
        .expect_err("a wrapper of a wrapper is an error, not an infinite loop");
    let elapsed = start.elapsed();

    let StreamError::NotAudio { detail } = error else {
        panic!("expected NotAudio, got: {error:?}");
    };
    assert!(
        detail.contains("another wrapper"),
        "the error says so: {detail}"
    );
    assert!(
        elapsed < HANG_BOUND,
        "one level only: the open took {elapsed:?}"
    );
    CountingServer::settle();
    assert_eq!(
        server.paths(),
        vec!["/one.pls".to_owned(), "/two.m3u".to_owned()],
        "the third hop must never be fetched"
    );
}

#[test]
fn a_wrapper_with_an_unparsable_body_is_not_audio_never_a_panic() {
    let server = CountingServer::start(|_| {
        vec![(
            "/junk".to_owned(),
            String::new(),
            b"this is neither audio nor a wrapper playlist\r\n".to_vec(),
        )]
    });

    let error =
        stream::open(&server.url("/junk"), wrapper_timeouts()).expect_err("junk is not audio");
    assert!(
        matches!(error, StreamError::NotAudio { .. }),
        "got: {error:?}"
    );
}

#[test]
fn a_wrapper_with_no_playable_entry_maps_to_a_named_error() {
    let server = CountingServer::start(|_| {
        vec![(
            "/empty.pls".to_owned(),
            String::new(),
            b"[playlist]\r\nNumberOfEntries=0\r\n".to_vec(),
        )]
    });

    let error = stream::open(&server.url("/empty.pls"), wrapper_timeouts())
        .expect_err("a wrapper with no playable entry is an error");
    let StreamError::NotAudio { detail } = error else {
        panic!("expected NotAudio, got: {error:?}");
    };
    assert!(
        detail.contains("no playable"),
        "the mapped reason is named: {detail}"
    );
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
    let head = ok_head("icy-metaint: 8192\r\n", wrapped.len() + 4096);
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

    assert!(
        outcome.is_err(),
        "a stalled body must end in an error, not a clean end"
    );
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

// ------------------------------------------------------------------ expansion (cliamp S1)

/// A m3u channel of songs, the way the live service lists them: an
/// `#EXTINF` line naming `Artist - Title` and one entry per song, absolute
/// and relative mixed, plus a comment the parser must skip.
fn channel_m3u(base: &str) -> Vec<u8> {
    format!(
        "#EXTM3U\r\n\
         #EXTINF:-1,Alice - First\r\n\
         {base}/channel/songs/a\r\n\
         #EXTINF:-1,Bob - Second\r\n\
         /channel/songs/b\r\n\
         #EXTINF:-1,Cara - Third\r\n\
         songs/c\r\n"
    )
    .into_bytes()
}

#[test]
fn a_multi_entry_wrapper_expands_to_its_songs_in_wrapper_order() {
    let server = CountingServer::start(|base| {
        vec![
            (
                "/channel.m3u".to_owned(),
                String::from("Content-Type: audio/x-mpegurl\r\n"),
                channel_m3u(base),
            ),
            ("/channel/songs/a".to_owned(), String::new(), b"a".to_vec()),
            ("/channel/songs/b".to_owned(), String::new(), b"b".to_vec()),
            ("/songs/c".to_owned(), String::new(), b"c".to_vec()),
        ]
    });

    let expansion =
        stream::expand(&server.url("/channel.m3u"), wrapper_timeouts()).expect("it expands");

    let base = server.url("");
    assert_eq!(
        expansion,
        stream::Expansion::Songs(vec![
            format!("{base}/channel/songs/a"),
            format!("{base}/channel/songs/b"),
            // A directory-relative entry joins the wrapper's own directory:
            // the wrapper is at the root here, so `songs/c` sits beside it.
            format!("{base}/songs/c"),
        ]),
        "one absolute URL per entry, in the wrapper's order, relatives resolved"
    );
    // Expanding decides from the wrapper alone: no song is fetched here —
    // the cache fetches a song when the producer reaches it.
    CountingServer::settle();
    assert_eq!(
        server.paths(),
        vec!["/channel.m3u".to_owned()],
        "only the wrapper was fetched"
    );
}

#[test]
fn a_pls_with_several_entries_expands_to_them_in_order() {
    let server = CountingServer::start(|base| {
        vec![(
            "/channel.pls".to_owned(),
            String::new(),
            format!(
                "[playlist]\r\nFile1={base}/channel/songs/b\r\nFile2={base}/channel/songs/a\r\n"
            )
            .into_bytes(),
        )]
    });

    let expansion =
        stream::expand(&server.url("/channel.pls"), wrapper_timeouts()).expect("it expands");
    let base = server.url("");
    assert_eq!(
        expansion,
        stream::Expansion::Songs(vec![
            format!("{base}/channel/songs/b"),
            format!("{base}/channel/songs/a"),
        ]),
        "a PLS's entries sort by their File<n> number"
    );
}

#[test]
fn a_one_entry_wrapper_plays_live() {
    let server = CountingServer::start(|base| {
        vec![(
            "/one.m3u".to_owned(),
            String::new(),
            format!("#EXTM3U\r\n{base}/stream\r\n").into_bytes(),
        )]
    });

    let expansion =
        stream::expand(&server.url("/one.m3u"), wrapper_timeouts()).expect("one entry is live");
    assert_eq!(
        expansion,
        stream::Expansion::Live,
        "a wrapper with one entry is a station, not a song list (cliamp S1)"
    );
}

#[test]
fn a_plain_stream_url_is_not_a_song_list() {
    let plain = std::fs::read(fixture()).expect("the fixture is readable");
    let (url, _requests) = served(ok_head("", plain.len()), plain);

    let expansion = stream::expand(&url, wrapper_timeouts()).expect("audio is live");
    assert_eq!(expansion, stream::Expansion::Live);
}

#[test]
fn a_wrapper_with_no_song_entry_is_refused_with_its_reason() {
    let server = CountingServer::start(|_| {
        vec![(
            "/empty.m3u".to_owned(),
            String::new(),
            b"#EXTM3U\r\n#EXTINF:-1,named but never listed\r\n".to_vec(),
        )]
    });

    let error = stream::expand(&server.url("/empty.m3u"), wrapper_timeouts())
        .expect_err("nothing to open as songs and nothing to play live");
    assert!(
        error.to_string().contains("no song entry"),
        "the refusal names the reason, got: {error}"
    );
}

#[test]
fn an_unreachable_channel_is_refused_with_its_reason() {
    let url = served_silently();
    let error = stream::expand(&url, wrapper_timeouts())
        .expect_err("a station that never answers is a refusal, not a hang");
    assert!(
        matches!(error, StreamError::Unreachable { .. }),
        "got: {error}"
    );
}

#[test]
fn a_channel_that_answers_an_error_status_is_refused() {
    let server = CountingServer::start(|_| Vec::new()); // every route 404s

    let error =
        stream::expand(&server.url("/gone.m3u"), wrapper_timeouts()).expect_err("404 is a refusal");
    assert_eq!(error, StreamError::BadStatus { status: 404 });
}

#[test]
fn an_hls_manifest_is_refused_as_its_own_thing() {
    let server = CountingServer::start(|_| {
        vec![(
            "/live.m3u8".to_owned(),
            String::new(),
            b"#EXTM3U\r\n#EXT-X-TARGETDURATION:10\r\n#EXTINF:10,\r\nseg-1.ts\r\n".to_vec(),
        )]
    });

    let error =
        stream::expand(&server.url("/live.m3u8"), wrapper_timeouts()).expect_err("HLS is refused");
    assert_eq!(error, StreamError::Hls);
}

#[test]
fn a_wrapper_larger_than_the_inspection_cap_still_expands_whole() {
    // The stream open inspects at most 16 KiB before deciding; a channel
    // with enough songs to pass that cap must still expand completely —
    // no entry cut in half by the inspection.
    let padding = "#EXTM3U\r\n".to_owned()
        + &"# a comment line padding the wrapper past the inspection cap\r\n".repeat(300);
    let body = padding
        + "#EXTINF:-1,Alice - Late\r\n/channel/songs/late\r\n#EXTINF:-1,Bob - Later\r\n/channel/songs/later\r\n";
    assert!(
        body.len() > 16 * 1024,
        "the wrapper must actually pass the 16 KiB cap"
    );
    let server = CountingServer::start(move |_| {
        vec![("/big.m3u".to_owned(), String::new(), body.into_bytes())]
    });

    let expansion =
        stream::expand(&server.url("/big.m3u"), wrapper_timeouts()).expect("it expands");
    let base = server.url("");
    assert_eq!(
        expansion,
        stream::Expansion::Songs(vec![
            format!("{base}/channel/songs/late"),
            format!("{base}/channel/songs/later"),
        ]),
        "the songs past the cap are resolved whole, in order"
    );
}

#[test]
fn a_directory_relative_entry_joins_the_wrappers_own_directory() {
    // The wrapper sits in a directory of its own (`/lofi/tracks.m3u`), so a
    // relative entry resolves against that directory — `../` climbs out of
    // it, `./` and repeated slashes do not move it.
    let server = CountingServer::start(|base| {
        let body = format!(
            "#EXTM3U\r\n{base}/shared/absolute\r\ntracks/late\r\n./tracks/dotted\r\n../shared/up\r\n"
        );
        vec![(
            "/lofi/tracks.m3u".to_owned(),
            String::new(),
            body.into_bytes(),
        )]
    });

    let expansion =
        stream::expand(&server.url("/lofi/tracks.m3u"), wrapper_timeouts()).expect("it expands");
    let base = server.url("");
    assert_eq!(
        expansion,
        stream::Expansion::Songs(vec![
            format!("{base}/shared/absolute"),
            format!("{base}/lofi/tracks/late"),
            format!("{base}/lofi/tracks/dotted"),
            format!("{base}/shared/up"),
        ]),
        "relative entries resolve against the wrapper's directory, `..` climbs"
    );
}
