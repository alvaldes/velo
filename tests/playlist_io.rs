//! Playlist import and export (issue #9): `velo playlist import` reads a
//! `.m3u`, `.m3u8` or `.pls` into the saved library — reporting the entries
//! whose file is gone instead of dropping them silently — and
//! `velo playlist export` writes a saved playlist back out as `.m3u`.
//!
//! Both verbs are headless like `history`: no terminal, no library index.
//! The tests drive the real binary with a throwaway HOME, the way
//! `tests/saved.rs` and `tests/cli.rs` do.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_velo")
}

/// A throwaway home with its own `velo` config folder, outside the worktree.
fn temp_home(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("velo-playlist-io-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join(".config").join("velo")).unwrap();
    // Canonical, because the binary resolves its current directory and the
    // importer joins it: on macOS /var is /private/var, and the expectations
    // below must use the same spelling the process sees.
    fs::canonicalize(&dir).unwrap()
}

/// Runs the binary headless (stdin closed, output piped) with `home` as its
/// HOME, no `XDG_CONFIG_HOME`, the index at `home/index.db` and `home` as the
/// current directory. Reports exit code, stdout and stderr.
fn run(home: &Path, arguments: &[&str]) -> (i32, String, String) {
    let output = Command::new(binary())
        .args(arguments)
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env("VELO_DB", home.join("index.db"))
        .current_dir(home)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap_or_else(|error| panic!("cannot run {}: {error}", binary()));
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// A `music/` folder in `home` with three real files, so existence checks
/// have something to find; the contents are irrelevant, only existence is.
fn music_dir(home: &Path) -> PathBuf {
    let music = home.join("music");
    fs::create_dir_all(&music).unwrap();
    for name in ["a.flac", "b.flac", "c.flac"] {
        fs::write(music.join(name), "").unwrap();
    }
    music
}

/// Writes `library.toml` with one playlist of the given name and tracks.
fn seed_playlist(home: &Path, name: &str, tracks: &[&str]) {
    let track_list = tracks
        .iter()
        .map(|track| format!("\"{track}\""))
        .collect::<Vec<_>>()
        .join(", ");
    fs::write(
        home.join(".config").join("velo").join("library.toml"),
        format!("[[playlist]]\nname = \"{name}\"\ntrack = [{track_list}]\n"),
    )
    .unwrap();
}

/// The store as it stands, loaded from `home`'s config directory.
fn store(home: &Path) -> velo::saved::Saved {
    velo::saved::Saved::load(&home.join(".config").join("velo").join("library.toml")).unwrap()
}

// ---- import -----------------------------------------------------------------

#[test]
fn an_m3u_imports_under_its_stem_with_relative_entries_resolved_against_its_directory() {
    let home = temp_home("import-m3u");
    let music = music_dir(&home);
    let source = home.join("noche.m3u");
    fs::write(
        &source,
        format!(
            "#EXTM3U\n#EXTINF:-1,A\n{}\n#EXTINF:-1,B\nmusic/b.flac\n",
            music.join("a.flac").display()
        ),
    )
    .unwrap();

    let (code, stdout, stderr) = run(&home, &["playlist", "import", "noche.m3u"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("imported \"noche\""), "stdout: {stdout}");

    let saved = store(&home);
    let playlist = saved.playlist("noche").expect("saved under the stem");
    assert_eq!(
        playlist.track,
        [
            music.join("a.flac").display().to_string(),
            // The relative entry resolved against the playlist file's own
            // directory, not the current directory's guesswork elsewhere.
            music.join("b.flac").display().to_string(),
        ]
    );
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn an_m3u8_imports_the_same_way() {
    let home = temp_home("import-m3u8");
    let music = music_dir(&home);
    let source = home.join("fiesta.m3u8");
    fs::write(&source, format!("{}\n", music.join("a.flac").display())).unwrap();

    let (code, _, stderr) = run(&home, &["playlist", "import", "fiesta.m3u8"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        store(&home).playlist("fiesta").is_some(),
        "saved under the stem of the .m3u8"
    );
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_pls_imports_its_file_entries_in_order() {
    let home = temp_home("import-pls");
    let music = music_dir(&home);
    let source = home.join("mixtape.pls");
    fs::write(
        &source,
        format!(
            "[playlist]\nNumberOfEntries=2\nFile2={}\nFile1=music/a.flac\nTitle1=A\nTitle2=B\n",
            music.join("b.flac").display()
        ),
    )
    .unwrap();

    let (code, _, stderr) = run(&home, &["playlist", "import", "mixtape.pls"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let saved = store(&home);
    let playlist = saved.playlist("mixtape").expect("saved under the stem");
    assert_eq!(
        playlist.track,
        [
            music.join("a.flac").display().to_string(),
            music.join("b.flac").display().to_string(),
        ],
        "File1 before File2, however the file wrote them"
    );
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_taken_name_is_refused_in_one_line_and_nothing_is_written() {
    let home = temp_home("import-taken");
    let music = music_dir(&home);
    seed_playlist(&home, "noche", &["/music/kept.flac"]);
    let before =
        fs::read_to_string(home.join(".config").join("velo").join("library.toml")).unwrap();

    let source = home.join("noche.m3u");
    fs::write(&source, format!("{}\n", music.join("a.flac").display())).unwrap();

    let (code, stdout, stderr) = run(&home, &["playlist", "import", "noche.m3u"]);
    assert_eq!(code, 1, "stdout: {stdout}");
    assert!(
        stdout.is_empty(),
        "nothing is reported as progress: {stdout}"
    );
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 1, "one line: {stderr:?}");
    assert!(
        lines[0].contains("noche"),
        "the line names the name: {stderr}"
    );
    assert_eq!(
        fs::read_to_string(home.join(".config").join("velo").join("library.toml")).unwrap(),
        before,
        "nothing was written"
    );
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn entries_whose_file_is_gone_are_reported_and_not_stored() {
    let home = temp_home("import-gone");
    let music = music_dir(&home);
    let source = home.join("noche.m3u");
    fs::write(
        &source,
        format!(
            "{}\nmusic/b.flac\n/nowhere/old-take.flac\n",
            music.join("a.flac").display()
        ),
    )
    .unwrap();

    let (code, stdout, stderr) = run(&home, &["playlist", "import", "noche.m3u"]);
    assert_eq!(code, 0, "the import still happens; stderr: {stderr}");
    assert!(
        stdout.contains("1 of 3 entries are gone"),
        "the count, in the issue's words: {stdout}"
    );
    assert!(
        stdout.contains("/nowhere/old-take.flac"),
        "one line per missing path: {stdout}"
    );

    let saved = store(&home);
    let playlist = saved.playlist("noche").unwrap();
    assert_eq!(
        playlist.track,
        [
            music.join("a.flac").display().to_string(),
            music.join("b.flac").display().to_string(),
        ],
        "the gone entry is not stored"
    );
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_playlist_whose_every_entry_is_gone_is_refused() {
    let home = temp_home("import-all-gone");
    music_dir(&home);
    let source = home.join("ghosts.m3u");
    fs::write(&source, "/nowhere/one.flac\n/nowhere/two.flac\n").unwrap();

    let (code, stdout, stderr) = run(&home, &["playlist", "import", "ghosts.m3u"]);
    assert_eq!(code, 1);
    assert!(
        stdout.is_empty(),
        "nothing stored, nothing claimed: {stdout}"
    );
    assert!(stderr.contains("gone"), "the reason is said: {stderr}");
    let saved = store(&home);
    assert!(
        saved.playlist.is_empty(),
        "nothing to save means nothing saved"
    );
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_text_file_named_m3u_is_refused_as_all_gone() {
    // A `.m3u` extension is trusted, so this parses as an M3U whose one
    // "entry" is a path that does not exist: refused, nothing saved.
    let home = temp_home("import-text-m3u");
    music_dir(&home);
    let source = home.join("notes.m3u");
    fs::write(&source, "just some words\n").unwrap();

    let (code, stdout, stderr) = run(&home, &["playlist", "import", "notes.m3u"]);
    assert_eq!(code, 1);
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert!(stderr.contains("gone"), "stderr: {stderr}");
    assert_eq!(store(&home).playlist, [] as [velo::saved::Playlist; 0]);
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_file_that_is_neither_pls_nor_m3u_is_refused() {
    // Without a usable extension the body is sniffed, the station way.
    let home = temp_home("import-not-a-playlist");
    music_dir(&home);
    let source = home.join("notes.txt");
    fs::write(&source, "just some words\n").unwrap();

    let (code, _, stderr) = run(&home, &["playlist", "import", "notes.txt"]);
    assert_eq!(code, 1);
    assert!(
        stderr.contains("neither a PLS nor an M3U playlist"),
        "stderr: {stderr}"
    );
    let _ = fs::remove_dir_all(&home);
}

// ---- export -----------------------------------------------------------------

#[test]
fn an_export_writes_extm3u_and_extinf_lines_and_round_trips_through_import() {
    let home = temp_home("export-round-trip");
    let music = music_dir(&home);
    let a = music.join("a.flac").display().to_string();
    let b = music.join("b.flac").display().to_string();
    seed_playlist(&home, "Noche", &[&a, &b]);

    let target = home.join("noche-backup.m3u");
    let (code, stdout, stderr) = run(
        &home,
        &["playlist", "export", "Noche", target.to_str().unwrap()],
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("exported \"Noche\""), "stdout: {stdout}");

    let text = fs::read_to_string(&target).unwrap();
    assert!(text.starts_with("#EXTM3U\n"), "got:\n{text}");
    assert_eq!(text.matches("#EXTINF").count(), 2, "got:\n{text}");
    assert!(
        text.contains(&format!("\n{a}\n")),
        "absolute path, got:\n{text}"
    );
    assert!(
        text.contains(&format!("\n{b}\n")),
        "absolute path, got:\n{text}"
    );

    // The round trip: importing the export back yields the same tracks.
    let (code, _, stderr) = run(&home, &["playlist", "import", "noche-backup.m3u"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let saved = store(&home);
    assert_eq!(
        saved.playlist("noche-backup").unwrap().track,
        [a, b],
        "the round trip is exact"
    );
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn exporting_an_unknown_name_is_refused() {
    let home = temp_home("export-unknown");
    music_dir(&home);
    seed_playlist(&home, "Noche", &["/music/a.flac"]);

    let (code, stdout, stderr) = run(&home, &["playlist", "export", "Fiesta", "out.m3u"]);
    assert_eq!(code, 1);
    assert!(stdout.is_empty(), "stdout: {stdout}");
    assert!(
        stderr.contains("Fiesta"),
        "the line names the name: {stderr}"
    );
    assert!(
        !home.join("out.m3u").exists(),
        "nothing is written for an unknown name"
    );
    let _ = fs::remove_dir_all(&home);
}
