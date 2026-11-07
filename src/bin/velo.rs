use std::path::PathBuf;
use std::process::ExitCode;

use velo::audio::{Player, TrackSource};

fn main() -> ExitCode {
    let mut info_only = false;
    let mut paths: Vec<PathBuf> = Vec::new();

    for argument in std::env::args().skip(1) {
        match argument.as_str() {
            "--info" => info_only = true,
            "--help" | "-h" => {
                print_help();
                return ExitCode::SUCCESS;
            }
            option if option.starts_with('-') => {
                eprintln!("velo: unknown option {option}");
                return ExitCode::from(2);
            }
            path => paths.push(PathBuf::from(path)),
        }
    }

    if paths.is_empty() {
        print_help();
        return ExitCode::from(2);
    }

    if info_only {
        return print_info(&paths);
    }
    play(&paths)
}

/// Decodes every file and reports its format and length without opening a device.
fn print_info(paths: &[PathBuf]) -> ExitCode {
    for path in paths {
        match TrackSource::open(path) {
            Ok(mut source) => {
                let spec = source.spec();
                let mut frames = 0_usize;
                loop {
                    match source.read_block() {
                        Ok(Some(block)) => frames += block.len() / usize::from(spec.channels),
                        Ok(None) => break,
                        Err(error) => {
                            eprintln!("velo: {}: {error}", path.display());
                            return ExitCode::FAILURE;
                        }
                    }
                }
                // Frame counts stay far below the exact integer range of f64.
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "a track holds fewer frames than f64 represents exactly"
                )]
                let seconds = frames as f64 / f64::from(spec.sample_rate);
                println!(
                    "{}: {} Hz, {} ch, {frames} frames ({seconds:.2}s)",
                    path.display(),
                    spec.sample_rate,
                    spec.channels
                );
            }
            Err(error) => {
                eprintln!("velo: {}: {error}", path.display());
                return ExitCode::FAILURE;
            }
        }
    }
    ExitCode::SUCCESS
}

fn play(paths: &[PathBuf]) -> ExitCode {
    let mut player = match Player::play_gapless(paths.to_vec()) {
        Ok(player) => player,
        Err(error) => {
            eprintln!("velo: {error}");
            return ExitCode::FAILURE;
        }
    };

    if let Err(error) = player.wait_until_finished() {
        eprintln!("velo: {error}");
        return ExitCode::FAILURE;
    }

    let underruns = player.underruns();
    player.stop();
    if underruns > 0 {
        eprintln!("velo: played with {underruns} underruns");
    }
    ExitCode::SUCCESS
}

fn print_help() {
    println!(
        "velo — a fast, single-binary music player for your terminal

USAGE:
    velo [--info] <file>...

ARGS:
    <file>...    Tracks to play as one continuous, gapless stream

OPTIONS:
    --info       Report format, frame count and length without playing
    -h, --help   Show this help

Audio must share one format for now: resampling lands in T2."
    );
}
