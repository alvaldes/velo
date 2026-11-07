//! The Subsonic client and its authentication (T12a, S14).
//!
//! No test here touches the network: every request goes through a canned
//! [`Transport`], and the binary-level tests speak to a `TcpListener` on
//! `127.0.0.1` that answers with the same canned bytes. Environment values
//! and the home directory are always passed as arguments, so no test reads
//! or writes the user's real configuration.

use std::ffi::OsStr;
use std::fs;
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::Parser as _;
use velo::config::Cli;
use velo::subsonic::Error;
use velo::subsonic::client::{Auth, AuthKind, Client, Transport, build_request_url, token};
use velo::subsonic::credentials::resolve;

/// A transport that answers from a table of canned bodies: the first entry
/// whose marker is contained in the URL wins.
struct Canned {
    responses: Vec<(String, Result<Vec<u8>, Error>)>,
}

impl Canned {
    fn new(responses: Vec<(&str, Result<Vec<u8>, Error>)>) -> Self {
        Self {
            responses: responses
                .into_iter()
                .map(|(marker, body)| (marker.to_owned(), body))
                .collect(),
        }
    }
}

impl Transport for Canned {
    fn get(&self, url: &str) -> Result<Vec<u8>, Error> {
        for (marker, body) in &self.responses {
            if url.contains(marker.as_str()) {
                return body.clone();
            }
        }
        Err(Error::Unreachable {
            detail: format!("no canned response for {url}"),
        })
    }
}

/// A `subsonic-response` success envelope, with `extra` spliced into the
/// inner object (for the extensions list).
fn ok_envelope(extra: &str) -> Vec<u8> {
    format!(
        r#"{{"subsonic-response":{{"status":"ok","version":"1.16.1","type":"Navidrome","serverVersion":"0.55.2","openSubsonic":true{extra}}}}}"#
    )
    .into_bytes()
}

fn error_envelope(code: u32, message: &str) -> Vec<u8> {
    format!(
        r#"{{"subsonic-response":{{"status":"failed","version":"1.16.1","error":{{"code":{code},"message":"{message}"}}}}}}"#
    )
    .into_bytes()
}

fn xml_envelope() -> Vec<u8> {
    r#"<?xml version="1.0" encoding="UTF-8"?><subsonic-response xmlns="http://subsonic.org/restapi" status="ok" version="1.15.0"></subsonic-response>"#
        .as_bytes()
        .to_vec()
}

// ---------------------------------------------------------------- token/salt

#[test]
fn the_token_is_md5_of_password_plus_salt() {
    // The example from the Subsonic API documentation: password "sesame",
    // salt "c19b2d" → token 26719a1196d2a940705a59634eb18eab.
    assert_eq!(
        token("sesame", "c19b2d"),
        "26719a1196d2a940705a59634eb18eab"
    );
}

#[test]
fn a_different_salt_changes_the_token() {
    let first = token("sesame", "c19b2d");
    let second = token("sesame", "c19b2e");
    assert_ne!(first, second);
    // And the same password and salt always produce the same token.
    assert_eq!(token("sesame", "c19b2d"), first);
}

// ------------------------------------------------------------- URL building

/// Every pair of the URL's query string.
fn query_pairs(url: &str) -> Vec<String> {
    let query = url.split_once('?').map_or("", |(_, query)| query);
    query.split('&').map(str::to_owned).collect()
}

#[test]
fn every_request_carries_the_mandatory_parameters() {
    let url = build_request_url(
        "http://music.example.com",
        "admin",
        &Auth::Token {
            password: "sesame".to_owned(),
        },
        "c19b2d",
        "ping",
        &[],
    );
    assert!(
        url.starts_with("http://music.example.com/rest/ping?"),
        "{url}"
    );
    let pairs = query_pairs(&url);
    for piece in [
        "u=admin",
        "v=1.16.1",
        "c=velo",
        "f=json",
        "t=26719a1196d2a940705a59634eb18eab",
        "s=c19b2d",
    ] {
        assert!(pairs.contains(&piece.to_owned()), "{piece} missing: {url}");
    }
    assert!(
        pairs.iter().all(|pair| !pair.starts_with("apiKey")),
        "{url}"
    );
}

#[test]
fn an_api_key_request_carries_the_key_and_never_a_token() {
    let url = build_request_url(
        "http://music.example.com",
        "admin",
        &Auth::ApiKey {
            key: "SECRET-KEY".to_owned(),
        },
        "c19b2d",
        "ping",
        &[],
    );
    let pairs = query_pairs(&url);
    assert!(pairs.contains(&"apiKey=SECRET-KEY".to_owned()), "{url}");
    assert!(pairs.iter().all(|pair| !pair.starts_with("t=")), "{url}");
    assert!(pairs.iter().all(|pair| !pair.starts_with("s=")), "{url}");
}

#[test]
fn a_trailing_slash_in_the_server_url_is_harmless() {
    let url = build_request_url(
        "http://music.example.com/",
        "admin",
        &Auth::Token {
            password: "sesame".to_owned(),
        },
        "c19b2d",
        "ping",
        &[],
    );
    assert!(
        url.starts_with("http://music.example.com/rest/ping?"),
        "{url}"
    );
}

#[test]
fn an_existing_query_string_is_joined_correctly() {
    let url = build_request_url(
        "http://music.example.com/sub?x=1",
        "admin",
        &Auth::Token {
            password: "sesame".to_owned(),
        },
        "c19b2d",
        "ping",
        &[],
    );
    assert!(
        url.starts_with("http://music.example.com/sub/rest/ping?x=1&"),
        "{url}"
    );
    assert!(query_pairs(&url).contains(&"f=json".to_owned()), "{url}");
}

#[test]
fn query_values_are_percent_encoded() {
    let url = build_request_url(
        "http://h",
        "a b&c",
        &Auth::ApiKey {
            key: "k&/=".to_owned(),
        },
        "s",
        "ping",
        &[("extra", "v l&u=e")],
    );
    let pairs = query_pairs(&url);
    assert!(pairs.contains(&"u=a%20b%26c".to_owned()), "{url}");
    assert!(pairs.contains(&"apiKey=k%26%2F%3D".to_owned()), "{url}");
    assert!(pairs.contains(&"extra=v%20l%26u%3De".to_owned()), "{url}");
}

// ------------------------------------------------------------- error mapping

#[test]
fn every_named_error_code_produces_its_own_sentence() {
    let cases: [(u32, &str); 6] = [
        (0, "the server failed"),
        (10, "the server could not handle the request"),
        (30, "too old"),
        (40, "credentials were rejected"),
        (50, "not authorised"),
        (70, "not found"),
    ];
    for (code, expected) in cases {
        let error = Error::Server {
            code,
            message: "detail from the server".to_owned(),
        };
        let text = error.to_string();
        assert!(text.contains(expected), "code {code}: {text}");
        assert!(
            text.contains("detail from the server"),
            "code {code} must carry the server's message: {text}"
        );
    }
}

#[test]
fn an_unnamed_error_code_still_carries_the_code_and_the_message() {
    let error = Error::Server {
        code: 20,
        message: "Incompatible client".to_owned(),
    };
    let text = error.to_string();
    assert!(text.contains("20"), "{text}");
    assert!(text.contains("Incompatible client"), "{text}");
}

#[test]
fn an_unreachable_server_is_not_reported_as_a_credential_problem() {
    // Nothing listens on port 1; the transport-level error must stay
    // "could not be reached", never a password complaint.
    let error = Error::Unreachable {
        detail: "connection refused".to_owned(),
    };
    let text = error.to_string();
    assert!(text.contains("could not be reached"), "{text}");
    assert!(!text.contains("credential"), "{text}");
    assert!(!text.to_lowercase().contains("password"), "{text}");
}

// ------------------------------------------------------- credential resolution

/// Makes `<temp>/<name>/velo`, like the config tests do, so no test ever
/// touches the user's real configuration.
fn temp_dir(name: &str) -> PathBuf {
    let dir = Path::new(&std::env::temp_dir())
        .join(format!("velo-subsonic-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("velo"))
        .unwrap_or_else(|error| panic!("cannot create {}: {error}", dir.display()));
    dir
}

fn write_key_file(dir: &Path, contents: &str) -> PathBuf {
    let path = dir.join("velo").join("subsonic.key");
    fs::write(&path, contents).unwrap();
    path
}

#[test]
fn the_config_api_key_beats_the_environment_beats_the_file() {
    let dir = temp_dir("order");
    write_key_file(&dir, "file-secret\n");

    let credentials = resolve(
        "http://music.example.com",
        "admin",
        Some("config-key"),
        Some(OsStr::new("env-secret")),
        Some(dir.as_os_str()),
        None,
    )
    .unwrap_or_else(|error| panic!("resolve failed: {error}"));
    assert_eq!(credentials.secret, "config-key");

    let credentials = resolve(
        "http://music.example.com",
        "admin",
        None,
        Some(OsStr::new("env-secret")),
        Some(dir.as_os_str()),
        None,
    )
    .unwrap_or_else(|error| panic!("resolve failed: {error}"));
    assert_eq!(credentials.secret, "env-secret");

    let credentials = resolve(
        "http://music.example.com",
        "admin",
        None,
        None,
        Some(dir.as_os_str()),
        None,
    )
    .unwrap_or_else(|error| panic!("resolve failed: {error}"));
    assert_eq!(credentials.secret, "file-secret");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_key_file_is_looked_up_under_xdg_then_home() {
    let xdg = temp_dir("xdg-key");
    write_key_file(&xdg, "xdg-secret\n");
    let credentials = resolve(
        "http://s",
        "admin",
        None,
        None,
        Some(xdg.as_os_str()),
        Some(OsStr::new("/home/elsewhere")),
    )
    .unwrap_or_else(|error| panic!("resolve failed: {error}"));
    assert_eq!(credentials.secret, "xdg-secret");
    let _ = fs::remove_dir_all(&xdg);

    let home = temp_dir("home-key");
    fs::create_dir_all(home.join(".config").join("velo")).unwrap();
    fs::write(
        home.join(".config").join("velo").join("subsonic.key"),
        "home-secret\n",
    )
    .unwrap();
    let credentials = resolve(
        "http://s",
        "admin",
        None,
        None,
        None,
        Some(home.as_os_str()),
    )
    .unwrap_or_else(|error| panic!("resolve failed: {error}"));
    assert_eq!(credentials.secret, "home-secret");
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_missing_key_file_is_a_clear_error_naming_the_path() {
    let dir = temp_dir("missing-key");
    let error = resolve("http://s", "admin", None, None, Some(dir.as_os_str()), None)
        .expect_err("no secret anywhere must fail");
    let text = error.to_string();
    let expected = dir.join("velo").join("subsonic.key").display().to_string();
    assert!(text.contains(&expected), "{text}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn an_empty_environment_value_is_no_secret_at_all() {
    let dir = temp_dir("empty-env");
    let error = resolve(
        "http://s",
        "admin",
        None,
        Some(OsStr::new("")),
        Some(dir.as_os_str()),
        None,
    )
    .expect_err("an empty env value must not become an empty secret");
    assert!(matches!(error, Error::Credentials(_)), "{error}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_world_readable_key_file_warns_with_the_path_and_the_mode() {
    let dir = temp_dir("wide-open-key");
    let path = write_key_file(&dir, "file-secret\n");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

    let credentials = resolve("http://s", "admin", None, None, Some(dir.as_os_str()), None)
        .unwrap_or_else(|error| panic!("resolve failed: {error}"));
    assert_eq!(credentials.warnings.len(), 1, "{:?}", credentials.warnings);
    let warning = &credentials.warnings[0];
    assert!(
        warning.contains(path.display().to_string().as_str()),
        "{warning}"
    );
    assert!(warning.contains("0644"), "{warning}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_private_key_file_produces_no_warning() {
    let dir = temp_dir("private-key");
    let path = write_key_file(&dir, "file-secret\n");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

    let credentials = resolve("http://s", "admin", None, None, Some(dir.as_os_str()), None)
        .unwrap_or_else(|error| panic!("resolve failed: {error}"));
    assert!(
        credentials.warnings.is_empty(),
        "{:?}",
        credentials.warnings
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_multiline_key_file_is_refused() {
    let dir = temp_dir("multiline-key");
    write_key_file(&dir, "line one\nline two\n");
    let error = resolve("http://s", "admin", None, None, Some(dir.as_os_str()), None)
        .expect_err("a multi-line key file must not be silently truncated");
    assert!(matches!(error, Error::Credentials(_)), "{error}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn resolution_normalizes_the_server_url() {
    let dir = temp_dir("normalize");
    write_key_file(&dir, "s\n");
    let credentials = resolve(
        "http://music.example.com/",
        "admin",
        None,
        None,
        Some(dir.as_os_str()),
        None,
    )
    .unwrap_or_else(|error| panic!("resolve failed: {error}"));
    assert_eq!(credentials.url, "http://music.example.com");
    let _ = fs::remove_dir_all(&dir);
}

// ------------------------------------------------------ the probe (the seam)

#[test]
fn a_server_that_only_answers_xml_is_reported_as_too_old() {
    let transport = Canned::new(vec![("t=", Ok(xml_envelope()))]);
    let error = Client::probe(transport, "http://music.example.com", "admin", "sesame")
        .expect_err("an XML-only server must fail");
    let text = error.to_string();
    assert!(text.contains("too old"), "{text}");
    assert!(text.contains("JSON"), "{text}");
    assert!(matches!(error, Error::TooOld { .. }), "{text}");
}

#[test]
fn a_probe_reports_the_server_the_extensions_and_the_auth_it_used() {
    let transport = Canned::new(vec![
        (
            "getOpenSubsonicExtensions",
            Ok(ok_envelope(
                r#","openSubsonicExtensions":[{"name":"apiKeyAuthentication","versions":[1]},{"name":"formPost","versions":[1]}]"#,
            )),
        ),
        ("apiKey=", Ok(ok_envelope(""))),
        ("t=", Ok(ok_envelope(""))),
    ]);

    let report = Client::probe(transport, "http://music.example.com", "admin", "sesame")
        .unwrap_or_else(|error| panic!("probe failed: {error}"));

    assert_eq!(report.server.server_type.as_deref(), Some("Navidrome"));
    assert_eq!(report.server.server_version.as_deref(), Some("0.55.2"));
    assert_eq!(report.server.api_version.as_deref(), Some("1.16.1"));
    assert!(report.server.open_subsonic);
    assert_eq!(
        report.server.extensions,
        vec!["apiKeyAuthentication".to_owned(), "formPost".to_owned()]
    );
    assert_eq!(report.auth, AuthKind::ApiKey);
}

#[test]
fn a_server_without_the_extensions_call_still_probes_with_token_auth() {
    let transport = Canned::new(vec![
        (
            "getOpenSubsonicExtensions",
            Err(Error::Unreachable {
                detail: "HTTP 404".to_owned(),
            }),
        ),
        ("t=", Ok(ok_envelope(""))),
    ]);

    let report = Client::probe(transport, "http://music.example.com", "admin", "sesame")
        .unwrap_or_else(|error| panic!("probe failed: {error}"));

    assert_eq!(report.auth, AuthKind::Token);
    assert_eq!(report.server.extensions, Vec::<String>::new());
}

#[test]
fn an_api_key_the_server_does_not_accept_falls_back_to_token_auth() {
    let transport = Canned::new(vec![
        (
            "getOpenSubsonicExtensions",
            Ok(ok_envelope(
                r#","openSubsonicExtensions":[{"name":"apiKeyAuthentication","versions":[1]}]"#,
            )),
        ),
        ("apiKey=", Ok(error_envelope(40, "Wrong key"))),
        ("t=", Ok(ok_envelope(""))),
    ]);

    let report = Client::probe(transport, "http://music.example.com", "admin", "sesame")
        .unwrap_or_else(|error| panic!("probe failed: {error}"));

    assert_eq!(report.auth, AuthKind::Token);
}

#[test]
fn a_probe_with_wrong_credentials_reports_the_server_sentence() {
    let transport = Canned::new(vec![("", Ok(error_envelope(40, "Wrong user or password")))]);
    let error = Client::probe(transport, "http://music.example.com", "admin", "sesame")
        .expect_err("wrong credentials must fail");
    let text = error.to_string();
    assert!(text.contains("credentials were rejected"), "{text}");
    assert!(text.contains("Wrong user or password"), "{text}");
}

#[test]
fn an_old_server_that_refuses_the_extensions_call_is_not_fatal() {
    let transport = Canned::new(vec![
        (
            "getOpenSubsonicExtensions",
            Ok(error_envelope(0, "Not implemented")),
        ),
        ("t=", Ok(ok_envelope(""))),
    ]);
    let report = Client::probe(transport, "http://music.example.com", "admin", "sesame")
        .unwrap_or_else(|error| panic!("probe failed: {error}"));
    assert_eq!(report.auth, AuthKind::Token);
    assert_eq!(report.server.extensions, Vec::<String>::new());
}

// ------------------------------------------------- the binary, end to end

/// Serves the given bodies in order to sequential HTTP requests from a local
/// listener. Every answer closes the connection, so each request of the
/// probe arrives as its own connection.
fn serve_bodies(listener: &TcpListener, bodies: &[Vec<u8>]) {
    for body in bodies {
        let (mut stream, _) = listener
            .accept()
            .unwrap_or_else(|error| panic!("no connection: {error}"));
        let mut request = [0_u8; 4096];
        // The request is one small GET; a single read is enough to receive it.
        let _ = stream.read(&mut request);
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).unwrap();
        stream.write_all(body).unwrap();
    }
}

fn subsonic_config_toml(url: &str) -> String {
    format!("[subsonic]\nurl = \"{url}\"\nuser = \"admin\"\napi_key = \"sesame\"\n")
}

#[test]
fn subsonic_test_prints_the_server_and_exits_zero_against_a_local_server() {
    let dir = temp_dir("binary-ok");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    fs::write(
        dir.join("velo").join("config.toml"),
        subsonic_config_toml(&format!("http://127.0.0.1:{port}")),
    )
    .unwrap();

    // The probe order: token ping, extensions, then the api-key ping.
    let bodies = vec![
        ok_envelope(""),
        ok_envelope(
            r#","openSubsonicExtensions":[{"name":"apiKeyAuthentication","versions":[1]}]"#,
        ),
        ok_envelope(""),
    ];
    let server = std::thread::spawn(move || serve_bodies(&listener, &bodies));

    let output = Command::new(env!("CARGO_BIN_EXE_velo"))
        .arg("--subsonic-test")
        .env("XDG_CONFIG_HOME", dir.as_os_str())
        .output()
        .unwrap();
    server.join().unwrap();

    assert!(
        output.status.success(),
        "exit {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Navidrome"), "{stdout}");
    assert!(stdout.contains("1.16.1"), "{stdout}");
    assert!(stdout.contains("apiKeyAuthentication"), "{stdout}");
    assert!(stdout.contains("api key"), "{stdout}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn subsonic_test_against_an_unreachable_server_exits_one_with_one_reason() {
    let dir = temp_dir("binary-unreachable");
    fs::write(
        dir.join("velo").join("config.toml"),
        subsonic_config_toml("http://127.0.0.1:1"),
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_velo"))
        .arg("--subsonic-test")
        .env("XDG_CONFIG_HOME", dir.as_os_str())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1), "exit code must be 1");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("could not be reached"), "{stderr}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn subsonic_test_without_a_subsonic_table_fails_with_a_reason() {
    let dir = temp_dir("binary-no-table");
    fs::write(dir.join("velo").join("config.toml"), "volume = 40\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_velo"))
        .arg("--subsonic-test")
        .env("XDG_CONFIG_HOME", dir.as_os_str())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1), "exit code must be 1");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("subsonic"), "{stderr}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_cli_parses_the_subsonic_test_flag() {
    let cli = Cli::try_parse_from(["velo", "--subsonic-test"])
        .unwrap_or_else(|error| panic!("parse failed: {error}"));
    assert!(cli.subsonic_test);
}

#[test]
fn the_cli_parses_the_sync_flag() {
    let cli = Cli::try_parse_from(["velo", "--sync"])
        .unwrap_or_else(|error| panic!("parse failed: {error}"));
    assert!(cli.sync);
}

// ------------------------------------------------ the sync's two endpoints

/// A transport that answers every request with one body and records the
/// URLs it was asked for.
struct Recording {
    body: Vec<u8>,
    urls: std::sync::Mutex<Vec<String>>,
}

impl Transport for Recording {
    fn get(&self, url: &str) -> Result<Vec<u8>, Error> {
        self.urls.lock().unwrap().push(url.to_owned());
        Ok(self.body.clone())
    }
}

fn album_list_page() -> Vec<u8> {
    r#"{"subsonic-response":{"status":"ok","albumList2":{"album":[{"id":"al-1","name":"Harbor","artist":"Tide","year":2021},{"id":"al-2","name":"Views"}]}}}"#
        .as_bytes()
        .to_vec()
}

#[test]
fn album_list2_pages_with_type_size_and_offset_and_parses_the_albums() {
    let transport = Recording {
        body: album_list_page(),
        urls: std::sync::Mutex::new(Vec::new()),
    };
    let client = Client::new(
        &transport,
        "http://music.example.com",
        "admin",
        Auth::ApiKey {
            key: "k".to_owned(),
        },
    );

    let albums = client
        .album_list2("alphabeticalByName", 500, 500)
        .unwrap_or_else(|error| panic!("album_list2 failed: {error}"));

    let urls = transport.urls.into_inner().unwrap();
    assert_eq!(urls.len(), 1);
    assert!(urls[0].contains("/rest/getAlbumList2?"), "{}", urls[0]);
    let pairs = query_pairs(&urls[0]);
    for piece in ["type=alphabeticalByName", "size=500", "offset=500"] {
        assert!(
            pairs.contains(&piece.to_owned()),
            "{piece} missing: {}",
            urls[0]
        );
    }

    // What the sync needs from a page: the id to fetch, the name and
    // artist the views show; a missing field is absent, not an error.
    assert_eq!(albums.len(), 2);
    assert_eq!(albums[0].id, "al-1");
    assert_eq!(albums[0].name, "Harbor");
    assert_eq!(albums[0].artist.as_deref(), Some("Tide"));
    assert_eq!(albums[0].year, Some(2021));
    assert_eq!(albums[1].id, "al-2");
    assert_eq!(albums[1].name, "Views");
    assert_eq!(albums[1].artist, None);
}

#[test]
fn album_fetches_by_id_and_parses_its_songs() {
    let transport = Recording {
        body: r#"{"subsonic-response":{"status":"ok","album":{"id":"al-9","name":"Harbor","artist":"Tide","year":2021,"song":[{"id":"s-1","title":"Dawn","artist":"Tide","duration":213.4,"track":1,"discNumber":2,"year":2021},{"id":"s-2","title":"Bare","duration":30}]}}}"#
            .as_bytes()
            .to_vec(),
        urls: std::sync::Mutex::new(Vec::new()),
    };
    let client = Client::new(
        &transport,
        "http://music.example.com",
        "admin",
        Auth::Token {
            password: "p".to_owned(),
        },
    );

    let album = client
        .album("al-9")
        .unwrap_or_else(|error| panic!("album failed: {error}"));

    let urls = transport.urls.into_inner().unwrap();
    assert!(
        urls[0].starts_with("http://music.example.com/rest/getAlbum?"),
        "{}",
        urls[0]
    );
    assert!(
        query_pairs(&urls[0]).contains(&"id=al-9".to_owned()),
        "{}",
        urls[0]
    );

    assert_eq!(album.name, "Harbor");
    assert_eq!(album.songs.len(), 2);
    assert_eq!(album.songs[0].id, "s-1");
    assert_eq!(album.songs[0].title.as_deref(), Some("Dawn"));
    assert_eq!(album.songs[0].duration, Some(213.4));
    assert_eq!(album.songs[0].track, Some(1));
    assert_eq!(album.songs[0].disc_number, Some(2));
    // A song without a field reads as absent, not as a parse failure.
    assert_eq!(album.songs[1].title.as_deref(), Some("Bare"));
    assert_eq!(album.songs[1].artist, None);
    assert_eq!(album.songs[1].track, None);
}

#[test]
fn a_failed_album_answer_carries_the_server_sentence() {
    let transport = Recording {
        body: error_envelope(40, "Wrong credentials"),
        urls: std::sync::Mutex::new(Vec::new()),
    };
    let client = Client::new(
        transport,
        "http://music.example.com",
        "admin",
        Auth::Token {
            password: "p".to_owned(),
        },
    );
    let error = client
        .album("al-9")
        .expect_err("a failed album answer must fail");
    assert!(error.to_string().contains("Wrong credentials"), "{error}");
}

// ------------------------------------------------------------- stream (T12c)

#[test]
fn the_stream_endpoint_asks_for_the_raw_bytes_with_auth() {
    let transport = Recording {
        body: b"ID3-audio-bytes".to_vec(),
        urls: std::sync::Mutex::new(Vec::new()),
    };
    let client = Client::new(
        &transport,
        "https://music.example.com",
        "demo",
        Auth::Token {
            password: "demo".to_owned(),
        },
    );

    let bytes = client.stream_bytes("s-1").expect("the stream fetch");
    assert_eq!(
        bytes, b"ID3-audio-bytes",
        "the track's original bytes, untouched"
    );

    let urls = transport.urls.lock().unwrap();
    assert_eq!(urls.len(), 1);
    assert!(
        urls[0].starts_with("https://music.example.com/rest/stream?"),
        "{}",
        urls[0]
    );
    let pairs = query_pairs(&urls[0]);
    for piece in ["id=s-1", "format=raw", "u=demo", "f=json"] {
        assert!(
            pairs.contains(&piece.to_owned()),
            "{piece} missing: {}",
            urls[0]
        );
    }
}

#[test]
fn a_failed_stream_reports_the_server_s_own_sentence() {
    let transport = Recording {
        body: error_envelope(70, "Song not found"),
        urls: std::sync::Mutex::new(Vec::new()),
    };
    let client = Client::new(
        &transport,
        "https://music.example.com",
        "demo",
        Auth::Token {
            password: "demo".to_owned(),
        },
    );
    let error = client
        .stream_bytes("missing")
        .expect_err("a failed stream must fail, not play the error envelope");
    assert!(error.to_string().contains("Song not found"), "{error}");
}

#[test]
fn a_stream_of_audio_that_is_not_json_is_the_track_itself() {
    // Raw audio is never JSON; the answer must reach the caller as bytes,
    // not be mistaken for a too-old server.
    let transport = Recording {
        body: vec![0xFF, 0xFB, 0x90, 0x00],
        urls: std::sync::Mutex::new(Vec::new()),
    };
    let client = Client::new(
        &transport,
        "https://music.example.com",
        "demo",
        Auth::Token {
            password: "demo".to_owned(),
        },
    );
    assert_eq!(
        client.stream_bytes("s-1").unwrap(),
        vec![0xFF, 0xFB, 0x90, 0x00]
    );
}
