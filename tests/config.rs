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
use velo::config::{Cli, Config, DEFAULT_LIBRARY_ROOT, DEFAULT_VOLUME, config_path};

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
    assert_eq!(settings.library.as_path(), Path::new(DEFAULT_LIBRARY_ROOT));
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

    assert_eq!(settings.library, PathBuf::from("/music/root"));
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
    assert_eq!(settings.library.as_path(), Path::new(DEFAULT_LIBRARY_ROOT));
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

// --------------------------------------------------------------- precedence

#[test]
fn the_config_file_beats_the_built_in_defaults() {
    let dir = temp_dir("config-beats-default");
    let path = write_config(&dir, "library = \"/music/from/config\"\nvolume = 42\n");

    let settings = Config::load_from(&path)
        .unwrap_or_else(|error| panic!("load failed: {error}"))
        .merged(&cli_with_no_flags());

    assert_eq!(settings.library, PathBuf::from("/music/from/config"));
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

    assert_eq!(settings.library, PathBuf::from("/music/from/command-line"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_cli_value_alone_beats_the_default() {
    let settings = Config::default().merged(&cli_with_library("/music/cli-only"));
    assert_eq!(settings.library, PathBuf::from("/music/cli-only"));
}

#[test]
fn keys_absent_from_the_config_keep_their_defaults() {
    let dir = temp_dir("partial-config");
    let path = write_config(&dir, "volume = 10\n");

    let settings = Config::load_from(&path)
        .unwrap_or_else(|error| panic!("load failed: {error}"))
        .merged(&cli_with_no_flags());

    assert_eq!(settings.library.as_path(), Path::new(DEFAULT_LIBRARY_ROOT));
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
