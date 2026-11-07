use std::io::{IsTerminal, Stdout};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{CommandFactory as _, Parser};

use velo::audio::{Player, TrackSource};
use velo::config::{self, Cli, Config, ReplayGainMode, Settings};
use velo::library::Library;
use velo::ui::indexing::Indexer;
use velo::ui::{Action, App, artwork, crossterm};

use crossterm::event::{Event, KeyEventKind};
use crossterm::{execute, terminal};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

fn main() -> ExitCode {
    let cli = Cli::parse();
    // The two environment values are read here, at the edge, so the config
    // layer itself stays testable without touching the user's environment.
    let home = std::env::var_os("HOME").filter(|value| !value.is_empty());
    let config_file = config::config_path(
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        home.as_deref(),
    );
    if let Some(path) = &config_file
        && let Err(error) = config::write_default_config(path)
    {
        // Not fatal: velo runs on its defaults; the user learns why there is
        // no file to edit.
        eprintln!("velo: cannot write {}: {error}", path.display());
    }
    let settings = match user_settings(&cli, config_file.as_deref()) {
        Ok(settings) => settings,
        Err(error) => {
            // A config the user wrote and that does not parse is a usage
            // error of theirs, not a crash of ours.
            eprintln!("velo: {error}");
            return ExitCode::from(2);
        }
    };
    let cwd = std::env::current_dir().unwrap_or_default();
    let library = settings
        .library
        .as_deref()
        .map(|raw| config::expand_path(raw, home.as_deref().map(Path::new), &cwd));

    // `bit_perfect` is resolved and carried in `settings`; T17 reads it.
    if cli.index {
        let Some(root) = library else {
            let file = config_file.as_deref().map_or_else(
                || "the config file".to_owned(),
                |path| path.display().to_string(),
            );
            eprintln!(
                "velo: no library to index — set `library` in {file} or pass --library <PATH>"
            );
            return ExitCode::from(2);
        };
        return index_library(&root);
    }

    if cli.files.is_empty() {
        if cli.info || cli.play {
            // The old loop printed the help and exited 2 when a file mode
            // pointed at nothing; that contract stays.
            return usage_help();
        }
        // No files: the interface browses the library index.
        return browse_interface(library.as_deref(), config_file, &settings);
    }

    if cli.info {
        return print_info(&cli.files);
    }
    if cli.play {
        return play(&cli.files, settings.replay_gain);
    }
    run_interface(cli.files, settings.replay_gain, settings.volume)
}

/// Resolves the user's settings: the config file at `path` (a missing file is
/// normal and silent) merged under the command line.
fn user_settings(cli: &Cli, path: Option<&Path>) -> Result<Settings, config::ConfigError> {
    match path {
        Some(path) => Config::load_from(path).map(|file| file.merged(cli)),
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
fn run_interface(paths: Vec<PathBuf>, replay_gain: ReplayGainMode, volume: u8) -> ExitCode {
    if !std::io::stdout().is_terminal() {
        eprintln!("velo: the interface needs an interactive terminal");
        return ExitCode::FAILURE;
    }

    // Cover art is detected once, before the alternate screen, while the
    // terminal still answers queries (T8). `None` draws no art at all.
    let picker = artwork::detect();
    let outcome = with_terminal(|terminal| {
        let mut app = App::new(paths);
        app.set_replay_gain(replay_gain);
        app.set_volume(volume);
        app.show_artwork(picker);
        event_loop(&mut app, None, terminal)
    });
    report_outcome(outcome)
}

/// The library interface: browses the index at once while the library root
/// is re-indexed in the background (incrementally, so it is fast when little
/// changed); the lists reload when it ends. A missing root or an empty index
/// still opens the interface and says so instead of exiting. With no root
/// configured, a floating prompt asks for one and the answer is saved to the
/// config file, then indexed the same way.
fn browse_interface(
    root: Option<&Path>,
    config_file: Option<PathBuf>,
    settings: &Settings,
) -> ExitCode {
    if !std::io::stdout().is_terminal() {
        eprintln!("velo: the interface needs an interactive terminal");
        return ExitCode::FAILURE;
    }

    // The same one-time detection as the file interface (T8).
    let picker = artwork::detect();
    let outcome = with_terminal(|terminal| {
        let opened = Library::open();
        let mut app = App::browse(opened.as_ref().ok());
        app.set_replay_gain(settings.replay_gain);
        app.set_volume(settings.volume);
        app.show_artwork(picker);
        app.set_config_file(config_file.clone());
        match (&opened, root) {
            (Err(error), _) => app.index_unavailable(error.to_string()),
            (Ok(_), Some(root)) => {
                // Every launch refreshes the index; the event loop starts it.
                app.set_library_root(Some(root.to_path_buf()));
                app.apply(Action::Reindex);
            }
            (Ok(_), None) => app.ask_for_library(config_file),
        }
        event_loop(&mut app, opened.as_ref().ok(), terminal)
    });
    report_outcome(outcome)
}

/// Starts indexing `root` on a background thread with its own connection to
/// the index, or says why it did not.
fn start_index(app: &mut App<'_>, root: PathBuf) -> Option<Indexer> {
    if !root.is_dir() {
        app.note(format!(
            "velo: library root {} does not exist — no indexing",
            root.display()
        ));
        return None;
    }
    app.index_started(&root);
    Some(Indexer::spawn(root, Library::open))
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
    library: Option<&Library>,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
) -> std::io::Result<()> {
    // At most one background index at a time. Quitting drops it: the thread
    // is detached and its uncommitted transaction never reaches the index.
    let mut indexer: Option<Indexer> = None;
    loop {
        if let Some(outcome) = indexer.as_ref().and_then(Indexer::poll) {
            indexer = None;
            app.index_finished(outcome);
        }
        // A root accepted in the prompt, `u`, or the launch: indexed and
        // browsed the same way. One chosen while an index runs waits for it.
        if indexer.is_none()
            && library.is_some()
            && let Some(root) = app.take_library_root()
        {
            indexer = start_index(app, root);
        }

        // The producer announces skipped tracks asynchronously; every turn
        // of the loop — every ~33 ms while the spectrum moves, ~100 ms otherwise —
        // collects what it published and feeds the spectrum.
        app.tick();
        terminal.draw(|frame| app.draw(frame))?;

        if crossterm::event::poll(app.frame_interval())?
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

fn play(paths: &[PathBuf], replay_gain: ReplayGainMode) -> ExitCode {
    let queue = std::sync::Arc::new(std::sync::Mutex::new(paths.to_vec()));
    let mut player = match Player::play_gapless_with_gain(queue, replay_gain) {
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
