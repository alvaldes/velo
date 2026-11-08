use std::ffi::OsStr;
use std::io::{IsTerminal, Stdout};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::{CommandFactory as _, Parser};

use velo::audio::loudness;
use velo::audio::output::{Entry, PathResolver};
use velo::audio::replay_gain::{GainTags, format_gain_db, format_peak, write_tags};
use velo::audio::{Player, TrackSource};
use velo::config::{self, Cli, Config, Settings};
use velo::library::Library;
use velo::subsonic;
use velo::subsonic::cache::{Cache, CoverArt, SubsonicResolver, Unconfigured, cache_dir};
use velo::subsonic::client::{Auth, Client, HttpTransport};
use velo::subsonic::scrobble::{Scrobbler, Scrobbles};
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

    // The Subsonic probe needs no terminal and no library root, so it runs
    // before both — it must work over ssh (T12a). Cleaning the caches is the
    // same kind of headless chore: env in, two directories cleaned, stdout
    // out.
    if cli.cache_clean {
        return cache_clean(
            std::env::var_os("XDG_CACHE_HOME").as_deref(),
            home.as_deref(),
        );
    }
    if cli.subsonic_test {
        return subsonic_test(&settings, home.as_deref());
    }
    // The sync needs no terminal either — it runs over ssh like the probe.
    if cli.sync {
        return sync_library(&settings, home.as_deref());
    }
    // The ReplayGain scan is the same kind of headless chore: files in, a
    // report out, and nothing written unless `--apply` was passed (T13).
    if cli.replaygain_scan {
        if cli.files.is_empty() {
            return usage_help();
        }
        return replaygain_scan(&cli.files, cli.apply);
    }
    // `bit_perfect` is resolved and carried in `settings`; the audio path
    // and the interface read it (T17).
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
        let seams = subsonic_seams(
            &settings,
            std::env::var_os("VELO_SUBSONIC_SECRET").as_deref(),
            std::env::var_os("XDG_CONFIG_HOME").as_deref(),
            std::env::var_os("XDG_CACHE_HOME").as_deref(),
            home.as_deref(),
        );
        return browse_interface(
            library.as_deref(),
            config_file,
            &settings,
            seams.resolver,
            seams.covers,
            seams.scrobbler,
            &seams.warnings,
        );
    }

    if cli.info {
        return print_info(&cli.files);
    }
    if cli.play {
        let seams = subsonic_seams(
            &settings,
            std::env::var_os("VELO_SUBSONIC_SECRET").as_deref(),
            std::env::var_os("XDG_CONFIG_HOME").as_deref(),
            std::env::var_os("XDG_CACHE_HOME").as_deref(),
            home.as_deref(),
        );
        return play(&cli.files, &settings, seams.resolver);
    }
    run_interface(cli.files, &settings, config_file)
}

/// The three seams one configured Subsonic server provides: turning queue
/// entries into files (T12c), fetching cover art (T12d) and scrobbling
/// (T12d). All three come from one credential resolution and one transport —
/// the resolver, the covers source and the scrobbler each hold their own
/// client over a clone of it, and clones of the same connection pool.
struct SubsonicSeams {
    resolver: Option<Arc<dyn PathResolver>>,
    covers: Option<Arc<dyn CoverArt>>,
    scrobbler: Option<Arc<dyn Scrobbles>>,
    /// The credential resolver's warnings — today, a group- or world-readable
    /// `subsonic.key`. They ride to the interface so the status line can say
    /// them once at launch, where the user is looking, instead of only inside
    /// a skip reason when a download fails.
    warnings: Vec<String>,
}

/// Builds the seams above from the user's settings. The environment values
/// are arguments, read here at the edge, so the wiring stays out of the
/// library and the tests. A server whose credentials cannot be resolved
/// still gets a resolver — one that skips its remote tracks with the
/// credential sentence — because the interface must start, and the queue
/// must survive a half-configured server; covers and scrobbles then simply
/// never arrive, which is exactly what a missing cover and a missing play
/// history should look like.
fn subsonic_seams(
    settings: &Settings,
    env_secret: Option<&OsStr>,
    xdg_config_home: Option<&OsStr>,
    xdg_cache_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> SubsonicSeams {
    let without = |resolver: Option<Arc<dyn PathResolver>>| SubsonicSeams {
        resolver,
        covers: None,
        scrobbler: None,
        warnings: Vec::new(),
    };
    let Some(server) = &settings.subsonic else {
        return without(None);
    };
    let credentials = match subsonic::credentials::resolve(
        &server.url,
        &server.user,
        server.api_key.as_deref(),
        env_secret,
        xdg_config_home,
        home,
    ) {
        Ok(credentials) => credentials,
        Err(error) => return without(Some(Arc::new(Unconfigured::new(error.to_string())))),
    };
    let transport = match HttpTransport::new(server.insecure) {
        Ok(transport) => transport,
        Err(error) => {
            // HttpTransport::new never fails today; if it ever does,
            // the sentence still reaches the skip path.
            return without(Some(Arc::new(Unconfigured::new(error.to_string()))));
        }
    };
    let Some(cache) =
        cache_dir(xdg_cache_home, home).map(|dir| Cache::new(dir).with_cap(Cache::TRACK_CACHE_CAP))
    else {
        // With no place for the caches there is nothing a remote track can
        // resolve to; the same sentence path as unconfigured credentials.
        return without(Some(Arc::new(Unconfigured::new(
            "no cache directory — neither XDG_CACHE_HOME nor HOME is usable".to_owned(),
        ))));
    };
    let client = Client::new(
        transport.clone(),
        &credentials.url,
        &credentials.user,
        // Token authentication: every Subsonic-compatible server
        // accepts it, which is why the probe tries it first (T12a).
        Auth::Token {
            password: credentials.secret.clone(),
        },
    );
    let source = velo::library::source_for(&credentials.url);
    let resolver = Arc::new(SubsonicResolver::new(client, source, cache));
    let covers: Arc<dyn CoverArt> = Arc::clone(&resolver) as Arc<dyn CoverArt>;
    // The scrobbler speaks to the same server over the same pool, from its
    // own client: it fires from background threads, never from a tick's path
    // back into the player (T12d).
    let scrobbler = Scrobbler::new(Client::new(
        transport,
        &credentials.url,
        &credentials.user,
        Auth::Token {
            password: credentials.secret,
        },
    ));
    SubsonicSeams {
        resolver: Some(resolver),
        covers: Some(covers),
        scrobbler: Some(Arc::new(scrobbler)),
        warnings: credentials.warnings,
    }
}

/// Resolves the user's settings: the config file at `path` (a missing file is
/// normal and silent) merged under the command line.
fn user_settings(cli: &Cli, path: Option<&Path>) -> Result<Settings, config::ConfigError> {
    match path {
        Some(path) => Config::load_from(path).map(|file| file.merged(cli)),
        None => Ok(Config::default().merged(cli)),
    }
}

/// Empties both Subsonic caches and reports what each freed. The caches are
/// the only two directories this reads: a sibling file beside them survives.
/// Always exits 0 — cleaning succeeded, or there was nothing to clean.
fn cache_clean(xdg_cache_home: Option<&OsStr>, home: Option<&OsStr>) -> ExitCode {
    let Some(track_dir) = cache_dir(xdg_cache_home, home) else {
        println!(
            "no cache directory — neither XDG_CACHE_HOME nor HOME is usable; nothing to clean"
        );
        return ExitCode::SUCCESS;
    };
    let covers_dir = Cache::new(track_dir.clone()).covers_cache();
    for (name, dir) in [
        ("tracks", track_dir),
        ("covers", covers_dir.dir().to_path_buf()),
    ] {
        let (files, bytes) = subsonic::cache::clean(&dir);
        let unit = if files == 1 { "file" } else { "files" };
        println!(
            "{name} cache: removed {files} {unit}, freed {} — {}",
            human_bytes(bytes),
            dir.display()
        );
    }
    ExitCode::SUCCESS
}

/// A byte count a person can read: whole bytes below 1 KiB, one decimal
/// above it, up to GiB.
fn human_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "a cache holds far fewer bytes than f64 represents exactly"
    )]
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= KIB && unit < 3 {
        value /= KIB;
        unit += 1;
    }
    let name = ["KiB", "MiB", "GiB"][unit - 1];
    format!("{value:.1} {name}")
}

/// The help screen plus the usage-error exit code, for a file mode pointed
/// at nothing.
fn usage_help() -> ExitCode {
    let _ = Cli::command().print_help();
    ExitCode::from(2)
}

/// Answers "can I use this server?" (T12a): resolves the credentials, probes
/// the server and prints what it is, what it advertises and which
/// authentication was verified. Exit 0 on success, 1 with a one-line reason
/// on failure.
fn subsonic_test(settings: &Settings, home: Option<&OsStr>) -> ExitCode {
    let Some(server) = &settings.subsonic else {
        eprintln!("velo: no [subsonic] table in the config file — nothing to test");
        return ExitCode::FAILURE;
    };

    let credentials = subsonic::credentials::resolve(
        &server.url,
        &server.user,
        server.api_key.as_deref(),
        std::env::var_os("VELO_SUBSONIC_SECRET").as_deref(),
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        home,
    );
    let credentials = match credentials {
        Ok(credentials) => credentials,
        Err(error) => {
            eprintln!("velo: {error}");
            return ExitCode::FAILURE;
        }
    };
    // A key file readable by group or others is advice, not a failure.
    for warning in &credentials.warnings {
        eprintln!("velo: warning: {warning}");
    }

    let transport = match subsonic::client::HttpTransport::new(server.insecure) {
        Ok(transport) => transport,
        Err(error) => {
            eprintln!("velo: {error}");
            return ExitCode::FAILURE;
        }
    };
    match subsonic::client::Client::probe(
        transport,
        &credentials.url,
        &credentials.user,
        &credentials.secret,
    ) {
        Ok(report) => {
            let server = &report.server;
            let name = server
                .server_type
                .as_deref()
                .unwrap_or("unknown Subsonic server");
            let version = server.server_version.as_deref().unwrap_or("?");
            let api = server.api_version.as_deref().unwrap_or("?");
            let open = if server.open_subsonic { "yes" } else { "no" };
            println!("server: {name} {version} (API {api}, OpenSubsonic: {open})");
            if server.extensions.is_empty() {
                println!("extensions: none");
            } else {
                println!("extensions: {}", server.extensions.join(", "));
            }
            println!("authentication: {} — verified", report.auth);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("velo: {error}");
            ExitCode::FAILURE
        }
    }
}
/// Syncs the configured Subsonic server into the library index (T12b) and
/// prints the counts per source. Exit 0 on success, 1 with a one-line reason
/// on failure. A failed sync writes nothing, so the index is never left
/// half-synced.
fn sync_library(settings: &Settings, home: Option<&OsStr>) -> ExitCode {
    let Some(server) = &settings.subsonic else {
        eprintln!("velo: no [subsonic] table in the config file — nothing to sync");
        return ExitCode::FAILURE;
    };

    let credentials = subsonic::credentials::resolve(
        &server.url,
        &server.user,
        server.api_key.as_deref(),
        std::env::var_os("VELO_SUBSONIC_SECRET").as_deref(),
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        home,
    );
    let credentials = match credentials {
        Ok(credentials) => credentials,
        Err(error) => {
            eprintln!("velo: {error}");
            return ExitCode::FAILURE;
        }
    };
    for warning in &credentials.warnings {
        eprintln!("velo: warning: {warning}");
    }

    let library = match Library::open() {
        Ok(library) => library,
        Err(error) => {
            eprintln!("velo: cannot open the index: {error}");
            return ExitCode::FAILURE;
        }
    };
    let transport = match subsonic::client::HttpTransport::new(server.insecure) {
        Ok(transport) => transport,
        Err(error) => {
            eprintln!("velo: {error}");
            return ExitCode::FAILURE;
        }
    };

    // The same authentication order as the probe (T12a): token auth first —
    // every Subsonic-compatible server accepts it — and one api-key retry
    // when the server rejects the credentials, because the stored secret may
    // be a key rather than a password. The ping happens before any sync
    // request, so bad credentials fail here, before anything was fetched.
    let client = subsonic::client::Client::new(
        &transport,
        &credentials.url,
        &credentials.user,
        subsonic::client::Auth::Token {
            password: credentials.secret.clone(),
        },
    );
    let client = match client.ping() {
        Ok(_) => client,
        Err(token_error @ subsonic::Error::Server { code: 40, .. }) => {
            // The retry rides on the same client, only to learn whether the
            // secret is a key; the kept client is rebuilt the same way the
            // token one was, so both arms hold the same type.
            let accepts_key = client
                .with_auth(subsonic::client::Auth::ApiKey {
                    key: credentials.secret.clone(),
                })
                .ping()
                .is_ok();
            // The server's own message rides on the first error.
            if !accepts_key {
                eprintln!("velo: sync failed: {token_error}");
                return ExitCode::FAILURE;
            }
            subsonic::client::Client::new(
                &transport,
                &credentials.url,
                &credentials.user,
                subsonic::client::Auth::ApiKey {
                    key: credentials.secret.clone(),
                },
            )
        }
        Err(error) => {
            eprintln!("velo: sync failed: {error}");
            return ExitCode::FAILURE;
        }
    };

    // The sync does not probe: the server's answers themselves say whether
    // the credentials work, and a failed request aborts before any write.
    let source = velo::library::source_for(&credentials.url);
    match library.sync_subsonic(&client, &source) {
        Ok(report) => {
            // The credential ping above is one more request the sync cost.
            let requests = report.requests + 1;
            println!(
                "synced {source}: {} tracks, {} albums, {} removed, {requests} requests",
                report.tracks, report.albums, report.removed
            );
            match library.counts_by_source() {
                Ok(counts) => {
                    let line: Vec<String> = counts
                        .into_iter()
                        .map(|(source, count)| format!("{source}: {count} tracks"))
                        .collect();
                    println!("{}", line.join(" · "));
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("velo: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Err(error) => {
            eprintln!("velo: sync failed: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Measures each file's loudness and reports the `ReplayGain` tags velo would
/// write (T13, S15). All files in one invocation are one album: the album gain
/// is the pooled blocks gated together, never an average of track gains.
///
/// Per file the report says `ok`, `skipped (no audio)` or `failed (reason)`;
/// one bad file never stops the run. Without `--apply` nothing on disk is
/// touched — a dry run is the only default for something that edits files.
/// Exit 0 when every file succeeded, 1 when any file failed (measuring it or
/// writing it), after every file was still attempted.
fn replaygain_scan(files: &[PathBuf], apply: bool) -> ExitCode {
    enum Outcome {
        Measured(loudness::LoudnessMeter),
        NoAudio,
        Failed(String),
    }

    let outcomes: Vec<(&PathBuf, Outcome)> = files
        .iter()
        .map(|path| {
            let outcome = match loudness::meter_for(path) {
                Ok(meter) => match meter.integrated() {
                    Some(_) => Outcome::Measured(meter),
                    None => Outcome::NoAudio,
                },
                Err(error) => Outcome::Failed(error.to_string()),
            };
            (path, outcome)
        })
        .collect();

    // The album: every measured file's blocks pooled and gated together.
    let album =
        loudness::integrated_album(outcomes.iter().filter_map(|(_, outcome)| match outcome {
            Outcome::Measured(meter) => Some(meter),
            _ => None,
        }));

    let mut failures = 0_usize;
    let mut written = 0_usize;
    for (path, outcome) in &outcomes {
        match outcome {
            Outcome::NoAudio => println!("{}: skipped (no audio)", path.display()),
            Outcome::Failed(reason) => {
                failures += 1;
                println!("{}: failed ({reason})", path.display());
            }
            Outcome::Measured(meter) => {
                // Some(_) above: a measured file has a measurement.
                let track = meter
                    .integrated()
                    .expect("a measured file has a measurement");
                let album = album.expect("a measured file makes an album measurement");
                let tags = GainTags {
                    track_gain_db: track.gain_db(),
                    track_peak: track.true_peak,
                    album_gain_db: album.gain_db(),
                    album_peak: album.true_peak,
                };
                if apply {
                    match write_tags(path, &tags) {
                        Ok(()) => {
                            written += 1;
                            println!(
                                "{}: ok, wrote track gain {}, peak {}; album gain {}, peak {}",
                                path.display(),
                                format_gain_db(tags.track_gain_db),
                                format_peak(tags.track_peak),
                                format_gain_db(tags.album_gain_db),
                                format_peak(tags.album_peak)
                            );
                        }
                        Err(error) => {
                            failures += 1;
                            println!(
                                "{}: failed (cannot write the tags: {error})",
                                path.display()
                            );
                        }
                    }
                } else {
                    println!(
                        "{}: ok, track gain {}, peak {}; album gain {}, peak {}",
                        path.display(),
                        format_gain_db(tags.track_gain_db),
                        format_peak(tags.track_peak),
                        format_gain_db(tags.album_gain_db),
                        format_peak(tags.album_peak)
                    );
                }
            }
        }
    }

    if let Some(album) = album {
        println!(
            "album: gain {}, peak {}",
            format_gain_db(album.gain_db()),
            format_peak(album.true_peak)
        );
    }
    if apply {
        if written > 0 {
            let unit = if written == 1 { "file" } else { "files" };
            println!("wrote ReplayGain tags to {written} {unit}");
        }
    } else {
        println!("dry run: nothing written — rerun with --apply to write these tags");
    }
    if failures > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
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
            // Totals now span sources. Printing one number for two libraries
            // would be true and useless, so the breakdown appears as soon as
            // there is more than the index the user scanned yesterday (T12b).
            if let Ok(by_source) = library.counts_by_source() {
                let breakdown: Vec<String> = by_source
                    .iter()
                    .map(|(source, tracks)| format!("{source} {tracks}"))
                    .collect();
                if by_source.len() > 1 || by_source.first().is_some_and(|(s, _)| s != "local") {
                    println!("by source: {}", breakdown.join(" · "));
                }
            }
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

/// The terminal interface over files. Restores the terminal even when the
/// loop errors.
fn run_interface(
    paths: Vec<PathBuf>,
    settings: &Settings,
    config_file: Option<PathBuf>,
) -> ExitCode {
    if !std::io::stdout().is_terminal() {
        eprintln!("velo: the interface needs an interactive terminal");
        return ExitCode::FAILURE;
    }

    // Cover art is detected once, before the alternate screen, while the
    // terminal still answers queries (T8). `None` draws no art at all.
    let picker = artwork::detect();
    let outcome = with_terminal(|terminal| {
        let mut app = App::new(paths);
        // Bit-perfect first: it pins the volume and forces ReplayGain off,
        // and the setters below must land on the pinned player (T17).
        app.set_bit_perfect(settings.bit_perfect);
        app.set_replay_gain(settings.replay_gain);
        app.set_volume(settings.volume);
        app.set_theme(settings.theme);
        app.set_ui(settings.ui);
        app.show_artwork(picker);
        app.set_config_file(config_file);
        // The shuffle and repeat the file saved reach the files interface
        // too; the tab and the open node belong to the browser (S3), and
        // `set_session` remembers the volume above as what the file says.
        app.set_session(settings.session.clone());
        event_loop(&mut app, None, terminal)
    });
    report_outcome(outcome)
}

/// The library interface: browses the index at once while the library root
/// is re-indexed in the background (incrementally, so it is fast when little
/// changed); the lists reload when it ends. A missing root or an empty index
/// still opens the interface and says so instead of exiting. With no root
/// configured, the launch no longer demands a local folder (T12c): a
/// Subsonic server's synced index is the library, and with neither a library
/// nor a server the interface says what to configure in one sentence. `L`
/// still opens the folder prompt at any time.
fn browse_interface(
    root: Option<&Path>,
    config_file: Option<PathBuf>,
    settings: &Settings,
    resolver: Option<Arc<dyn PathResolver>>,
    covers: Option<Arc<dyn CoverArt>>,
    scrobbler: Option<Arc<dyn Scrobbles>>,
    warnings: &[String],
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
        // Bit-perfect first, as in the file interface (T17).
        app.set_bit_perfect(settings.bit_perfect);
        app.set_replay_gain(settings.replay_gain);
        app.set_volume(settings.volume);
        app.set_theme(settings.theme);
        app.set_ui(settings.ui);
        app.show_artwork(picker);
        app.set_config_file(config_file);
        // The session comes back before anything else can move (S3), and
        // remembers the volume above as what the file says.
        app.set_session(settings.session.clone());
        // Remote rows play through the cache (T12c); without a server this
        // stays `None` and every entry already is a file.
        app.set_resolver(resolver);
        // Remote covers and scrobbles ride the same server (T12d); without
        // one, covers simply never arrive and nothing is ever sent.
        app.set_covers(covers);
        app.set_scrobbler(scrobbler);
        // A credential warning — a `subsonic.key` the whole group can read —
        // is said once, in the status line, at the start (T12c's debt): a
        // warning that only surfaces inside a skip reason is no warning.
        app.set_subsonic_warnings(warnings);
        match (&opened, root) {
            (Err(error), _) => app.index_unavailable(error.to_string()),
            (Ok(_), Some(root)) => {
                // Every launch refreshes the index; the event loop starts it.
                app.set_library_root(Some(root.to_path_buf()));
                app.apply(Action::Reindex);
            }
            // No folder to ask for at launch any more (T12c): with only a
            // server configured, the synced index is the library; with
            // neither, one sentence says what to configure instead.
            (Ok(_), None) => app.launch_without_library(settings.subsonic.is_some()),
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

fn play(
    files: &[PathBuf],
    settings: &Settings,
    resolver: Option<Arc<dyn PathResolver>>,
) -> ExitCode {
    let queue = std::sync::Arc::new(std::sync::Mutex::new(
        files.iter().cloned().map(Entry::from).collect(),
    ));
    let mut player = match Player::play_gapless_at_full(
        queue,
        0,
        std::time::Duration::ZERO,
        settings.replay_gain,
        velo::audio::output::Repeat::Off,
        settings.bit_perfect,
        // A `subsonic://<host>/<id>` argument is fetched into the cache and
        // played from there, exactly as the interface does it (T12c). Without
        // a configured server it stays a file, which is what it always was.
        resolver,
    ) {
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

    // One-time notices — a bit-perfect fallback above all — before the end:
    // they are advice, not failures (T17).
    for notice in player.take_notices() {
        eprintln!("velo: {notice}");
    }

    let skipped = player.take_failures();
    let underruns = player.underruns();
    player.stop();
    // A skip is not a failure — the album kept playing — but running to the end
    // without a word would hide a corrupt file, which is what T19 is about.
    for failure in &skipped {
        eprintln!("velo: skipped {}: {}", failure.label, failure.message);
    }
    if underruns > 0 {
        eprintln!("velo: played with {underruns} underruns");
    }
    ExitCode::SUCCESS
}
