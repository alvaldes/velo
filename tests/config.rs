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
use velo::audio::output::Repeat;
use velo::config::{
    Cli, Config, DEFAULT_VOLUME, OpenNode, ReplayGainMode, ThemeColor, ThemeColors, UiParts,
    config_path, default_config_text, expand_path, library_to_save, remove_value, set_library,
    set_value, write_default_config,
};
use velo::ui::{BrowseBy, View};

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

// ------------------------------------------------ state, theme and ui tables

#[test]
fn a_full_file_carries_the_state_the_theme_and_the_ui() {
    let dir = temp_dir("every-table");
    let path = write_config(
        &dir,
        r##"library = "/music/root"

[state]
volume = 72
shuffle = true
repeat = "all"
view = "queue"
browse_by = "albums"
artist = "Lo-Fi Scientists"
album = "Drift"

[theme]
background = "reset"
text = "#e0e0e0"
dim = "#767676"
accent = "#FF00AA"
secondary = "#5f87d7"
border = "#303030"
ink = "#101010"

[ui]
cover_art = false
spectrum = false
hints = false
top_bar = false
advisories = false
view_numbers = false
"##,
    );

    let config = Config::load_from(&path).unwrap_or_else(|error| panic!("load failed: {error}"));
    let settings = config.merged(&cli_with_no_flags());

    assert_eq!(settings.volume, 72);
    assert!(settings.session.shuffle);
    assert_eq!(settings.session.repeat, Repeat::All);
    assert_eq!(settings.session.view, View::Queue);
    assert_eq!(settings.session.browse_by, BrowseBy::Albums);
    assert_eq!(
        settings.session.node,
        Some(OpenNode {
            artist: Some("Lo-Fi Scientists".to_owned()),
            album: Some("Drift".to_owned()),
            genre: None,
        })
    );
    assert_eq!(
        settings.theme.accent,
        ThemeColor::Rgb {
            red: 0xff,
            green: 0x00,
            blue: 0xaa,
        },
        "hex is read case-insensitively"
    );
    assert_eq!(settings.theme.background, ThemeColor::Terminal);
    assert_eq!(
        settings.theme.text,
        ThemeColor::Rgb {
            red: 0xe0,
            green: 0xe0,
            blue: 0xe0
        }
    );
    assert_eq!(
        settings.theme.border,
        ThemeColor::Rgb {
            red: 0x30,
            green: 0x30,
            blue: 0x30,
        }
    );
    assert_eq!(
        settings.ui,
        UiParts {
            cover_art: false,
            spectrum: false,
            hints: false,
            top_bar: false,
            advisories: false,
            view_numbers: false,
        }
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn every_new_table_is_optional_and_defaults_to_what_velo_did_before() {
    let settings = Config::default().merged(&cli_with_no_flags());

    assert!(!settings.session.shuffle);
    assert_eq!(settings.session.repeat, Repeat::Off);
    assert_eq!(settings.session.view, View::Library);
    assert_eq!(settings.session.browse_by, BrowseBy::Artists);
    assert_eq!(settings.session.node, None);
    assert_eq!(settings.theme, ThemeColors::default());
    assert_eq!(settings.ui, UiParts::default());
    assert!(
        settings.ui.cover_art,
        "the cover is drawn unless told not to"
    );
    assert!(settings.ui.spectrum);
    assert!(settings.ui.hints);
    assert!(settings.ui.top_bar);
    // A file written before the two switches existed draws what it drew: the
    // explanation of a hidden cover and the view numbers come back.
    assert!(settings.ui.advisories, "advisories default to on");
    assert!(settings.ui.view_numbers, "view numbers default to on");
}

#[test]
fn the_state_volume_is_the_one_that_counts_and_the_old_flat_key_still_reads() {
    let dir = temp_dir("state-volume-wins");
    let path = write_config(&dir, "volume = 40\n\n[state]\nvolume = 72\n");
    let settings = Config::load_from(&path)
        .unwrap_or_else(|error| panic!("load failed: {error}"))
        .merged(&cli_with_no_flags());
    assert_eq!(settings.volume, 72, "[state] is where volume lives now");
    let _ = fs::remove_dir_all(&dir);

    let dir = temp_dir("state-volume-legacy");
    let path = write_config(&dir, "volume = 40\n");
    let settings = Config::load_from(&path)
        .unwrap_or_else(|error| panic!("load failed: {error}"))
        .merged(&cli_with_no_flags());
    assert_eq!(
        settings.volume, 40,
        "a file written before [state] still reads"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_state_volume_out_of_range_names_the_file_and_the_line() {
    let dir = temp_dir("state-volume-bounds");
    let path = write_config(&dir, "[state]\nvolume = 101\n");
    let error = Config::load_from(&path).expect_err("101 is not a volume");
    let message = error.to_string();
    assert!(message.contains("config.toml"), "{message}");
    assert!(message.contains("line 2"), "{message}");
    assert!(message.contains("between 0 and 100"), "{message}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_theme_colour_is_hex_or_reset_in_the_two_places_reset_makes_sense() {
    for (name, body, line, expected) in [
        (
            "theme-reset-accent",
            "[theme]\naccent = \"reset\"\n",
            2,
            "background and text only",
        ),
        (
            "theme-no-hash",
            "[theme]\naccent = \"ff00aa\"\n",
            2,
            "is not a colour",
        ),
        (
            "theme-short",
            "[theme]\naccent = \"#f0a\"\n",
            2,
            "is not a colour",
        ),
        (
            "theme-not-hex",
            "[theme]\naccent = \"#gg00aa\"\n",
            2,
            "is not a colour",
        ),
        (
            "theme-unknown-key",
            "[theme]\naccent = \"#ff00aa\"\naccent_bright = \"#ff00aa\"\n",
            3,
            "unknown field",
        ),
    ] {
        let dir = temp_dir(name);
        let path = write_config(&dir, body);
        let error = Config::load_from(&path).expect_err(name);
        let message = error.to_string();
        assert!(message.contains(expected), "{name}: {message}");
        assert!(
            message.contains(&format!("line {line}")),
            "{name}: {message}"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}

#[test]
fn the_state_names_are_checked_by_name_and_an_empty_node_is_no_node() {
    for (name, body) in [
        ("state-repeat", "[state]\nrepeat = \"sometimes\"\n"),
        ("state-view", "[state]\nview = \"mixer\"\n"),
        ("state-browse-by", "[state]\nbrowse_by = \"playlists\"\n"),
    ] {
        let dir = temp_dir(name);
        let path = write_config(&dir, body);
        let error = Config::load_from(&path).expect_err(name);
        let message = error.to_string();
        assert!(message.contains("unknown variant"), "{name}: {message}");
        let _ = fs::remove_dir_all(&dir);
    }

    let dir = temp_dir("state-empty-node");
    let path = write_config(
        &dir,
        "[state]\nbrowse_by = \"albums\"\nartist = \"\"\nalbum = \"   \"\ngenre = \"\"\n",
    );
    let settings = Config::load_from(&path)
        .unwrap_or_else(|error| panic!("load failed: {error}"))
        .merged(&cli_with_no_flags());
    assert_eq!(settings.session.node, None, "a blank name is no node");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_default_file_names_every_table_and_reads_back_as_the_defaults() {
    let text = default_config_text();
    for table in ["[state]", "[theme]", "[ui]"] {
        assert!(text.contains(table), "{table} is missing from:\n{text}");
    }
    assert!(
        text.contains("[state]\nvolume = "),
        "volume lives under [state] now:\n{text}"
    );

    let dir = temp_dir("default-round-trip");
    let path = write_config(&dir, &text);
    let settings = Config::load_from(&path)
        .unwrap_or_else(|error| panic!("the default file must load: {error}"))
        .merged(&cli_with_no_flags());
    assert_eq!(settings.volume, DEFAULT_VOLUME);
    assert_eq!(settings.theme, ThemeColors::default());
    assert_eq!(settings.ui, UiParts::default());
    assert_eq!(settings.session.repeat, Repeat::Off);
    assert_eq!(settings.session.view, View::Library);
    assert_eq!(settings.session.browse_by, BrowseBy::Artists);
    // The two switches the first delivery did not have are written too, so
    // the default file names every key the parser reads.
    assert!(text.contains("advisories = true"), "{text}");
    assert!(text.contains("view_numbers = true"), "{text}");
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

    // Every key is there, each with a comment right above it — or right above
    // the table it opens, which is where the first key of a table sits.
    let lines: Vec<&str> = text.lines().collect();
    let keys = [
        "library",
        "bit_perfect",
        "replay_gain",
        "volume",
        "shuffle",
        "repeat",
        "view",
        "browse_by",
        "background",
        "text",
        "dim",
        "accent",
        "secondary",
        "border",
        "ink",
        "cover_art",
        "spectrum",
        "hints",
        "top_bar",
        "advisories",
        "view_numbers",
    ];
    for key in keys {
        let at = lines
            .iter()
            .position(|line| {
                let line = line.trim_start_matches('#').trim_start();
                line.starts_with(&format!("{key} ="))
            })
            .unwrap_or_else(|| panic!("{key} missing from\n{text}"));
        let commented = at > 0 && lines[at - 1].starts_with('#');
        let under_its_table = lines[..at]
            .iter()
            .rposition(|line| line.starts_with('['))
            .is_some_and(|header| header > 0 && lines[header - 1].starts_with('#'));
        assert!(
            commented || under_its_table,
            "{key} needs a comment above it or above its table\n{text}"
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

// ------------------------------------------- writing and removing values (T4, S2)

#[test]
fn a_key_is_inserted_into_an_existing_table_between_its_neighbours() {
    let dir = temp_dir("set-value-insert");
    let path = write_config(
        &dir,
        "# mine\n\n[state]\nvolume = 72\nrepeat = \"off\"\n\n[theme]\naccent = \"#af00af\"\n",
    );

    set_value(&path, Some("state"), "shuffle", "false")
        .unwrap_or_else(|error| panic!("set: {error}"));

    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "# mine\n\n[state]\nvolume = 72\nrepeat = \"off\"\nshuffle = false\n\n[theme]\naccent = \"#af00af\"\n"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn an_active_key_inside_a_table_is_replaced_in_place() {
    let dir = temp_dir("set-value-replace");
    let before = "[state]\nvolume = 30\nshuffle = true\nrepeat = \"off\"\n";
    let path = write_config(&dir, before);

    set_value(&path, Some("state"), "shuffle", "false")
        .unwrap_or_else(|error| panic!("set: {error}"));

    let after = fs::read_to_string(&path).unwrap();
    assert_eq!(
        after,
        "[state]\nvolume = 30\nshuffle = false\nrepeat = \"off\"\n"
    );
    assert_eq!(
        after.lines().count(),
        before.lines().count(),
        "a replacement must not change the line count\n{after}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_commented_key_inside_a_table_is_uncommented_not_duplicated() {
    let dir = temp_dir("set-value-uncomment");
    let path = write_config(&dir, "[ui]\n# cover_art = true\nspectrum = true\n");

    set_value(&path, Some("ui"), "cover_art", "false")
        .unwrap_or_else(|error| panic!("set: {error}"));

    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "[ui]\ncover_art = false\nspectrum = true\n"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_key_is_written_into_a_table_that_does_not_exist_yet() {
    let dir = temp_dir("set-value-new-table");
    let path = write_config(&dir, "library = \"/music\"\nbit_perfect = false\n");

    set_value(&path, Some("ui"), "top_bar", "true").unwrap_or_else(|error| panic!("set: {error}"));

    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "library = \"/music\"\nbit_perfect = false\n[ui]\ntop_bar = true\n"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_top_level_key_is_inserted_above_the_first_table_never_inside_one() {
    let dir = temp_dir("set-value-top-level");
    let path = write_config(&dir, "[state]\nvolume = 72\n[ui]\ncover_art = true\n");

    set_value(&path, None, "bit_perfect", "true").unwrap_or_else(|error| panic!("set: {error}"));

    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "bit_perfect = true\n[state]\nvolume = 72\n[ui]\ncover_art = true\n"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn writing_to_a_missing_file_starts_from_the_default_text() {
    let dir = temp_dir("set-value-missing");
    let path = dir.join("velo").join("config.toml");

    set_value(&path, Some("state"), "volume", "50").unwrap_or_else(|error| panic!("set: {error}"));

    let expected = default_config_text().replace("volume = 100", "volume = 50");
    assert_eq!(fs::read_to_string(&path).unwrap(), expected);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn remove_value_drops_one_key_and_leaves_everything_else_identical() {
    let dir = temp_dir("remove-value");
    let path = write_config(
        &dir,
        "[state]\nvolume = 72\n# shuffle = true\nshuffle = false\n\n[theme]\naccent = \"#af00af\"\n",
    );

    remove_value(&path, Some("state"), "shuffle").unwrap_or_else(|error| panic!("remove: {error}"));

    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "[state]\nvolume = 72\n\n[theme]\naccent = \"#af00af\"\n"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn removing_a_missing_key_or_table_or_file_is_a_silent_no_op() {
    // A missing key and a missing table: the file is not rewritten.
    let dir = temp_dir("remove-no-op");
    let path = write_config(&dir, "[state]\nvolume = 72\n");
    remove_value(&path, Some("state"), "shuffle").unwrap();
    remove_value(&path, Some("theme"), "accent").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "[state]\nvolume = 72\n");
    let _ = fs::remove_dir_all(&dir);

    // A missing file: no file appears.
    let dir = temp_dir("remove-no-file");
    let path = dir.join("velo").join("config.toml");
    remove_value(&path, Some("state"), "volume").unwrap();
    assert!(!path.exists());
    let _ = fs::remove_dir_all(&dir);
}

/// The legacy migration (S2.5): the first write of `volume` under `[state]`
/// appends the table; the stale top-level line stays where it is, so the
/// write stays a per-key edit. The parser reads `[state]` and ignores the
/// stale one, and no table ever holds two `volume` keys.
#[test]
fn the_first_state_write_puts_volume_under_state_without_touching_the_old_line() {
    let dir = temp_dir("legacy-volume");
    let path = write_config(&dir, "volume = 40\n");

    set_value(&path, Some("state"), "volume", "72").unwrap_or_else(|error| panic!("set: {error}"));

    let text = fs::read_to_string(&path).unwrap();
    assert_eq!(text, "volume = 40\n[state]\nvolume = 72\n");
    let state_volume_lines = text
        .lines()
        .skip_while(|line| *line != "[state]")
        .filter(|line| line.starts_with("volume"))
        .count();
    assert_eq!(
        state_volume_lines, 1,
        "one volume line under [state]:\n{text}"
    );
    let settings = Config::load_from(&path)
        .unwrap_or_else(|error| panic!("load: {error}"))
        .merged(&cli_with_no_flags());
    assert_eq!(settings.volume, 72, "[state] volume is the one that counts");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_written_value_round_trips_through_the_parser() {
    let dir = temp_dir("write-round-trip");
    let path = write_config(&dir, &default_config_text());

    set_value(&path, Some("state"), "repeat", "\"all\"")
        .unwrap_or_else(|error| panic!("set repeat: {error}"));
    set_value(&path, Some("theme"), "accent", "\"#123456\"")
        .unwrap_or_else(|error| panic!("set accent: {error}"));
    set_value(&path, Some("ui"), "cover_art", "false")
        .unwrap_or_else(|error| panic!("set cover_art: {error}"));

    let settings = Config::load_from(&path)
        .unwrap_or_else(|error| panic!("load: {error}"))
        .merged(&cli_with_no_flags());
    assert_eq!(settings.session.repeat, Repeat::All);
    assert_eq!(
        settings.theme.accent,
        ThemeColor::Rgb {
            red: 0x12,
            green: 0x34,
            blue: 0x56,
        }
    );
    assert!(!settings.ui.cover_art);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn comments_between_keys_survive_a_write_and_a_removal() {
    let dir = temp_dir("comments-survive");
    let path = write_config(
        &dir,
        "[state]\n# my volume\nvolume = 72\n# shuffle on?\n# shuffle = true\nrepeat = \"off\"\n",
    );

    set_value(&path, Some("state"), "shuffle", "false")
        .unwrap_or_else(|error| panic!("set: {error}"));
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "[state]\n# my volume\nvolume = 72\n# shuffle on?\nshuffle = false\nrepeat = \"off\"\n"
    );

    remove_value(&path, Some("state"), "shuffle").unwrap_or_else(|error| panic!("remove: {error}"));
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "[state]\n# my volume\nvolume = 72\n# shuffle on?\nrepeat = \"off\"\n"
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
