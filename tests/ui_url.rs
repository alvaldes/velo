//! The URL prompt (issue #42): `u` takes a one-off `http`/`https` URL,
//! resolved the way a configured station is — a plain stream (or a wrapper
//! with a single entry) plays live as a station, a wrapper listing several
//! songs queues them as one finite list of file rows — and every URL that
//! cannot play is one status line naming the reason, never a silent
//! nothing. Nothing is persisted: no station in `radio.toml`.
//!
//! The network cases speak to a `TcpListener` on `127.0.0.1:0`, the way
//! `tests/radio_stream.rs` does; no test touches the real network.

use std::path::PathBuf;

use velo::audio::output::Entry;
use velo::ui::crossterm::event::{KeyCode, KeyEvent};
use velo::ui::ratatui::Terminal;
use velo::ui::ratatui::backend::TestBackend;
use velo::ui::{Action, App};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::from(code)
}

/// One key press, its action applied.
fn press(app: &mut App<'_>, code: KeyCode) {
    let action = app.on_key(key(code));
    app.apply(action);
}

/// Types every character into whatever owns printable keys right now.
fn type_text(app: &mut App<'_>, text: &str) {
    for character in text.chars() {
        app.on_key(key(KeyCode::Char(character)));
    }
}

/// One loopback server with canned routes: one thread, every connection
/// answered with its route's body as `audio/mpeg`.
mod url_server {
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    pub struct Server {
        base: String,
    }

    impl Server {
        /// Starts the server; the accept loop answers every connection with
        /// the route's body — sniffs by content alone, so the type only has
        /// to carry the bytes.
        pub fn start(routes: &[(&'static str, &'static str)]) -> Server {
            let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
            let base = format!("http://{}", listener.local_addr().expect("a local address"));
            let routes: HashMap<String, &str> = routes
                .iter()
                .map(|(path, body)| ((*path).to_owned(), *body))
                .collect();
            std::thread::spawn(move || {
                for mut connection in listener.incoming().flatten() {
                    let mut request = Vec::new();
                    let mut chunk = [0_u8; 1024];
                    loop {
                        let read = connection.read(&mut chunk).unwrap_or(0);
                        if read == 0 {
                            break;
                        }
                        request.extend_from_slice(&chunk[..read]);
                        if request.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    let request = String::from_utf8_lossy(&request).into_owned();
                    let path = request
                        .lines()
                        .next()
                        .and_then(|line| line.split_whitespace().nth(1))
                        .unwrap_or_default()
                        .to_owned();
                    if let Some(body) = routes.get(&path) {
                        let head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\n\
                             Content-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = connection.write_all(head.as_bytes());
                        let _ = connection.write_all(body.as_bytes());
                    } else {
                        let _ = connection.write_all(
                            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\
                              Connection: close\r\n\r\n",
                        );
                    }
                    let _ = connection.flush();
                }
            });
            Server { base }
        }

        /// The URL of one route on the server.
        pub fn url(&self, path: &str) -> String {
            format!("{}{}", self.base, path)
        }
    }
}

// ---- the prompt itself: open, edit, cancel ---------------------------------

#[test]
fn u_opens_a_prompt_that_types_backspaces_and_cancels() {
    let mut app = App::new(Vec::new());
    assert!(!app.url_prompt_open());

    press(&mut app, KeyCode::Char('u'));
    assert!(app.url_prompt_open(), "u opens the prompt");
    assert_eq!(app.url_prompt_input(), "");

    type_text(&mut app, "https://radio.example/stream");
    assert_eq!(app.url_prompt_input(), "https://radio.example/stream");
    press(&mut app, KeyCode::Backspace);
    assert_eq!(
        app.url_prompt_input(),
        "https://radio.example/strea",
        "backspace deletes, the way the seek prompt does"
    );

    press(&mut app, KeyCode::Esc);
    assert!(!app.url_prompt_open(), "esc closes the prompt");
    assert!(app.queue().is_empty(), "a cancelled prompt queues nothing");
}

#[test]
fn an_answer_that_is_not_an_http_url_stays_in_the_card_with_its_reason() {
    let mut app = App::new(Vec::new());
    press(&mut app, KeyCode::Char('u'));
    type_text(&mut app, "ftp://radio.example/stream");

    let action = app.on_key(key(KeyCode::Enter));
    assert_eq!(action, Action::None, "a refused answer queues nothing");
    assert!(
        app.url_prompt_open(),
        "a refused answer leaves the prompt open"
    );
    let error = app.url_prompt_error().unwrap_or_default();
    assert!(error.contains("http"), "{error}");
    assert_eq!(app.queue(), [] as [Entry; 0]);
}

// ---- the three outcomes ----------------------------------------------------

#[test]
fn a_plain_stream_url_queues_a_live_station_and_plays_it() {
    let server = url_server::Server::start(&[("/stream", "ID3\x03audio-bytes")]);
    let mut app = App::new(Vec::new());
    press(&mut app, KeyCode::Char('u'));
    type_text(&mut app, &server.url("/stream"));
    assert!(app.url_prompt_open(), "the prompt still owns the keys");

    let action = app.on_key(key(KeyCode::Enter));
    assert_eq!(action, Action::PlayQueue { from: 0 }, "the stream plays");
    assert!(
        !app.url_prompt_open(),
        "an accepted answer closes the prompt"
    );
    assert_eq!(
        app.queue(),
        vec![Entry::Station {
            url: server.url("/stream"),
            name: server.url("/stream"),
        }],
        "a plain stream plays live, exactly a configured station does"
    );
}

#[test]
fn a_url_that_lists_songs_queues_them_as_file_rows_with_the_url_as_the_path() {
    let server = url_server::Server::start(&[(
        "/songs.m3u",
        "#EXTM3U\n#EXTINF:180,One - First\nhttps://wrap.example/a.mp3\n\
         #EXTINF:200,Two - Second\nhttps://wrap.example/b.mp3\n",
    )]);
    let mut app = App::new(Vec::new());
    press(&mut app, KeyCode::Char('u'));
    type_text(&mut app, &server.url("/songs.m3u"));

    let action = app.on_key(key(KeyCode::Enter));
    assert_eq!(action, Action::PlayQueue { from: 0 });
    assert_eq!(
        app.queue(),
        vec![
            Entry::File(PathBuf::from("https://wrap.example/a.mp3")),
            Entry::File(PathBuf::from("https://wrap.example/b.mp3")),
        ],
        "one row per song, in the wrapper's order, the URL the row's path"
    );
}

#[test]
fn an_hls_url_is_one_refusal_line_and_nothing_is_queued() {
    let server = url_server::Server::start(&[(
        "/live.m3u8",
        "#EXTM3U\n#EXT-X-TARGETDURATION:10\n#EXTINF:10,\nseg-1.ts\n",
    )]);
    let mut app = App::new(Vec::new());
    press(&mut app, KeyCode::Char('u'));
    type_text(&mut app, &server.url("/live.m3u8"));

    let action = app.on_key(key(KeyCode::Enter));
    assert_eq!(action, Action::None, "nothing was queued");
    assert_eq!(app.queue(), [] as [Entry; 0]);
    let status = app.status().to_owned();
    assert!(status.contains("HLS"), "{status}");
    assert!(status.contains(&server.url("/live.m3u8")), "{status}");
}

#[test]
fn an_unreachable_url_is_one_refusal_line_and_nothing_is_queued() {
    // A port that was free a moment ago and is closed now: connecting to it
    // is refused at once, never a timeout.
    let unchecked = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a local port");
        let address = listener.local_addr().expect("a local address");
        drop(listener);
        format!("http://{address}/nowhere")
    };
    let mut app = App::new(Vec::new());
    press(&mut app, KeyCode::Char('u'));
    type_text(&mut app, &unchecked);

    let action = app.on_key(key(KeyCode::Enter));
    assert_eq!(action, Action::None, "nothing was queued");
    assert_eq!(app.queue(), [] as [Entry; 0]);
    let status = app.status().to_owned();
    assert!(status.contains("could not be reached"), "{status}");
    assert!(status.contains(&unchecked), "{status}");
}

// ---- the key it took -------------------------------------------------------

#[test]
fn u_no_longer_reindexes_the_prompt_took_its_place() {
    let mut app = App::new(Vec::new());
    let action = app.on_key(key(KeyCode::Char('u')));
    assert_eq!(
        action,
        Action::None,
        "u opens a prompt, it no longer reindexes"
    );
    assert!(app.url_prompt_open());
    assert_eq!(app.take_library_root(), None, "nothing was handed over");
}

#[test]
fn the_help_card_names_the_url_prompt_and_the_index_refresh() {
    let mut app = App::new(Vec::new());
    press(&mut app, KeyCode::Char('?'));

    let mut terminal = Terminal::new(TestBackend::new(80, 50)).expect("a test terminal");
    terminal
        .draw(|frame| app.draw(frame))
        .expect("the frame draws");
    let screen: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol().to_owned())
        .collect();
    assert!(screen.contains("refresh the library index"), "{screen}");
    assert!(screen.contains("open a URL"), "{screen}");
}
