use std::io::{IsTerminal, Stdout};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{CommandFactory as _, Parser};

use velo::audio::{Player, TrackSource};
use velo::config::{self, Cli, Config, Settings};
use velo::library::Library;
use velo::ui::{Action, App, artwork, crossterm};

use crossterm::event::{Event, KeyEventKind};
use crossterm::{execute, terminal};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let settings = match user_settings(&cli) {
        Ok(settings) => settings,
        Err(error) => {
            // A config the user wrote and that does not parse is a usage
            // error of theirs, not a crash of ours.
            eprintln!("velo: {error}");
            return ExitCode::from(2);
        }
    };

    // `bit_perfect` is resolved and carried in `settings`; T17 reads it.
    if cli.index {
        return index_library(&settings.library);
    }

    if cli.files.is_empty() {
        if cli.info || cli.play {
            // The old loop printed the help and exited 2 when a file mode
            // pointed at nothing; that contract stays.
            return usage_help();
        }
        // No files: the interface browses the library index.
        return browse_interface(&settings.library);
    }

    if cli.info {
        return print_info(&cli.files);
    }
    if cli.play {
        return play(&cli.files);
    }
    run_interface(cli.files)
}

/// Resolves the user's settings: the config file (a missing file is normal
/// and silent) merged under the command line.
///
/// The two environment values are read here, at the edge, so the config
/// layer itself stays testable without touching the user's environment.
fn user_settings(cli: &Cli) -> Result<Settings, config::ConfigError> {
    let path = config::config_path(
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    );
    match path {
        Some(path) => Config::load_from(&path).map(|file| file.merged(cli)),
        None => Ok(Config::default().merged(cli)),
    }
}

/// The help screen plus the usage-error exit code, for a file mode pointed
/// at nothing.
fn usage_help() -> ExitCode {
    let _ = Cli::command().print_help();
    ExitCode::from(2)
}
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

    // Cover art is detected once, before the alternate screen, while the
    // terminal still answers queries (T8). `None` draws no art at all.
    let picker = artwork::detect();
    let outcome = with_terminal(|terminal| {
        let mut app = App::new(paths);
        app.show_artwork(picker);
        event_loop(&mut app, terminal)
    });
    report_outcome(outcome)
}

/// The library interface: indexes the library root (incrementally, so a
/// second launch is fast) with progress in the status line, then browses the
/// index. A missing root or an empty index still opens the interface and says
/// so instead of exiting.
fn browse_interface(root: &Path) -> ExitCode {
    if !std::io::stdout().is_terminal() {
        eprintln!("velo: the interface needs an interactive terminal");
        return ExitCode::FAILURE;
    }

    // The same one-time detection as the file interface (T8).
    let picker = artwork::detect();
    let outcome = with_terminal(|terminal| {
        let opened = Library::open();
        let mut app = match &opened {
            Ok(library) => App::browse(Some(library)),
            Err(_) => App::browse(None),
        };
        app.show_artwork(picker);
        match &opened {
            Ok(library) => {
                if root.is_dir() {
                    app.note(format!("indexing {} …", root.display()));
                    terminal.draw(|frame| app.draw(frame))?;
                    match library.index(root) {
                        Ok(report) => app.note(format!(
                            "indexed: {} added, {} updated, {} unchanged, {} removed",
                            report.added, report.updated, report.unchanged, report.removed
                        )),
                        Err(error) => app.note(format!("velo: indexing failed: {error}")),
                    }
                } else {
                    app.note(format!(
                        "velo: library root {} does not exist — no indexing",
                        root.display()
                    ));
                }
            }
            Err(error) => app.note(format!("velo: cannot open the library index: {error}")),
        }
        app.reload();
        event_loop(&mut app, terminal)
    });
    report_outcome(outcome)
}

/// Runs `work` inside the alternate screen with raw mode on, and restores the
/// user's terminal whatever happens.
fn with_terminal(
    work: impl FnOnce(&mut Terminal<CrosstermBackend<Stdout>>) -> std::io::Result<()>,
) -> std::io::Result<()> {
    terminal::enable_raw_mode()?;
    let outcome = execute!(std::io::stdout(), terminal::EnterAlternateScreen).and_then(|()| {
        let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
        work(&mut terminal)
    });
    let _ = terminal::disable_raw_mode();
    let _ = execute!(std::io::stdout(), terminal::LeaveAlternateScreen);
    outcome
}

fn report_outcome(outcome: std::io::Result<()>) -> ExitCode {
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("velo: {error}");
            ExitCode::FAILURE
        }
    }
}

fn event_loop(
    app: &mut App<'_>,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
) -> std::io::Result<()> {
    loop {
        // The producer announces skipped tracks asynchronously; every turn
        // of the loop — the ~100 ms poll cadence — collects what it published.
        app.tick();
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
    let queue = std::sync::Arc::new(std::sync::Mutex::new(paths.to_vec()));
    let mut player = match Player::play_gapless(queue) {
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

    let skipped = player.take_failures();
    let underruns = player.underruns();
    player.stop();
    // A skip is not a failure — the album kept playing — but running to the end
    // without a word would hide a corrupt file, which is what T19 is about.
    for failure in &skipped {
        let name = failure.path.file_name().map_or_else(
            || failure.path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        eprintln!("velo: skipped {name}: {}", failure.message);
    }
    if underruns > 0 {
        eprintln!("velo: played with {underruns} underruns");
    }
    ExitCode::SUCCESS
}
