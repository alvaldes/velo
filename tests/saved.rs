//! The saved store (issue #7, S1): the durable list of what the user marked,
//! in `library.toml` beside `config.toml`, carrying both tables — `[[favorite]]`
//! and `[[playlist]]` — from the start, so saved playlists (#4) never need a
//! format change.
//!
//! The rules under test, in the voice of the history and radio stores: a
//! missing file is an empty store, unknown keys are tolerated so a newer velo
//! never has its file wiped, a file that does not parse is an error a reader
//! can report, and a save writes atomically in the shape `radio/store.rs::save`
//! uses.

use std::fs;
use std::path::{Path, PathBuf};

use velo::saved::{Favorite, Playlist, Saved};

fn temp_dir(tag: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("velo-saved-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

/// The config file of a `velo` directory inside `dir`, the way a launch
/// resolves it; the store lives beside it.
fn config_in(dir: &Path) -> PathBuf {
    dir.join("velo").join("config.toml")
}

fn favorite(path: &str) -> Favorite {
    Favorite {
        path: path.to_owned(),
        artist: None,
        album: None,
        title: None,
    }
}

#[test]
fn a_missing_file_is_an_empty_store() {
    let dir = temp_dir("missing");
    let saved = Saved::load(&dir.join("library.toml")).unwrap();
    assert_eq!(saved, Saved::default());
    assert_eq!(saved.favorite.len(), 0);
    assert_eq!(saved.playlist.len(), 0);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_store_lives_beside_the_config_file() {
    let dir = temp_dir("path");
    let file = velo::saved::path_beside(&config_in(&dir));
    assert_eq!(
        file.file_name().unwrap(),
        "library.toml",
        "one file beside config.toml, the way radio.toml and history.toml live"
    );
    assert_eq!(
        file.parent(),
        Some(config_in(&dir).parent().unwrap()),
        "the same directory the config file lives in"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_round_trip_keeps_both_tables() {
    let dir = temp_dir("round-trip");
    let file = velo::saved::path_beside(&config_in(&dir));
    let mut saved = Saved::default();
    saved.toggle_favorite(Favorite {
        path: String::from("/music/Tide/01 - Nightfall.flac"),
        artist: Some(String::from("Tide")),
        album: Some(String::from("Harbor")),
        title: Some(String::from("Nightfall")),
    });
    saved.playlist.push(Playlist {
        name: String::from("Noche"),
        track: vec![String::from("/music/a.flac"), String::from("/music/b.flac")],
    });
    saved.save(&file).unwrap();

    let loaded = Saved::load(&file).unwrap();
    assert_eq!(loaded, saved);
    assert_eq!(loaded.favorite[0].path, "/music/Tide/01 - Nightfall.flac");
    assert_eq!(loaded.favorite[0].artist.as_deref(), Some("Tide"));
    assert_eq!(loaded.favorite[0].album.as_deref(), Some("Harbor"));
    assert_eq!(loaded.favorite[0].title.as_deref(), Some("Nightfall"));
    assert_eq!(loaded.playlist[0].name, "Noche");
    assert_eq!(
        loaded.playlist[0].track,
        ["/music/a.flac", "/music/b.flac"],
        "the playlist table is carried from the start, so #4 needs no format change"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_file_the_store_writes_names_both_tables() {
    let dir = temp_dir("format");
    let file = dir.join("library.toml");
    let mut saved = Saved::default();
    saved.toggle_favorite(Favorite {
        path: String::from("/music/Tide/01 - Nightfall.flac"),
        artist: Some(String::from("Tide")),
        album: Some(String::from("Harbor")),
        title: Some(String::from("Nightfall")),
    });
    saved.playlist.push(Playlist {
        name: String::from("Noche"),
        track: vec![String::from("/music/a.flac")],
    });
    saved.save(&file).unwrap();

    let text = fs::read_to_string(&file).unwrap();
    assert!(text.contains("[[favorite]]"), "got:\n{text}");
    assert!(text.contains("path = \"/music/Tide/01 - Nightfall.flac\""));
    assert!(text.contains("artist = \"Tide\""));
    assert!(text.contains("album = \"Harbor\""));
    assert!(text.contains("title = \"Nightfall\""));
    assert!(text.contains("[[playlist]]"), "got:\n{text}");
    assert!(text.contains("name = \"Noche\""));
    assert!(text.contains("track = [\"/music/a.flac\"]"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn unknown_keys_are_tolerated_so_a_newer_velo_never_has_its_file_wiped() {
    let dir = temp_dir("forward");
    let file = dir.join("library.toml");
    fs::write(
        &file,
        concat!(
            "future = 1\n",
            "[[favorite]]\n",
            "path = \"/music/one.flac\"\n",
            "mood = \"calm\"\n",
            "\n",
            "[[playlist]]\n",
            "name = \"Noche\"\n",
            "track = [\"/music/a.flac\"]\n",
            "cover = \"x.png\"\n",
        ),
    )
    .unwrap();

    let saved = Saved::load(&file).unwrap();
    assert_eq!(
        saved.favorite.len(),
        1,
        "the unknown key did not drop the row"
    );
    assert_eq!(saved.favorite[0].path, "/music/one.flac");
    assert_eq!(
        saved.playlist.len(),
        1,
        "the unknown key did not drop the table"
    );

    // Saving back over it must not have needed the unknown keys to survive
    // the load: the known tables come through whole.
    saved.save(&file).unwrap();
    assert_eq!(
        Saved::load(&file).unwrap().favorite[0].path,
        "/music/one.flac"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_file_that_does_not_parse_is_an_error_a_reader_can_report() {
    let dir = temp_dir("corrupt");
    let file = dir.join("library.toml");
    fs::write(&file, "[[favorite]\npath = ").unwrap();
    let error = Saved::load(&file).unwrap_err();
    assert!(
        matches!(error, velo::saved::SavedError::Parse(_)),
        "a broken file is reported, not silently emptied: {error}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn toggling_a_path_marks_unmarks_and_never_duplicates() {
    let mut saved = Saved::default();

    assert!(
        saved.toggle_favorite(favorite("/music/one.flac")),
        "the first mark turns it on"
    );
    assert!(saved.is_favorite("/music/one.flac"));
    assert_eq!(saved.favorite.len(), 1);

    assert!(
        !saved.toggle_favorite(favorite("/music/one.flac")),
        "marking again unmarks it"
    );
    assert!(!saved.is_favorite("/music/one.flac"));
    assert_eq!(saved.favorite.len(), 0, "no duplicate row is left behind");

    saved.toggle_favorite(favorite("/music/one.flac"));
    saved.toggle_favorite(favorite("/music/one.flac"));
    saved.toggle_favorite(favorite("/music/one.flac"));
    assert_eq!(saved.favorite.len(), 1, "marking three times marks once");
}

#[test]
fn the_identity_is_the_path_exactly() {
    let mut saved = Saved::default();
    saved.toggle_favorite(favorite("subsonic://music.example/rest/stream?id=42"));
    assert!(saved.is_favorite("subsonic://music.example/rest/stream?id=42"));
    assert!(
        !saved.is_favorite("subsonic://MUSIC.example/rest/stream?id=42"),
        "a path is not a URL: no station-key normalising applies to a track's path"
    );
    assert_eq!(saved.favorite.len(), 1);
}

#[test]
fn the_marker_is_state_coming_back_from_the_file() {
    // The store reloaded from the file answers the same marks the session
    // made, so a marker drawn from the store survives a restart.
    let dir = temp_dir("restart");
    let file = velo::saved::path_beside(&config_in(&dir));
    let mut saved = Saved::default();
    saved.toggle_favorite(favorite("/music/one.flac"));
    saved.save(&file).unwrap();
    let reloaded = Saved::load(&file).unwrap();
    assert!(reloaded.is_favorite("/music/one.flac"));
    let _ = fs::remove_dir_all(&dir);
}

// ---- the launch's template -------------------------------------------------
//
// `library.toml` must EXIST after a launch, self-documenting the way
// `config.toml` is: created at startup when missing, never touched when
// present. These drive the real binary — the launch is the unit under test —
// with a throwaway HOME, the way `tests/cli.rs` runs it.

use std::process::Command;

/// Path to the built `velo` binary, provided by the test harness.
fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_velo")
}

/// A throwaway home with its own `velo` config folder, outside the worktree.
fn temp_home(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("velo-saved-home-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join(".config").join("velo")).unwrap();
    dir
}

/// Runs the binary headless (the `history` subcommand needs no terminal) with
/// `home` as its HOME and no `XDG_CONFIG_HOME`, so the launch writes beside
/// `home/.config/velo/config.toml`.
fn launch(home: &Path) {
    let output = Command::new(binary())
        .args(["history"])
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env("VELO_DB", home.join("index.db"))
        .current_dir(home)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap_or_else(|error| panic!("cannot run {}: {error}", binary()));
    assert_eq!(
        output.status.code(),
        Some(0),
        "the launch succeeded: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// (a) A fresh config directory gains `library.toml` with the template, and
/// the template — every line a comment — loads as an empty store.
#[test]
fn a_launch_creates_library_toml_with_the_template_and_it_loads_empty() {
    let home = temp_home("template");
    launch(&home);

    let file = home.join(".config").join("velo").join("library.toml");
    let text = fs::read_to_string(&file).expect("library.toml exists after a launch");
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        assert!(
            line.starts_with('#'),
            "an untouched template is all comments, got: {line:?}"
        );
    }
    assert!(text.contains("[[favorite]]"), "got:\n{text}");
    assert!(text.contains("[[playlist]]"), "got:\n{text}");
    assert!(
        text.contains("rewrites"),
        "the template says velo rewrites the file whole when it saves, got:\n{text}"
    );
    assert_eq!(
        Saved::load(&file).unwrap(),
        Saved::default(),
        "the template parses as an empty store"
    );
    let _ = fs::remove_dir_all(&home);
}

/// (b) An existing `library.toml` — hand-written, with a favourite and a
/// playlist — is byte-identical after a launch: creation is create-only.
#[test]
fn a_launch_never_touches_an_existing_library_toml() {
    let home = temp_home("existing");
    let file = home.join(".config").join("velo").join("library.toml");
    let hand_written = concat!(
        "# my own notes, kept exactly\n",
        "\n",
        "[[favorite]]\n",
        "path = \"/music/Tide/01 - Nightfall.flac\"\n",
        "artist = \"Tide\"\n",
        "\n",
        "[[playlist]]\n",
        "name = \"Noche\"\n",
        "track = [\"/music/a.flac\", \"/music/b.flac\"]\n",
    );
    fs::write(&file, hand_written).unwrap();

    launch(&home);

    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        hand_written,
        "the launch rewrote, reordered or reformatted the file"
    );
    let _ = fs::remove_dir_all(&home);
}

/// (c) A launch does NOT create an empty `history.toml`: it appears when the
/// first track begins, not before.
#[test]
fn a_launch_does_not_create_history_toml() {
    let home = temp_home("no-history");
    launch(&home);

    let history = home.join(".config").join("velo").join("history.toml");
    assert!(
        !history.exists(),
        "history.toml is written by the first beginning, never by a launch"
    );
    let _ = fs::remove_dir_all(&home);
}
