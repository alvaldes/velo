//! The config file layer (T9, S12).
//!
//! Every test uses `load_from` with a path inside a throwaway temporary
//! directory, or pure functions fed their environment as arguments, so no test
//! reads or writes the user's real config and no test ever touches an
//! environment variable (`set_var` is `unsafe` in edition 2024 and this
//! repository forbids unsafe code).

use std::fs;
use std::path::{Path, PathBuf};

use std::ffi::OsStr;

use clap::Parser as _;
use velo::config::{
    Cli, Config, DEFAULT_VOLUME, ReplayGainMode, config_path, default_config_text, expand_path,
    library_to_save, set_library, write_default_config,
};

/// Makes `<temp>/<name>`, verifies the directory can be removed, and returns
/// the path. Each test gets its own directory, so tests cannot see each other.
fn temp_dir(name: &str) -> PathBuf {
    let dir =
        Path::new(&std::env::temp_dir()).join(format!("velo-config-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir)
        .unwrap_or_else(|error| panic!("cannot create {}: {error}", dir.display()));
    dir
}

/// Writes a config file inside `dir/velo/config.toml` and returns its path.
fn write_config(dir: &Path, contents: &str) -> PathBuf {
    let path = dir.join("velo").join("config.toml");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, contents).unwrap();
    path
}

fn cli_with_library(root: &str) -> Cli {
    Cli::try_parse_from(["velo", "--library", root])
        .unwrap_or_else(|error| panic!("parse failed: {error}"))
}

fn cli_with_no_flags() -> Cli {
    Cli::try_parse_from(["velo"]).unwrap_or_else(|error| panic!("parse failed: {error}"))
}

// ---------------------------------------------------------------- missing file

#[test]
fn a_missing_config_file_is_normal_and_silent() {
    let dir = temp_dir("missing");
    // A plausible but absent path: the load must succeed with defaults, not
    // report an error.
    let config = Config::load_from(&dir.join("velo").join("config.toml"));
    let settings = config.unwrap().merged(&cli_with_no_flags());
    assert_eq!(settings.library, None);
    assert_eq!(settings.volume, DEFAULT_VOLUME);
    assert!(!settings.bit_perfect);
    let _ = fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------- every key

#[test]
fn every_key_of_the_config_file_parses() {
    let dir = temp_dir("every-key");
    let path = write_config(
        &dir,
        r#"library = "/music/root"
volume = 85
bit_perfect = true
"#,
    );

    let config = Config::load_from(&path).unwrap_or_else(|error| panic!("load failed: {error}"));
    let settings = config.merged(&cli_with_no_flags());

    assert_eq!(settings.library, Some(PathBuf::from("/music/root")));
    assert_eq!(settings.volume, 85);
    assert!(settings.bit_perfect);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_boundaries_of_volume_are_accepted() {
    let dir = temp_dir("volume-boundaries");
    let path = write_config(&dir, "volume = 0\nbit_perfect = false\n");
    let config = Config::load_from(&path).unwrap_or_else(|error| panic!("load failed: {error}"));
    let settings = config.merged(&cli_with_no_flags());
    assert_eq!(settings.volume, 0);
    assert!(!settings.bit_perfect);
    let _ = fs::remove_dir_all(&dir);

    let dir = temp_dir("volume-boundaries-100");
    let path = write_config(&dir, "volume = 100\n");
    let config = Config::load_from(&path).unwrap_or_else(|error| panic!("load failed: {error}"));
    let settings = config.merged(&cli_with_no_flags());
    assert_eq!(settings.volume, 100);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn an_empty_config_file_is_all_defaults() {
    let dir = temp_dir("empty-file");
    let path = write_config(&dir, "");
    let config = Config::load_from(&path).unwrap_or_else(|error| panic!("load failed: {error}"));
    let settings = config.merged(&cli_with_no_flags());
    assert_eq!(settings.library, None);
    assert!(!settings.bit_perfect);
    let _ = fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------- bad values

#[test]
fn a_bad_value_names_the_file_and_the_line() {
    let dir = temp_dir("bad-value");
    let path = write_config(
        &dir,
        r#"library = "/music/root"
volume = 300
"#,
    );

    let error = Config::load_from(&path).unwrap_err().to_string();
    let expected_file = path.display().to_string();
    assert!(
        error.contains(&expected_file),
        "error must name the file, got: {error}"
    );
    assert!(
        error.contains("line 2"),
        "error must name line 2 (volume), got: {error}"
    );
    assert!(
        error.contains("volume must be between 0 and 100"),
        "error must state the rule, got: {error}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_bad_type_names_the_file_and_the_line() {
    let dir = temp_dir("bad-type");
    let path = write_config(&dir, "bit_perfect = \"maybe\"\n");

    let error = Config::load_from(&path).unwrap_err().to_string();
    let expected_file = path.display().to_string();
    assert!(
        error.contains(&expected_file),
        "error must name the file, got: {error}"
    );
    assert!(
        error.contains("line 1"),
        "error must name line 1 (bit_perfect), got: {error}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn broken_toml_names_the_file_and_the_line() {
    let dir = temp_dir("broken-toml");
    let path = write_config(&dir, "library = \nvolume = 5\n");

    let error = Config::load_from(&path).unwrap_err().to_string();
    let expected_file = path.display().to_string();
    assert!(
        error.contains(&expected_file),
        "error must name the file, got: {error}"
    );
    assert!(
        error.contains("line 1"),
        "error must name line 1, got: {error}"
    );
    let _ = fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------- unknown keys

#[test]
fn an_unknown_key_is_an_error_naming_the_key() {
    let dir = temp_dir("unknown-key");
    let path = write_config(&dir, "volum = 50\n");

    let error = Config::load_from(&path).unwrap_err().to_string();
    let expected_file = path.display().to_string();
    assert!(
        error.contains(&expected_file),
        "error must name the file, got: {error}"
    );
    assert!(
        error.contains("volum"),
        "error must name the unknown key, got: {error}"
    );
    let _ = fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------ replay_gain

#[test]
fn the_replay_gain_key_accepts_all_three_values() {
    let cases = [
        ("track", ReplayGainMode::Track),
        ("album", ReplayGainMode::Album),
        ("off", ReplayGainMode::Off),
    ];
    for (value, expected) in cases {
        let dir = temp_dir("replay-gain-values");
        let path = write_config(&dir, &format!("replay_gain = \"{value}\"\n"));
        let config =
            Config::load_from(&path).unwrap_or_else(|error| panic!("load failed: {error}"));
        let settings = config.merged(&cli_with_no_flags());
        assert_eq!(settings.replay_gain, expected, "replay_gain = \"{value}\"");
        let _ = fs::remove_dir_all(&dir);
    }
}

#[test]
fn replay_gain_defaults_to_track_when_the_key_is_absent() {
    let dir = temp_dir("replay-gain-default");
    let path = write_config(&dir, "volume = 30\n");
    let config = Config::load_from(&path).unwrap_or_else(|error| panic!("load failed: {error}"));
    let settings = config.merged(&cli_with_no_flags());
    assert_eq!(settings.replay_gain, ReplayGainMode::Track);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_bad_replay_gain_value_names_the_file_the_line_and_the_allowed_values() {
    let dir = temp_dir("replay-gain-bad-value");
    let path = write_config(
        &dir,
        r#"library = "/music/root"
replay_gain = "loud"
"#,
    );

    let error = Config::load_from(&path).unwrap_err().to_string();
    let expected_file = path.display().to_string();
    assert!(
        error.contains(&expected_file),
        "error must name the file, got: {error}"
    );
    assert!(
        error.contains("line 2"),
        "error must name line 2 (replay_gain), got: {error}"
    );
    for allowed in ["track", "album", "off"] {
        assert!(
            error.contains(allowed),
            "error must allow \"{allowed}\", got: {error}"
        );
    }
    let _ = fs::remove_dir_all(&dir);
}

// --------------------------------------------------------------- precedence

#[test]
fn the_config_file_beats_the_built_in_defaults() {
    let dir = temp_dir("config-beats-default");
    let path = write_config(&dir, "library = \"/music/from/config\"\nvolume = 42\n");

    let settings = Config::load_from(&path)
        .unwrap_or_else(|error| panic!("load failed: {error}"))
        .merged(&cli_with_no_flags());

    assert_eq!(settings.library, Some(PathBuf::from("/music/from/config")));
    assert_eq!(settings.volume, 42);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_command_line_beats_the_config_file() {
    let dir = temp_dir("cli-beats-config");
    let path = write_config(&dir, "library = \"/music/from/config\"\n");

    let settings = Config::load_from(&path)
        .unwrap_or_else(|error| panic!("load failed: {error}"))
        .merged(&cli_with_library("/music/from/command-line"));

    assert_eq!(
        settings.library,
        Some(PathBuf::from("/music/from/command-line"))
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_cli_value_alone_beats_the_default() {
    let settings = Config::default().merged(&cli_with_library("/music/cli-only"));
    assert_eq!(settings.library, Some(PathBuf::from("/music/cli-only")));
}

#[test]
fn keys_absent_from_the_config_keep_their_defaults() {
    let dir = temp_dir("partial-config");
    let path = write_config(&dir, "volume = 10\n");

    let settings = Config::load_from(&path)
        .unwrap_or_else(|error| panic!("load failed: {error}"))
        .merged(&cli_with_no_flags());

    assert_eq!(settings.library, None);
    assert_eq!(settings.volume, 10);
    assert!(!settings.bit_perfect);
    let _ = fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------- path resolution

#[test]
fn the_default_path_honours_xdg_config_home() {
    let dir = temp_dir("xdg");
    let config = config_path(Some(dir.as_os_str()), Some(OsStr::new("/home/elsewhere")))
        .unwrap_or_else(|| panic!("no path"));
    assert_eq!(config, dir.join("velo").join("config.toml"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_default_path_falls_back_to_home_dot_config() {
    let config =
        config_path(None, Some(OsStr::new("/home/nobody"))).unwrap_or_else(|| panic!("no path"));
    assert_eq!(
        config,
        PathBuf::from("/home/nobody/.config/velo/config.toml")
    );
}

#[test]
fn the_default_path_honours_an_end_to_end_xdg_load() {
    // The same round trip the binary does: resolve from the two env values,
    // then load from the resolved path.
    let dir = temp_dir("xdg-end-to-end");
    let path = write_config(&dir, "volume = 7\n");
    let resolved = config_path(Some(dir.as_os_str()), None).unwrap_or_else(|| panic!("no path"));
    assert_eq!(resolved, path);
    let settings = Config::load_from(&resolved)
        .unwrap_or_else(|error| panic!("load failed: {error}"))
        .merged(&cli_with_no_flags());
    assert_eq!(settings.volume, 7);
    let _ = fs::remove_dir_all(&dir);
}

// ------------------------------------------------- the default file (T5, S4)

#[test]
fn a_missing_config_file_is_written_with_every_key_and_its_default() {
    let dir = temp_dir("default-written");
    let path = dir.join("velo").join("config.toml");

    let written = write_default_config(&path).unwrap_or_else(|error| panic!("write: {error}"));
    assert!(written, "a missing file is written");
    let text = fs::read_to_string(&path).unwrap();
    assert_eq!(text, default_config_text());

    // Every key is there, each with a comment right above it.
    let lines: Vec<&str> = text.lines().collect();
    for key in ["library", "volume", "bit_perfect", "replay_gain"] {
        let at = lines
            .iter()
            .position(|line| {
                let line = line.trim_start_matches('#').trim_start();
                line.starts_with(&format!("{key} ="))
            })
            .unwrap_or_else(|| panic!("{key} missing from\n{text}"));
        assert!(
            at > 0 && lines[at - 1].starts_with('#'),
            "{key} needs a comment above it\n{text}"
        );
    }
    // `library` has no universal default, so it is written commented out.
    assert!(text.contains("\n# library = \"~/Music\"\n"), "{text}");

    // And the file says exactly the defaults.
    let settings = Config::load_from(&path)
        .unwrap_or_else(|error| panic!("load failed: {error}"))
        .merged(&cli_with_no_flags());
    assert_eq!(settings.library, None);
    assert_eq!(settings.volume, DEFAULT_VOLUME);
    assert!(!settings.bit_perfect);
    assert_eq!(settings.replay_gain, ReplayGainMode::Track);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn an_existing_config_file_is_never_overwritten() {
    let dir = temp_dir("default-kept");
    let path = write_config(&dir, "volume   = 7 # mine\n");

    let written = write_default_config(&path).unwrap_or_else(|error| panic!("write: {error}"));
    assert!(!written);
    assert_eq!(fs::read_to_string(&path).unwrap(), "volume   = 7 # mine\n");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_configured_library_still_loads_exactly_as_before() {
    let dir = temp_dir("configured-library");
    let path = write_config(&dir, "library = \"/music/root\"\n");
    let settings = Config::load_from(&path)
        .unwrap_or_else(|error| panic!("load failed: {error}"))
        .merged(&cli_with_no_flags());
    let raw = settings.library.unwrap_or_else(|| panic!("library lost"));
    assert_eq!(raw, PathBuf::from("/music/root"));
    assert_eq!(
        expand_path(&raw, Some(Path::new("/home/me")), Path::new("/cwd")),
        PathBuf::from("/music/root")
    );
    let _ = fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------- subsonic (T12a)

#[test]
fn the_subsonic_table_parses_with_all_its_keys() {
    let dir = temp_dir("subsonic-full");
    let path = write_config(
        &dir,
        r#"[subsonic]
url = "https://music.example.com"
user = "admin"
api_key = "the-key"
insecure = true
"#,
    );
    let settings = Config::load_from(&path)
        .unwrap_or_else(|error| panic!("load failed: {error}"))
        .merged(&cli_with_no_flags());
    let subsonic = settings
        .subsonic
        .unwrap_or_else(|| panic!("the [subsonic] table was lost"));
    assert_eq!(subsonic.url, "https://music.example.com");
    assert_eq!(subsonic.user, "admin");
    assert_eq!(subsonic.api_key.as_deref(), Some("the-key"));
    assert!(subsonic.insecure);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_subsonic_table_defaults_insecure_to_false_and_api_key_to_none() {
    let dir = temp_dir("subsonic-minimal");
    let path = write_config(&dir, "[subsonic]\nurl = \"http://s\"\nuser = \"admin\"\n");
    let settings = Config::load_from(&path)
        .unwrap_or_else(|error| panic!("load failed: {error}"))
        .merged(&cli_with_no_flags());
    let subsonic = settings.subsonic.unwrap_or_else(|| panic!("table lost"));
    assert!(!subsonic.insecure);
    assert_eq!(subsonic.api_key, None);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_subsonic_table_without_url_is_an_error_naming_the_key() {
    let dir = temp_dir("subsonic-no-url");
    let path = write_config(&dir, "[subsonic]\nuser = \"admin\"\n");
    let error = Config::load_from(&path).unwrap_err().to_string();
    assert!(
        error.contains(path.display().to_string().as_str()),
        "error must name the file: {error}"
    );
    assert!(error.contains("url"), "error must name url: {error}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn an_unknown_key_inside_the_subsonic_table_is_an_error_naming_it() {
    let dir = temp_dir("subsonic-unknown-key");
    let path = write_config(
        &dir,
        "[subsonic]\nurl = \"http://s\"\nuser = \"admin\"\napikey = \"x\"\n",
    );
    let error = Config::load_from(&path).unwrap_err().to_string();
    assert!(
        error.contains(path.display().to_string().as_str()),
        "error must name the file: {error}"
    );
    assert!(error.contains("apikey"), "error must name the key: {error}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_bad_subsonic_url_names_the_file_the_line_and_the_rule() {
    let dir = temp_dir("subsonic-bad-url");
    let path = write_config(
        &dir,
        "[subsonic]\nurl = \"ftp://music.example.com\"\nuser = \"a\"\n",
    );
    let error = Config::load_from(&path).unwrap_err().to_string();
    assert!(
        error.contains(path.display().to_string().as_str()),
        "error must name the file: {error}"
    );
    assert!(error.contains("line 2"), "error must name line 2: {error}");
    assert!(error.contains("http"), "error must state the rule: {error}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn existing_keys_keep_working_alongside_a_subsonic_table() {
    let dir = temp_dir("subsonic-alongside");
    let path = write_config(
        &dir,
        "library = \"/music\"\nvolume = 33\n\n[subsonic]\nurl = \"http://s\"\nuser = \"admin\"\n",
    );
    let settings = Config::load_from(&path)
        .unwrap_or_else(|error| panic!("load failed: {error}"))
        .merged(&cli_with_no_flags());
    assert_eq!(settings.library, Some(PathBuf::from("/music")));
    assert_eq!(settings.volume, 33);
    assert!(settings.subsonic.is_some());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_default_config_mentions_the_subsonic_table_only_as_a_comment() {
    let text = default_config_text();
    assert!(text.contains("# [subsonic]"), "{text}");
    // And the written default file still parses strictly.
    let dir = temp_dir("default-with-subsonic-comment");
    let path = dir.join("velo").join("config.toml");
    write_default_config(&path).unwrap();
    let settings = Config::load_from(&path)
        .unwrap_or_else(|error| panic!("load failed: {error}"))
        .merged(&cli_with_no_flags());
    assert!(settings.subsonic.is_none());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn no_personal_path_is_hard_coded() {
    for source in [
        include_str!("../src/config.rs"),
        include_str!("../src/bin/velo.rs"),
        include_str!("../src/ui/mod.rs"),
    ] {
        assert!(!source.contains("/Users/"), "a personal path is hard-coded");
    }
}

// ---------------------------------------------------------- path expansion

#[test]
fn a_tilde_expands_against_home() {
    let home = Some(Path::new("/home/me"));
    let cwd = Path::new("/somewhere");
    assert_eq!(
        expand_path(Path::new("~"), home, cwd),
        PathBuf::from("/home/me")
    );
    assert_eq!(
        expand_path(Path::new("~/Music/Library"), home, cwd),
        PathBuf::from("/home/me/Music/Library")
    );
    // `~user` is not expanded: it is an ordinary relative name.
    assert_eq!(
        expand_path(Path::new("~other"), home, cwd),
        PathBuf::from("/somewhere/~other")
    );
}

#[test]
fn a_relative_path_resolves_against_the_current_directory() {
    let home = Some(Path::new("/home/me"));
    assert_eq!(
        expand_path(Path::new("Music"), home, Path::new("/work")),
        PathBuf::from("/work/Music")
    );
    assert_eq!(
        expand_path(Path::new("/abs/Music"), home, Path::new("/work")),
        PathBuf::from("/abs/Music")
    );
}

// -------------------------------------------------- writing the library key

#[test]
fn setting_the_library_uncomments_its_line_and_keeps_every_other_line() {
    let dir = temp_dir("set-library-default");
    let path = dir.join("velo").join("config.toml");
    write_default_config(&path).unwrap();

    set_library(&path, "~/Music").unwrap_or_else(|error| panic!("set: {error}"));

    let text = fs::read_to_string(&path).unwrap();
    let expected =
        default_config_text().replace("# library = \"~/Music\"", "library = \"~/Music\"");
    assert_eq!(text, expected);
    let config = Config::load_from(&path).unwrap_or_else(|error| panic!("load: {error}"));
    assert_eq!(config.library, Some(PathBuf::from("~/Music")));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn setting_the_library_replaces_an_active_line_and_quotes_the_value() {
    let dir = temp_dir("set-library-active");
    let path = write_config(&dir, "# mine\nvolume = 3\nlibrary = \"/old\"\n");

    set_library(&path, "/new \"quoted\" dir").unwrap();

    let text = fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("# mine\nvolume = 3\nlibrary = "), "{text}");
    let config = Config::load_from(&path).unwrap_or_else(|error| panic!("load: {error}"));
    assert_eq!(config.library, Some(PathBuf::from("/new \"quoted\" dir")));
    assert_eq!(config.volume, Some(3));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn setting_the_library_adds_the_key_when_the_file_has_none() {
    let dir = temp_dir("set-library-absent");
    let path = write_config(&dir, "volume = 3\n");

    set_library(&path, "/music").unwrap();

    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "library = \"/music\"\nvolume = 3\n"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// An empty `library` is no library: before, `""` joined onto the working
/// directory and indexed wherever velo was launched — the whole home folder,
/// launched from home. It now means unset, so the prompt asks instead.
#[test]
fn an_empty_library_value_is_no_library_at_all() {
    let dir = temp_dir("empty-library");
    let path = write_config(&dir, "library = \"\"\n");
    let settings = Config::load_from(&path)
        .unwrap_or_else(|error| panic!("load failed: {error}"))
        .merged(&cli_with_no_flags());

    assert_eq!(settings.library, None);
    let _ = fs::remove_dir_all(&dir);
}

/// What the prompt saves must load from anywhere: an absolute or `~` path is
/// kept as typed, a path relative to where velo ran is saved expanded.
#[test]
fn the_prompt_saves_a_path_that_loads_from_any_directory() {
    let expanded = Path::new("/home/me/music/My Music");

    assert_eq!(
        library_to_save("~/music/My Music", expanded),
        "~/music/My Music"
    );
    assert_eq!(
        library_to_save("/srv/music", Path::new("/srv/music")),
        "/srv/music"
    );
    assert_eq!(
        library_to_save("My Music", expanded),
        "/home/me/music/My Music"
    );
}

#[test]
fn an_empty_path_does_not_expand_into_the_working_directory() {
    assert_eq!(
        expand_path(
            Path::new(""),
            Some(Path::new("/home/me")),
            Path::new("/cwd")
        ),
        PathBuf::new()
    );
}
