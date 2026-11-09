//! `velo --radio-test <url>` (issue #13): the one-shot station probe.
//!
//! The report is a diagnosis, not playback: the URL as given, the wrapper
//! resolution, the redirect chain, the headers that matter (`content-type`,
//! `icy-metaint`), the codec the build decoded and the result of decoding the
//! first seconds of audio. No test touches the real network: every case
//! speaks to a `TcpListener` on `127.0.0.1:0` that answers with canned
//! bytes, exactly like `tests/subsonic.rs` and `tests/radio_stream.rs`.

use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;

/// The ICY title the fake stations embed.
const STREAM_TITLE: &str = "Fela Kuti - Zombie";

/// The audio the fake stations serve: the same MP3 fixture the stream tests
/// use, so the decode probe has real frames to read.
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

/// Wraps `audio` in ICY metadata: every `interval` audio bytes one
/// `StreamTitle` block (first) or an empty `L = 0` block (the rest).
fn icy_wrap(audio: &[u8], interval: usize) -> Vec<u8> {
    let title = format!("StreamTitle='{STREAM_TITLE}';");
    let mut padded = title.into_bytes();
    while padded.len() % 16 != 0 {
        padded.push(0);
    }
    let title_block = metadata_block(&padded);
    let empty_block = metadata_block(&[]);

    let mut wrapped = Vec::new();
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

/// A canned answer: head and body written on one connection.
struct Answer {
    head: String,
    body: Vec<u8>,
}

fn answer(head: String, body: Vec<u8>) -> Answer {
    Answer { head, body }
}

/// Serves the canned answers in order, one per connection, then stops. The
/// accepted socket is set blocking again (the macOS flake from the stream
/// tests: an accepted socket inherits `O_NONBLOCK` and a non-blocking read
/// answers before the request arrives).
fn serve(listener: TcpListener, answers: Vec<Answer>) {
    std::thread::spawn(move || {
        for answer in answers {
            let Ok((mut connection, _)) = listener.accept() else {
                return;
            };
            let _ = connection.set_nonblocking(false);
            let mut request = vec![0_u8; 4096];
            let _ = connection.read(&mut request);
            let _ = connection.write_all(answer.head.as_bytes());
            let _ = connection.write_all(&answer.body);
        }
    });
}

/// Runs the built binary against the listener's URL.
fn run_radio_test(url: &str, config_home: &std::path::Path) -> (Option<i32>, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_velo"))
        .arg("--radio-test")
        .arg(url)
        .env("XDG_CONFIG_HOME", config_home.as_os_str())
        .output()
        .expect("the binary runs");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// A scratch config home, so no test reads or writes the user's real
/// configuration. Removed by the caller.
fn config_home(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("velo-radio-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("velo")).expect("the config home is created");
    dir
}

// ------------------------------------------------------------------ playing

#[test]
fn a_playable_stream_reports_the_url_the_headers_the_codec_and_the_decode() {
    let dir = config_home("playable");
    let plain = std::fs::read(fixture()).expect("the fixture is readable");
    let wrapped = icy_wrap(&plain, 8192);
    let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
    let url = format!("http://{}", listener.local_addr().expect("a local address"));
    serve(
        listener,
        vec![answer(
            ok_head("icy-metaint: 8192\r\n", wrapped.len()),
            wrapped,
        )],
    );

    let (code, stdout, stderr) = run_radio_test(&url, &dir);
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(
        code,
        Some(0),
        "exit 0\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    // The report's lines, in the order the spec asks for them.
    let order: Vec<usize> = [
        "url: ",
        "wrapper: none",
        "redirects: none",
        "content-type: audio/mpeg",
        "icy-metaint: 8192",
        "codec: mp3",
        "decode: ",
    ]
    .iter()
    .map(|marker| {
        stdout
            .find(marker)
            .unwrap_or_else(|| panic!("{marker} missing from:\n{stdout}"))
    })
    .collect();
    let mut sorted = order.clone();
    sorted.sort_unstable();
    assert_eq!(
        order, sorted,
        "the report lines are out of order:\n{stdout}"
    );
    assert!(stderr.is_empty(), "nothing on stderr: {stderr}");
}

#[test]
fn a_wrapper_playlist_resolves_to_the_url_it_really_points_at() {
    let dir = config_home("wrapper");
    let plain = std::fs::read(fixture()).expect("the fixture is readable");
    let wrapped = icy_wrap(&plain, 8192);
    let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
    let address = listener.local_addr().expect("a local address");
    let audio_url = format!("http://{address}/audio");
    let playlist = format!("#EXTM3U\n{audio_url}\n");
    serve(
        listener,
        vec![
            answer(
                "HTTP/1.1 200 OK\r\nContent-Type: audio/x-mpegurl\r\nContent-Length: ".to_owned()
                    + &playlist.len().to_string()
                    + "\r\nConnection: close\r\n\r\n",
                playlist.into_bytes(),
            ),
            answer(ok_head("", wrapped.len()), wrapped),
        ],
    );
    let url = format!("http://{address}/stream.m3u");

    let (code, stdout, stderr) = run_radio_test(&url, &dir);
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(
        code,
        Some(0),
        "exit 0\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains(&format!("wrapper: {audio_url}")),
        "the resolved URL is in the report:\n{stdout}"
    );
    assert!(stdout.contains("decode: "), "{stdout}");
    assert!(stderr.is_empty(), "nothing on stderr: {stderr}");
}

#[test]
fn a_redirect_hop_is_reported() {
    let dir = config_home("redirect");
    let plain = std::fs::read(fixture()).expect("the fixture is readable");
    let wrapped = icy_wrap(&plain, 8192);
    let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
    let address = listener.local_addr().expect("a local address");
    let target = format!("http://{address}/audio");
    serve(
        listener,
        vec![
            answer(
                format!(
                    "HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                ),
                Vec::new(),
            ),
            answer(ok_head("", wrapped.len()), wrapped),
        ],
    );
    let url = format!("http://{address}/stream");

    let (code, stdout, stderr) = run_radio_test(&url, &dir);
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(
        code,
        Some(0),
        "exit 0\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains(&format!("redirect: {target}")),
        "the redirect hop is in the report:\n{stdout}"
    );
    assert!(stdout.contains("decode: "), "{stdout}");
    assert!(stderr.is_empty(), "nothing on stderr: {stderr}");
}

// ------------------------------------------------------------------ failing

#[test]
fn a_404_answers_exit_one_with_the_reason() {
    let dir = config_home("not-found");
    let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
    let url = format!("http://{}", listener.local_addr().expect("a local address"));
    serve(
        listener,
        vec![answer(
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
            Vec::new(),
        )],
    );

    let (code, stdout, stderr) = run_radio_test(&url, &dir);
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(
        code,
        Some(1),
        "exit 1\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stderr.contains("velo: "), "{stderr}");
    assert!(stderr.contains("404"), "{stderr}");
    assert!(stdout.is_empty(), "no report on failure: {stdout}");
}

#[test]
fn radio_test_without_a_url_is_a_usage_error() {
    let dir = config_home("no-url");

    let output = Command::new(env!("CARGO_BIN_EXE_velo"))
        .arg("--radio-test")
        .env("XDG_CONFIG_HOME", dir.as_os_str())
        .output()
        .expect("the binary runs");
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(output.status.code(), Some(2), "exit 2");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("radio-test"), "{stderr}");
    assert!(stderr.contains("a value is required"), "{stderr}");
}
