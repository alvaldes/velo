use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use velo::audio::{Player, TrackSource};
use velo::ui::{Action, App, crossterm};

use crossterm::event::{Event, KeyEventKind};
use crossterm::{execute, terminal};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1).peekable();
    let mut info_only = false;
    let mut play_and_exit = false;
    let mut index_only = false;
    let mut library_root: Option<PathBuf> = None;
    let mut paths: Vec<PathBuf> = Vec::new();

    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--info" => info_only = true,
            "--play" => play_and_exit = true,
            "--index" => index_only = true,
            "--library" => {
                if let Some(path) = args.next() {
                    library_root = Some(PathBuf::from(path));
                } else {
                    eprintln!("velo: --library needs a path");
                    return ExitCode::from(2);
                }
            }
            option if let Some(path) = option.strip_prefix("--library=") => {
                library_root = Some(PathBuf::from(path));
            }
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

    if index_only {
        return index_library(
            library_root
                .as_deref()
                .unwrap_or(Path::new(DEFAULT_LIBRARY_ROOT)),
        );
    }

    if paths.is_empty() {
        print_help();
        return ExitCode::from(2);
    }

    if info_only {
        return print_info(&paths);
    }
    if play_and_exit {
        return play(&paths);
    }
    run_interface(paths)
}

/// The library root indexed by `velo --index` until T9 makes it configurable.
const DEFAULT_LIBRARY_ROOT: &str = "/Users/alvaldes/Music/Library";

/// Builds or refreshes the library index and reports its counts.
fn index_library(root: &Path) -> ExitCode {
    if !root.is_dir() {
        eprintln!(
            "velo: library root {} does not exist or is not a directory",
            root.display()
        );
        return ExitCode::FAILURE;
    }

    let library = match velo::library::Library::open() {
        Ok(library) => library,
        Err(error) => {
            eprintln!("velo: cannot open the index: {error}");
            return ExitCode::FAILURE;
        }
    };

    if let Err(error) = library.index(root) {
        eprintln!("velo: indexing failed: {error}");
        return ExitCode::FAILURE;
    }

    match library.counts() {
        Ok(counts) => {
            println!("tracks: {}", counts.tracks);
            println!("albums: {}", counts.albums);
            println!("artists: {}", counts.artists);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("velo: {error}");
            ExitCode::FAILURE
        }
    }
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

/// The terminal interface. Restores the terminal even when the loop errors.
fn run_interface(paths: Vec<PathBuf>) -> ExitCode {
    if !std::io::stdout().is_terminal() {
        eprintln!("velo: the interface needs an interactive terminal");
        return ExitCode::FAILURE;
    }

    let mut app = App::new(paths);
    let outcome = interface_loop(&mut app);

    // Whatever happened above, the user's terminal comes back whole.
    let _ = terminal::disable_raw_mode();
    let _ = execute!(std::io::stdout(), terminal::LeaveAlternateScreen);

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("velo: {error}");
            ExitCode::FAILURE
        }
    }
}

fn interface_loop(app: &mut App) -> std::io::Result<()> {
    terminal::enable_raw_mode()?;
    execute!(std::io::stdout(), terminal::EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;

    loop {
        terminal.draw(|frame| app.draw(frame))?;

        if crossterm::event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = crossterm::event::read()?
            && key.kind == KeyEventKind::Press
        {
            let action = app.on_key(key);
            app.apply(action);
            if action == Action::Quit {
                break;
            }
        }
    }

    terminal.show_cursor()
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
    velo [--info | --play] <file>...
    velo [--library <PATH>] --index

ARGS:
    <file>...       Tracks to play as one continuous, gapless stream

OPTIONS:
    --info          Report format, frame count and length without playing
    --play          Play the list and exit, without the terminal interface
    --index         Build or refresh the library index, print track, album
                        and artist counts, then exit
    --library <PATH>  Library root to index (default /Users/alvaldes/Music/Library)
    -h, --help      Show this help

Interface keys: j/k move, enter play, space pause, +/- volume, ? help, q quit.

Audio must share one channel layout: sample rates are converted to the
    output device, channel layouts are not."
    );
}
