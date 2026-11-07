//! The command line (T9, S13): clap parsing and the exit-code contract.
//!
//! The parse tests drive `Cli::try_parse_from`, so they never touch the
//! environment. The exit-code tests run the real binary: `VELO_DB` is
//! verified only there, and every scenario below exits before the index or
//! the device can be touched.

use std::process::Command;

use clap::Parser as _;
use velo::config::Cli;

#[test]
fn the_positional_files_parse_in_order() {
    let cli = Cli::try_parse_from(["velo", "/a.flac", "/b.mp3", "/c.ogg"])
        .unwrap_or_else(|error| panic!("parse failed: {error}"));
    assert_eq!(
        cli.files
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>(),
        ["/a.flac", "/b.mp3", "/c.ogg"]
    );
    assert!(!cli.info);
    assert!(!cli.play);
    assert!(!cli.index);
    assert!(cli.library.is_none());
}

#[test]
fn every_flag_parses() {
    let cli = Cli::try_parse_from(["velo", "--info", "/a.flac"])
        .unwrap_or_else(|error| panic!("parse failed: {error}"));
    assert!(cli.info);
    let cli = Cli::try_parse_from(["velo", "--play", "/a.flac"])
        .unwrap_or_else(|error| panic!("parse failed: {error}"));
    assert!(cli.play);
    let cli = Cli::try_parse_from(["velo", "--index"])
        .unwrap_or_else(|error| panic!("parse failed: {error}"));
    assert!(cli.index);
}

#[test]
fn the_library_flag_takes_a_path_in_both_spellings() {
    let cli = Cli::try_parse_from(["velo", "--library", "/music/a", "--index"])
        .unwrap_or_else(|error| panic!("parse failed: {error}"));
    assert_eq!(
        cli.library.as_deref(),
        Some(std::path::Path::new("/music/a"))
    );
    assert!(cli.index);

    let cli = Cli::try_parse_from(["velo", "--library=/music/b"])
        .unwrap_or_else(|error| panic!("parse failed: {error}"));
    assert_eq!(
        cli.library.as_deref(),
        Some(std::path::Path::new("/music/b"))
    );
}

#[test]
fn a_flag_missing_its_value_is_a_usage_error() {
    let error = Cli::try_parse_from(["velo", "--library"]).expect_err("--library needs a path");
    assert_eq!(error.exit_code(), 2);
}

#[test]
fn an_unknown_option_is_a_usage_error() {
    let error = Cli::try_parse_from(["velo", "--bogus"]).expect_err("unknown option");
    assert_eq!(error.exit_code(), 2);
}

#[test]
fn no_arguments_parses_to_an_empty_file_list() {
    let cli = Cli::try_parse_from(["velo"]).unwrap_or_else(|error| panic!("parse failed: {error}"));
    assert_eq!(cli.files.len(), 0, "no arguments means no files");
}

#[test]
fn the_help_output_asked_for_exits_zero() {
    // Parse level: help is an error of kind DisplayHelp, meant to exit 0.
    let error = Cli::try_parse_from(["velo", "--help"]).expect_err("--help is not a normal parse");
    assert_eq!(error.exit_code(), 0);
    let error = Cli::try_parse_from(["velo", "-h"]).expect_err("-h is not a normal parse");
    assert_eq!(error.exit_code(), 0);
}

// ------------------------------------------------- the real binary's contract

/// Path to the built `velo` binary, provided by the test harness.
fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_velo")
}

/// Runs the binary in a headless (not a terminal) environment and reports its
/// exit status and stderr. Everything is stdin-closed and piped so the test
/// cannot hang waiting for a terminal.
fn run(arguments: &[&str]) -> (i32, String) {
    let output = Command::new(binary())
        .args(arguments)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap_or_else(|error| panic!("cannot run {}: {error}", binary()));
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn the_help_screen_exits_zero_and_names_the_new_features() {
    let output = Command::new(binary())
        .arg("--help")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap_or_else(|error| panic!("cannot run {}: {error}", binary()));
    assert_eq!(output.status.code(), Some(0));
    let help = String::from_utf8_lossy(&output.stdout).into_owned();
    for feature in ["search", "queue", "cover art", "--library", "--index"] {
        assert!(
            help.contains(feature),
            "--help must mention {feature:?}\n--- help ---\n{help}"
        );
    }
}

#[test]
fn no_arguments_opens_the_interface() {
    // Since T4, no arguments browse the library; headless the interface
    // refuses with exit 1 and a message. This pins what is, not what was.
    let (code, stderr) = run(&[]);
    assert_eq!(code, 1);
    assert!(stderr.contains("interactive terminal"), "stderr: {stderr}");
}

#[test]
fn a_usage_error_exits_two() {
    let (code, _) = run(&["--bogus"]);
    assert_eq!(code, 2);
    let (code, _) = run(&["--library"]);
    assert_eq!(code, 2);
    let (code, _) = run(&["--info"]);
    assert_eq!(code, 2);
}

#[test]
fn a_missing_file_exits_one() {
    let (code, stderr) = run(&["--info", "/no/such/velo-file.flac"]);
    assert_eq!(code, 1);
    assert!(
        stderr.contains("/no/such/velo-file.flac"),
        "stderr: {stderr}"
    );
}

#[test]
fn a_missing_library_root_exits_one() {
    // `--index` checks the root before it opens or writes any database.
    let (code, stderr) = run(&["--library", "/no/such/velo-root", "--index"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("/no/such/velo-root"), "stderr: {stderr}");
}
