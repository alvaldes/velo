//! The command line (T9, S13): clap parsing and the exit-code contract.
//!
//! The parse tests drive `Cli::try_parse_from`, so they never touch the
//! environment. The exit-code tests run the real binary: `VELO_DB` is
//! verified only there, and every scenario below exits before the index or
//! the device can be touched.

use std::process::Command;

use clap::Parser as _;
use velo::config::Command as HistoryCommand;
use velo::config::{Cli, HistoryAction};

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

/// A throwaway home outside the worktree, so the binary — which writes its
/// default config file on first run — never touches the user's own.
fn temp_home(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("velo-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Runs the binary in a headless (not a terminal) environment with `home` as
/// its HOME (and no `XDG_CONFIG_HOME`), the index at `home/index.db` and
/// `home` as its current directory, and reports its exit status and stderr.
/// Everything is stdin-closed and piped so the test cannot hang waiting for a
/// terminal.
fn run_in(home: &std::path::Path, arguments: &[&str]) -> (i32, String) {
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
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// [`run_in`] with a fresh home per call site.
fn run(arguments: &[&str]) -> (i32, String) {
    let name = arguments.join("_").replace(['/', '-', '.'], "");
    run_in(&temp_home(&format!("run{name}")), arguments)
}

#[test]
fn the_help_screen_exits_zero_and_names_the_new_features() {
    let home = temp_home("help");
    let output = Command::new(binary())
        .arg("--help")
        .env("HOME", &home)
        .env_remove("XDG_CONFIG_HOME")
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

// ------------------------------------------------ the config file (T5, S4)

#[test]
fn the_first_run_writes_the_default_config_file() {
    let home = temp_home("first-run");
    let (code, _) = run_in(&home, &["--info"]);
    assert_eq!(code, 2);
    let path = home.join(".config").join("velo").join("config.toml");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} not written: {error}", path.display()));
    assert_eq!(text, velo::config::default_config_text());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn index_without_a_library_is_a_usage_error_naming_the_key_and_the_flag() {
    let home = temp_home("index-no-library");
    let (code, stderr) = run_in(&home, &["--index"]);
    assert_eq!(code, 2, "stderr: {stderr}");
    assert!(stderr.contains("`library`"), "stderr: {stderr}");
    assert!(stderr.contains("config.toml"), "stderr: {stderr}");
    assert!(stderr.contains("--library"), "stderr: {stderr}");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_library_flag_expands_a_tilde_and_a_relative_path() {
    let home = temp_home("expand-flag");
    std::fs::create_dir_all(home.join("Music")).unwrap();
    let (code, stderr) = run_in(&home, &["--library", "~/Music", "--index"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let (code, stderr) = run_in(&home, &["--library", "Music", "--index"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_library_in_the_config_file_is_expanded_and_indexed() {
    let home = temp_home("expand-config");
    std::fs::create_dir_all(home.join("Music")).unwrap();
    let config = home.join(".config").join("velo");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("config.toml"), "library = \"~/Music\"\n").unwrap();
    let (code, stderr) = run_in(&home, &["--index"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    // The user's file was not rewritten.
    assert_eq!(
        std::fs::read_to_string(config.join("config.toml")).unwrap(),
        "library = \"~/Music\"\n"
    );
    let _ = std::fs::remove_dir_all(&home);
}

// ------------------------------------------------ the ReplayGain scan (T13)

/// [`run_in`] with stdout captured too: the scan's per-file report *is* its
/// product, so its lines are part of the contract.
fn run_capture(home: &std::path::Path, arguments: &[&str]) -> (i32, String, String) {
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

/// Writes a 16-bit PCM stereo WAV with a 1 kHz sine at `amplitude` (i16 units),
/// `frames` frames long.
fn write_sine_wav(path: &std::path::Path, frames: usize, amplitude: i16) {
    let sample_rate: u32 = 44_100;
    let channels: u16 = 2;
    let data_len = u32::try_from(frames * usize::from(channels) * 2).unwrap();
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * u32::from(channels) * 2).to_le_bytes());
    out.extend_from_slice(&(channels * 2).to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for frame in 0..frames {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "test fixtures stay far below u32::MAX frames"
        )]
        let frame = frame as u32;
        let value = f64::from(amplitude)
            * (2.0 * std::f64::consts::PI * 1_000.0 * f64::from(frame) / f64::from(sample_rate))
                .sin();
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the try_from below clamps the rounded amplitude"
        )]
        let rounded = value.round() as i64;
        let value = i16::try_from(rounded).unwrap();
        for _ in 0..channels {
            out.extend_from_slice(&value.to_le_bytes());
        }
    }
    std::fs::write(path, out).unwrap();
}

/// A zero-frame WAV: a valid container with no audio in it.
fn write_empty_wav(path: &std::path::Path) {
    let mut out = Vec::with_capacity(44);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&36u32.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&44_100u32.to_le_bytes());
    out.extend_from_slice(&176_400u32.to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&0u32.to_le_bytes());
    std::fs::write(path, out).unwrap();
}

#[test]
fn apply_without_the_scan_is_a_usage_error() {
    let error = Cli::try_parse_from(["velo", "--apply", "/a.flac"])
        .expect_err("--apply alone must be refused");
    assert_eq!(error.exit_code(), 2);
}

#[test]
fn the_scan_without_the_write_flag_changes_nothing_on_disk() {
    let home = temp_home("scan-dry");
    let wav = home.join("song.wav");
    write_sine_wav(&wav, 22_050, 3_277); // −20 dBFS
    let before = std::fs::read(&wav).unwrap();

    let (code, stdout, _) = run_capture(&home, &["--replaygain-scan", "song.wav"]);

    assert_eq!(code, 0, "a clean dry run exits 0");
    assert!(
        stdout.contains("track gain") && stdout.contains("album: gain"),
        "the dry run prints what it would write:\n{stdout}"
    );
    assert!(
        stdout.contains("nothing written"),
        "the dry run says so:\n{stdout}"
    );
    assert_eq!(
        std::fs::read(&wav).unwrap(),
        before,
        "without --apply the file must be byte-identical"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_scan_with_the_write_flag_writes_the_tags() {
    let home = temp_home("scan-apply");
    let wav = home.join("song.wav");
    write_sine_wav(&wav, 22_050, 3_277);
    let before = std::fs::read(&wav).unwrap();

    let (code, stdout, _) = run_capture(&home, &["--replaygain-scan", "--apply", "song.wav"]);

    assert_eq!(code, 0, "stdout: {stdout}");
    assert_ne!(
        std::fs::read(&wav).unwrap(),
        before,
        "with --apply the file changes"
    );
    // The tag is readable through the same path the player uses.
    let gain = velo::audio::replay_gain::Gain::read_from(&wav, velo::config::ReplayGainMode::Track);
    assert!(gain.applies(), "the written tag must reach the player");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_read_only_file_is_reported_and_the_run_continues() {
    let home = temp_home("scan-readonly");
    let readonly = home.join("first.wav");
    let writable = home.join("second.wav");
    write_sine_wav(&readonly, 22_050, 3_277);
    write_sine_wav(&writable, 22_050, 6_538);
    let mut perms = std::fs::metadata(&readonly).unwrap().permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&readonly, perms).unwrap();

    let (code, stdout, _) = run_capture(
        &home,
        &["--replaygain-scan", "--apply", "first.wav", "second.wav"],
    );

    assert_eq!(code, 1, "a failed file must not exit 0\nstdout: {stdout}");
    assert!(
        stdout.contains("first.wav") && stdout.contains("failed"),
        "the read-only file is reported:\n{stdout}"
    );
    assert!(
        velo::audio::replay_gain::Gain::read_from(&writable, velo::config::ReplayGainMode::Track)
            .applies(),
        "the run continued with the next file"
    );
    assert!(
        !velo::audio::replay_gain::Gain::read_from(&readonly, velo::config::ReplayGainMode::Track)
            .applies(),
        "the read-only file was never written"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_file_without_audio_is_skipped_not_failed() {
    let home = temp_home("scan-empty");
    let wav = home.join("empty.wav");
    write_empty_wav(&wav);

    let (code, stdout, _) = run_capture(&home, &["--replaygain-scan", "empty.wav"]);

    assert_eq!(code, 0, "stdout: {stdout}");
    assert!(stdout.contains("skipped (no audio)"), "stdout:\n{stdout}");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_broken_file_fails_without_stopping_the_run() {
    let home = temp_home("scan-broken");
    let broken = home.join("broken.wav");
    std::fs::write(&broken, b"this is not audio at all").unwrap();
    let good = home.join("good.wav");
    write_sine_wav(&good, 22_050, 3_277);

    let (code, stdout, _) = run_capture(&home, &["--replaygain-scan", "broken.wav", "good.wav"]);

    assert_eq!(code, 1, "stdout: {stdout}");
    assert!(
        stdout.contains("broken.wav") && stdout.contains("failed ("),
        "the broken file is reported with a reason:\n{stdout}"
    );
    assert!(
        stdout.contains("good.wav") && stdout.contains("track gain"),
        "the run continued with the good file:\n{stdout}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

// ----------------------------------------------------- the history (issue #5)

#[test]
fn the_history_subcommand_parses_in_its_three_shapes() {
    let cli = Cli::try_parse_from(["velo", "history"])
        .unwrap_or_else(|error| panic!("parse failed: {error}"));
    assert_eq!(
        cli.command,
        Some(HistoryCommand::History {
            json: false,
            action: None
        })
    );

    let cli = Cli::try_parse_from(["velo", "history", "--json"])
        .unwrap_or_else(|error| panic!("parse failed: {error}"));
    assert_eq!(
        cli.command,
        Some(HistoryCommand::History {
            json: true,
            action: None
        })
    );

    let cli = Cli::try_parse_from(["velo", "history", "clear"])
        .unwrap_or_else(|error| panic!("parse failed: {error}"));
    assert_eq!(
        cli.command,
        Some(HistoryCommand::History {
            json: false,
            action: Some(HistoryAction::Clear)
        })
    );

    assert!(
        Cli::try_parse_from(["velo", "history", "bogus"]).is_err(),
        "an unknown history action is a usage error"
    );
}

/// Writes a two-entry history, newest first: the top entry with artist and
/// title, the one below with only its path.
fn write_two_entry_history(home: &std::path::Path) {
    let dir = home.join(".config").join("velo");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("history.toml"),
        concat!(
            "[[entry]]\n",
            "path = \"/music/Tide/01 - Nightfall.flac\"\n",
            "played_at = 1762668000\n",
            "artist = \"Tide\"\n",
            "title = \"Nightfall\"\n",
            "\n",
            "[[entry]]\n",
            "path = \"/music/other/02 - Bare.flac\"\n",
            "played_at = 1762660000\n",
        ),
    )
    .unwrap();
}

#[test]
fn velo_history_prints_one_line_per_entry_newest_first() {
    let home = temp_home("history-plain");
    write_two_entry_history(&home);

    let (code, stdout, stderr) = run_capture(&home, &["history"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert_eq!(stdout, "Tide \u{2014} Nightfall\n02 - Bare\n");
    assert_eq!(stderr, "");

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn velo_history_json_prints_the_same_order_as_one_array() {
    let home = temp_home("history-json");
    write_two_entry_history(&home);

    let (code, stdout, stderr) = run_capture(&home, &["history", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(
        stdout,
        concat!(
            "[{\"path\":\"/music/Tide/01 - Nightfall.flac\",\"kind\":\"file\",\"played_at\":1762668000,",
            "\"artist\":\"Tide\",\"title\":\"Nightfall\"},",
            "{\"path\":\"/music/other/02 - Bare.flac\",\"kind\":\"file\",\"played_at\":1762660000}]\n"
        )
    );
    assert_eq!(stderr, "");

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn velo_history_clear_empties_the_file_and_stays_a_success_when_empty() {
    let home = temp_home("history-clear");
    write_two_entry_history(&home);

    let (code, _, stderr) = run_capture(&home, &["history", "clear"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let (code, stdout, _) = run_capture(&home, &["history"]);
    assert_eq!(code, 0);
    assert_eq!(stdout, "", "the file was emptied");
    let (code, _, _) = run_capture(&home, &["history", "clear"]);
    assert_eq!(code, 0, "a second clear is still a success");

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn velo_history_without_a_file_prints_nothing_and_exits_zero() {
    let home = temp_home("history-missing");

    let (code, stdout, stderr) = run_capture(&home, &["history"]);
    assert_eq!(code, 0);
    assert_eq!(stdout, "");
    assert!(
        !stderr.contains("interactive terminal"),
        "history needs no terminal: {stderr}"
    );

    let (code, stdout, _) = run_capture(&home, &["history", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(stdout, "[]\n");

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_corrupt_history_file_is_reported_not_ignored() {
    let home = temp_home("history-corrupt");
    let dir = home.join(".config").join("velo");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("history.toml"), "[[entry]]\npath = ").unwrap();

    let (code, stdout, stderr) = run_capture(&home, &["history"]);
    assert_eq!(code, 1, "stderr: {stderr}");
    assert_eq!(stdout, "");
    assert!(
        stderr.contains("cannot read") && stderr.contains("history.toml"),
        "the error names the file: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&home);
}

// ---------------------------------------------------------- mono (issue #8)

#[test]
fn the_mono_flag_parses_and_defaults_to_off() {
    let cli = Cli::try_parse_from(["velo", "--mono", "/a.flac"])
        .unwrap_or_else(|error| panic!("parse failed: {error}"));
    assert!(cli.mono);

    let cli = Cli::try_parse_from(["velo", "/a.flac"])
        .unwrap_or_else(|error| panic!("parse failed: {error}"));
    assert!(!cli.mono, "without --mono the session starts in stereo");
}

#[test]
fn mono_with_bit_perfect_is_refused_at_startup() {
    // `bit_perfect = true` in the config: one line on stderr, exit 1, and
    // nothing plays. The file below does not exist, so reaching it would mean
    // the check did not fire first.
    let home = temp_home("mono-bit-perfect");
    let config = home.join(".config").join("velo");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("config.toml"), "bit_perfect = true\n").unwrap();
    let (code, stderr) = run_in(&home, &["--mono", "--play", "/no/such/velo-mono.wav"]);
    assert_eq!(code, 1, "stderr: {stderr}");
    assert_eq!(
        stderr, "velo: --mono cannot be combined with bit_perfect\n",
        "exactly one line on stderr: {stderr:?}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn mono_without_bit_perfect_gets_past_the_startup_check() {
    // The differential: the same invocation without `bit_perfect` never
    // mentions the refusal — it fails later, on the file it was asked to open.
    let home = temp_home("mono-bit-perfect-off");
    let (code, stderr) = run_in(&home, &["--mono", "--play", "/no/such/velo-mono.wav"]);
    assert_eq!(code, 1, "stderr: {stderr}");
    assert!(
        !stderr.contains("bit_perfect"),
        "the refusal is bit-perfect's alone: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_fresh_home_gains_the_config_and_the_radio_template_but_no_history() {
    // What a person who just installed velo finds: the config that documents
    // itself, and the radio file that shows the format by example. The
    // history is not one of them — an empty one would say nothing.
    let home = temp_home("fresh-files");
    let (code, stderr) = run_in(&home, &[]);
    assert_eq!(code, 1, "a headless launch refuses the interface: {stderr}");

    let config_dir = home.join(".config").join("velo");
    assert!(
        config_dir.join("config.toml").is_file(),
        "the config is written on the first launch"
    );
    let radio = std::fs::read_to_string(config_dir.join("radio.toml"))
        .expect("the radio template is written on the first launch");
    assert!(
        radio.contains("[[favorite]]") && radio.contains("[[station]]"),
        "the radio file documents its own format: {radio}"
    );
    assert!(
        !config_dir.join("history.toml").exists(),
        "the history appears when the first track does, not before"
    );

    let _ = std::fs::remove_dir_all(&home);
}
