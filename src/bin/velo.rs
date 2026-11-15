use std::ffi::OsStr;
use std::io::{IsTerminal, Stdout};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{CommandFactory as _, Parser};

use velo::audio::loudness;
use velo::audio::output::{Entry, PathResolver};
use velo::audio::replay_gain::{GainTags, format_gain_db, format_peak, write_tags};
use velo::audio::{AudioError, Player, TrackSource};
use velo::config::{self, Cli, Command, Config, HistoryAction, PlaylistAction, Settings};
use velo::library::Library;
use velo::subsonic;
use velo::subsonic::cache::{
    Cache, CoverArt, RemoteResolver, SubsonicResolver, Unconfigured, cache_dir,
};
use velo::subsonic::client::{Auth, Client, HttpTransport, Transport};
use velo::subsonic::scrobble::{Scrobbler, Scrobbles};
use velo::ui::indexing::Indexer;
use velo::ui::{Action, App, MONO_WITH_BIT_PERFECT_REFUSED, artwork, crossterm};

use crossterm::event::{Event, KeyEventKind};
use crossterm::{execute, terminal};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use rustix::process::{Signal, getpid, kill_process};
use symphonia::core::io::MediaSource;

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
    // The files a person edits by hand exist from the first launch, with a
    // documented template inside; one that is already there is never touched.
    // `history.toml` is not one of them: it appears when the first track does.
    if let Some(config_file) = &config_file {
        let radio_file = config_file.with_file_name("radio.toml");
        if let Err(error) = velo::radio::store::write_template_if_missing(&radio_file) {
            eprintln!("velo: cannot write {}: {error}", radio_file.display());
        }
        let library_file = velo::saved::path_beside(config_file);
        if let Err(error) = velo::saved::write_default_library(&library_file) {
            eprintln!("velo: cannot write {}: {error}", library_file.display());
        }
        // The themes folder ships one file per theme the picker offers, so the
        // format of a theme file is one `ls` away instead of a guess.
        let themes = config::themes_dir(config_file);
        if let Err(error) = config::write_default_theme(&themes) {
            eprintln!(
                "velo: cannot write the shipped themes in {}: {error}",
                themes.display()
            );
        }
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

    // Mono and bit-perfect contradict each other (issue #8, S3): the downmix
    // scales samples, which is what bit-perfect exists to refuse. The flag is
    // rejected here, at startup, once — never applied silently on the way to
    // the ring.
    if cli.mono && settings.bit_perfect {
        eprintln!("{MONO_WITH_BIT_PERFECT_REFUSED}");
        return ExitCode::from(1);
    }

    // Every headless one-shot mode, in the order the help documents them.
    if let Some(code) = one_shot(
        &cli,
        &settings,
        home.as_deref(),
        config_file.as_deref(),
        library.as_deref(),
    ) {
        return code;
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
        // The tools are looked for once, here (issue #70, S1): the scan is a
        // file read and never a spawn, and its answer feeds the sources row
        // and the resolver the session plays through.
        let chain = detect_chain();
        return browse_interface(
            library.as_deref(),
            config_file,
            &settings,
            SessionResolver {
                resolver: Some(composed_resolver(seams.resolver, chain.clone())),
                youtube_program: chain.0,
            },
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
        // A `youtube://` argument resolves through the chain the same way the
        // interface would (issue #70): with the tools it plays, without them
        // it fails with the install sentence.
        let resolver = Some(composed_resolver(seams.resolver, detect_chain()));
        return play(&cli.files, &settings, resolver);
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
    // The track cache both a server's songs and a radio channel's songs
    // share (cliamp S2): `--cache-clean` cleans what either writes.
    let shared_cache = || {
        cache_dir(xdg_cache_home, home).map(|dir| Cache::new(dir).with_cap(Cache::TRACK_CACHE_CAP))
    };
    // Wraps the server's own resolver disposition with the remote-song cache
    // (cliamp S2): an `http(s)://` row — a configured radio channel's song —
    // caches and plays as a file, everything else follows the disposition.
    // Without a cache directory or a transport there is nothing to wrap, and
    // the disposition stands as it is.
    let with_remote = |fallback: Option<Arc<dyn PathResolver>>| -> Option<Arc<dyn PathResolver>> {
        let (Some(transport), Some(cache)) = (HttpTransport::new(false).ok(), shared_cache())
        else {
            return fallback;
        };
        Some(Arc::new(RemoteResolver::new(
            Arc::new(transport) as Arc<dyn Transport + Send + Sync>,
            cache,
            fallback,
        )))
    };
    let without = |resolver: Option<Arc<dyn PathResolver>>| SubsonicSeams {
        resolver,
        covers: None,
        scrobbler: None,
        warnings: Vec::new(),
    };
    let Some(server) = &settings.subsonic else {
        // No server: a radio channel's songs still cache through the same
        // track cache (cliamp S2), and local files pass through.
        return without(with_remote(None));
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
        Err(error) => {
            return without(with_remote(Some(Arc::new(Unconfigured::new(
                error.to_string(),
            )))));
        }
    };
    let transport = match HttpTransport::new(server.insecure) {
        Ok(transport) => transport,
        Err(error) => {
            // HttpTransport::new never fails today; if it ever does,
            // the sentence still reaches the skip path.
            return without(with_remote(Some(Arc::new(Unconfigured::new(
                error.to_string(),
            )))));
        }
    };
    let Some(cache) =
        cache_dir(xdg_cache_home, home).map(|dir| Cache::new(dir).with_cap(Cache::TRACK_CACHE_CAP))
    else {
        // With no place for the caches there is nothing a remote track can
        // resolve to; the same sentence path as unconfigured credentials.
        return without(with_remote(Some(Arc::new(Unconfigured::new(
            "no cache directory — neither XDG_CACHE_HOME nor HOME is usable".to_owned(),
        )))));
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
    let subsonic = Arc::new(SubsonicResolver::new(client, source, cache));
    let covers: Arc<dyn CoverArt> = Arc::clone(&subsonic) as Arc<dyn CoverArt>;
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
        resolver: with_remote(Some(subsonic as Arc<dyn PathResolver>)),
        covers: Some(covers),
        scrobbler: Some(Arc::new(scrobbler)),
        warnings: credentials.warnings,
    }
}

/// Resolves the user's settings: the config file at `path` (a missing file is
/// normal and silent) merged under the command line.
/// Every headless one-shot mode, in the order the help documents them: each
/// needs no terminal and no library root (the probes and the chores must work
/// over ssh), and each answers `Some` with the exit code the process ends
/// with. `None` means no one-shot was asked for, so the caller goes on into a
/// file mode or the interface. This order is the precedence the `--help` text
/// documents; the two live together on purpose.
fn one_shot(
    cli: &Cli,
    settings: &Settings,
    home: Option<&OsStr>,
    config_file: Option<&Path>,
    library: Option<&Path>,
) -> Option<ExitCode> {
    if cli.cache_clean {
        return Some(cache_clean(
            std::env::var_os("XDG_CACHE_HOME").as_deref(),
            home,
        ));
    }
    if cli.subsonic_test {
        return Some(subsonic_test(settings, home));
    }
    // The station probe is headless the same way: one URL in, a report out.
    if let Some(url) = &cli.radio_test {
        return Some(radio_test(url));
    }
    if cli.sync {
        return Some(sync_library(settings, home));
    }
    // Files in, a report out, and nothing written unless `--apply` was passed.
    if cli.replaygain_scan {
        if cli.files.is_empty() {
            return Some(usage_help());
        }
        return Some(replaygain_scan(&cli.files, cli.apply));
    }
    // `bit_perfect` is resolved and carried in `settings`; the audio path and
    // the interface read it.
    if cli.index {
        let Some(root) = library else {
            let file = config_file.map_or_else(
                || "the config file".to_owned(),
                |path| path.display().to_string(),
            );
            eprintln!(
                "velo: no library to index — set `library` in {file} or pass --library <PATH>"
            );
            return Some(ExitCode::from(2));
        };
        return Some(index_library(root));
    }
    // The history subcommand is the same kind of headless chore: no terminal,
    // no library, works over ssh.
    if let Some(Command::History { json, action }) = &cli.command {
        return Some(history_command(
            config_file,
            *json,
            matches!(action, Some(HistoryAction::Clear)),
        ));
    }
    // The playlist verbs are headless the same way (issue #9): a file in, a
    // report out, the saved store beside the config — no terminal, no index.
    if let Some(Command::Playlist { action }) = &cli.command {
        return Some(playlist_command(config_file, action));
    }
    // `velo update` asks for the check now (issue #69): headless like the
    // other chores, no terminal, no library, no config table consulted.
    if let Some(Command::Update) = &cli.command {
        return Some(update_command());
    }
    None
}

/// The `velo update` subcommand (issue #69): one check now, whatever
/// `[update] check` and `last_check` say, printed to stdout — exit 0 for
/// either verdict, 1 with one line on stderr when the request or the
/// answer failed. "How to update" is read from the running executable's
/// own path, so it names the command that belongs to this install — brew,
/// the one-line installer re-run, or the clone's own cargo build. Nothing
/// is ever downloaded, verified or replaced by velo itself (slice 2's
/// work, out of scope).
fn update_command() -> ExitCode {
    let route = std::env::current_exe().ok().as_deref().map_or(
        velo::update::InstallRoute::InstallScript,
        velo::update::install_route,
    );
    let transport = match velo::radio::client::HttpTransport::new() {
        Ok(transport) => transport,
        Err(error) => {
            eprintln!("velo: cannot ask for the newest release: {error}");
            return ExitCode::FAILURE;
        }
    };
    match velo::update::manual_check(&transport, route) {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(reason) => {
            eprintln!("velo: {reason}");
            ExitCode::FAILURE
        }
    }
}

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

/// How much audio the station probe decodes before it reports: long enough
/// that a broken demux or a wrong codec shows up, short enough that the
/// probe answers at once.
const RADIO_TEST_SECONDS: usize = 2;

/// Answers "can I listen to this station?" (issue #13) in the shape and tone
/// of [`subsonic_test`]: plain fact lines on stdout, exit 0 when the URL
/// plays end to end, and on any failure one `velo: ` line on stderr with
/// exit 1 — a report half-printed would be worse than a reason.
fn radio_test(url: &str) -> ExitCode {
    let (mut stream, probe) =
        match velo::radio::stream::probe(url, velo::radio::stream::Timeouts::default()) {
            Ok(opened) => opened,
            Err(error) => {
                eprintln!("velo: {error}");
                return ExitCode::FAILURE;
            }
        };

    // The decode probe: the first seconds, read exactly as the player reads
    // them — the same blocks, the same demux. A station that answers with
    // headers but no audio is not playable, whatever its headers say.
    let spec = stream.spec();
    let target = RADIO_TEST_SECONDS
        .saturating_mul(usize::try_from(spec.sample_rate).unwrap_or(0))
        .saturating_mul(usize::from(spec.channels));
    let channels = usize::from(spec.channels).max(1);
    let mut frames = 0_usize;
    loop {
        match stream.read_block() {
            Ok(Some(block)) => {
                frames += block.len() / channels;
                if frames * channels >= target {
                    break;
                }
            }
            Ok(None) => break,
            Err(error) => {
                eprintln!("velo: {error}");
                return ExitCode::FAILURE;
            }
        }
    }
    if frames == 0 {
        eprintln!("velo: the station decoded no audio");
        return ExitCode::FAILURE;
    }

    // The report, in the order the issue lists its facts (S2).
    println!("url: {url}");
    println!(
        "wrapper: {}",
        probe.resolved_url.as_deref().unwrap_or("none")
    );
    if probe.redirects.is_empty() {
        println!("redirects: none");
    } else {
        for hop in &probe.redirects {
            println!("redirect: {hop}");
        }
    }
    println!(
        "content-type: {}",
        probe.content_type.as_deref().unwrap_or("none")
    );
    if let Some(metaint) = probe.metaint {
        println!("icy-metaint: {metaint}");
    }
    let codec = stream.codec_name().unwrap_or("unknown");
    println!("codec: {codec} — this build can play it");
    // A station's frame count stays far below the exact integer range of f64.
    #[expect(
        clippy::cast_precision_loss,
        reason = "a decode probe holds fewer frames than f64 represents exactly"
    )]
    let seconds = frames as f64 / f64::from(spec.sample_rate);
    println!(
        "decode: {seconds:.1} s, {} Hz, {} ch",
        spec.sample_rate, spec.channels
    );
    ExitCode::SUCCESS
}

/// The `velo playlist` subcommand (issue #9): import a `.m3u`/`.m3u8`/`.pls`
/// into the saved library, export a saved playlist back out as `.m3u`.
/// Headless like [`history_command`]: no terminal, no library index — it
/// works over ssh. Exit 0 when the verb happened, 1 with one line on stderr
/// when it was refused.
fn playlist_command(config_file: Option<&Path>, action: &PlaylistAction) -> ExitCode {
    // Without a config directory there is no `library.toml` to read or save.
    let Some(config_file) = config_file else {
        eprintln!("velo: no config directory — nowhere to read or save a playlist");
        return ExitCode::FAILURE;
    };
    let store = velo::saved::path_beside(config_file);
    let saved = match velo::saved::Saved::load(&store) {
        Ok(saved) => saved,
        Err(error) => {
            eprintln!("velo: cannot read {}: {error}", store.display());
            return ExitCode::FAILURE;
        }
    };
    match action {
        PlaylistAction::Import { file } => playlist_import(file, saved, &store),
        PlaylistAction::Export { name, file } => playlist_export(name, file, &saved),
    }
}

/// True when the entry is an absolute `http`/`https` URL: not a file, so it
/// resolves to itself and is never reported gone — the same rule
/// [`velo::radio::lists`] uses for a playable station.
fn is_stream_url(entry: &str) -> bool {
    let lower = entry.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Imports `source` (a `.m3u`, `.m3u8` or `.pls` file) as a new saved
/// playlist named after its stem (issue #9, S1). Relative entries resolve
/// against the file's own directory, absolute ones stay (S1). Entries whose
/// file is gone are not stored, and the report says how many and which (S2);
/// a playlist whose every entry is gone is refused (S2). A name already
/// taken is refused with one line naming it, and nothing is written (S1).
fn playlist_import(source: &Path, mut saved: velo::saved::Saved, store: &Path) -> ExitCode {
    let bytes = match std::fs::read(source) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("velo: cannot read {}: {error}", source.display());
            return ExitCode::FAILURE;
        }
    };
    // The file's own extension decides how its body is read: `.pls` is a
    // PLS, `.m3u`/`.m3u8` an M3U (no `#EXTM3U` header required — hand-made
    // playlists often lack it), anything else the same content sniff the
    // station wrapper makes.
    let extension = source
        .extension()
        .and_then(OsStr::to_str)
        .map(str::to_ascii_lowercase);
    let parsed = match extension.as_deref() {
        Some("pls") => Ok(velo::radio::lists::pls_entries(&bytes)),
        Some("m3u" | "m3u8") => velo::radio::lists::m3u_entries(&bytes),
        _ => velo::radio::lists::entries(&bytes),
    };
    let entries = match parsed {
        Ok(entries) => entries,
        Err(error) => {
            eprintln!("velo: {}: {error}", source.display());
            return ExitCode::FAILURE;
        }
    };

    // Relative entries resolve against the playlist file's own directory —
    // made absolute, since the file may have been named relative to the
    // current directory (`noche.m3u` lives wherever velo runs). Absolute
    // entries and stream URLs stay as they are.
    let cwd = std::env::current_dir().unwrap_or_default();
    let base = match source.parent() {
        Some(parent) if parent.is_absolute() => parent.to_path_buf(),
        Some(parent) => {
            if parent.as_os_str().is_empty() || parent == Path::new(".") {
                cwd
            } else {
                cwd.join(parent)
            }
        }
        None => cwd,
    };
    let resolved: Vec<String> = entries
        .iter()
        .map(|entry| {
            if is_stream_url(entry) || Path::new(entry).is_absolute() {
                entry.clone()
            } else {
                base.join(entry).display().to_string()
            }
        })
        .collect();
    let total = resolved.len();
    let gone = |entry: &String| !is_stream_url(entry) && !Path::new(entry).exists();
    let missing: Vec<String> = resolved
        .iter()
        .filter(|entry| gone(entry))
        .cloned()
        .collect();
    let kept: Vec<String> = resolved
        .iter()
        .filter(|entry| !gone(entry))
        .cloned()
        .collect();

    // Nothing to save (S2): an empty file, and a playlist whose every entry
    // is gone. Both are refused before any name is claimed or written.
    if total == 0 {
        eprintln!("velo: {}: the file carries no entries", source.display());
        return ExitCode::FAILURE;
    }
    if kept.is_empty() {
        eprintln!(
            "velo: {}: all {total} entries are gone — nothing to save",
            source.display()
        );
        return ExitCode::FAILURE;
    }

    let Some(name) = source.file_stem().and_then(OsStr::to_str) else {
        eprintln!(
            "velo: {}: the file has no name to save the playlist under",
            source.display()
        );
        return ExitCode::FAILURE;
    };
    // The store is the authority on names; the check here keeps the refusal
    // to one line naming it, before anything is reported or written (S1).
    if saved.playlist(name).is_some() || saved.save_playlist(name, kept).is_err() {
        eprintln!("velo: a playlist named \"{name}\" is already saved");
        return ExitCode::FAILURE;
    }
    if !missing.is_empty() {
        println!("{} of {total} entries are gone", missing.len());
        for entry in &missing {
            println!("{entry}");
        }
    }
    if let Err(error) = saved.save(store) {
        eprintln!("velo: cannot save {}: {error}", store.display());
        return ExitCode::FAILURE;
    }
    println!(
        "imported \"{name}\" with {} of {total} entries",
        total - missing.len()
    );
    ExitCode::SUCCESS
}

/// Exports the saved playlist `name` as an `.m3u` at `target` (issue #9,
/// S3): `#EXTM3U`, then an `#EXTINF` line and one absolute path per track —
/// exactly the shape [`playlist_import`] reads back, so the round trip is
/// exact. A relative stored path is written against the current directory;
/// the library's paths are absolute already. An unknown name is refused and
/// nothing is written.
fn playlist_export(name: &str, target: &Path, saved: &velo::saved::Saved) -> ExitCode {
    let Some(playlist) = saved.playlist(name) else {
        eprintln!("velo: no playlist named \"{name}\" is saved");
        return ExitCode::FAILURE;
    };
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut text = String::from("#EXTM3U\n");
    for track in &playlist.track {
        let path = Path::new(track);
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd.join(path)
        };
        let title = absolute
            .file_name()
            .map_or_else(|| track.clone(), |file| file.to_string_lossy().into_owned());
        text.push_str("#EXTINF:-1,");
        text.push_str(&title);
        text.push('\n');
        text.push_str(&absolute.display().to_string());
        text.push('\n');
    }
    if let Err(error) = std::fs::write(target, text) {
        eprintln!("velo: cannot write {}: {error}", target.display());
        return ExitCode::FAILURE;
    }
    println!(
        "exported \"{name}\" ({} tracks) to {}",
        playlist.track.len(),
        target.display()
    );
    ExitCode::SUCCESS
}

/// The `velo history` subcommand: one line per entry, newest first, `--json`
/// as a JSON array, `clear` to empty the file. Needs no terminal and never
/// exits 2 — reading and clearing a history is a courtesy, not a session.
fn history_command(config_file: Option<&Path>, json: bool, clear: bool) -> ExitCode {
    // No config directory (no HOME/XDG) means nothing was ever recorded, and
    // there is nowhere a `clear` could write either.
    let Some(config_file) = config_file else {
        if json {
            println!("[]");
        }
        return ExitCode::SUCCESS;
    };
    let file = velo::history::path_beside(config_file);
    if clear {
        // A missing file is cleared all the same: the empty document is
        // written, so the answer to "clear" is always success or a named
        // write error, never "nothing to do".
        if let Err(error) = velo::history::History::clear(&file) {
            eprintln!("velo: cannot clear {}: {error}", file.display());
            return ExitCode::FAILURE;
        }
        return ExitCode::SUCCESS;
    }
    let history = match velo::history::History::load(&file) {
        Ok(history) => history,
        Err(error) => {
            eprintln!("velo: cannot read {}: {error}", file.display());
            return ExitCode::FAILURE;
        }
    };
    if json {
        println!("{}", history.to_json());
    } else {
        for entry in &history.entry {
            println!("{}", entry.label());
        }
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
        // and the setters below must land on the pinned player (T17). The
        // launch above already refused `--mono` under bit-perfect, so mono
        // starts only when it can (issue #8).
        app.set_bit_perfect(settings.bit_perfect);
        app.set_mono(settings.mono);
        app.set_replay_gain(settings.replay_gain);
        app.set_volume(settings.volume);
        // The theme library (issue #10), as the browse interface wires it.
        let themes_dir = config_file.as_deref().map(config::themes_dir);
        app.set_theme_library(
            config::select_theme(themes_dir.as_deref(), &settings.theme),
            settings.theme.clone(),
        );
        app.set_ui(settings.ui);
        // The media keys (issue #12, S6): best effort, on their own thread.
        // A launch that cannot have them — no thread for the client, a
        // session the OS refuses — says at most one line and starts all the
        // same.
        match velo::media_keys::MediaKeys::spawn() {
            Ok(media) => app.set_media_keys(media),
            Err(message) => app.note(format!("velo: {message}")),
        }
        app.show_artwork(picker);
        // The one-shot update check (issue #69, slice 1): off the event
        // loop, honouring `[update] check`, silent on failure — before the
        // config file's path moves into the app.
        start_update_check(&mut app, settings, config_file.as_deref());
        app.set_config_file(config_file);
        // The output device the config names (issue #44, S3): every player
        // this session builds opens it, until `d` chooses another one.
        app.set_audio_device(settings.audio_device.clone());
        // The shuffle and repeat the file saved reach the files interface
        // too; the open node belongs to the browser (S3), and
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
/// One session's play-through seam (issue #70): the composed resolver —
/// the Subsonic seams' under `YouTube`'s — and the program the launch's
/// `PATH` scan found, which the sources row's fact reads. Both come from
/// one detection and one composition, decided where the other seams are.
struct SessionResolver {
    resolver: Option<Arc<dyn PathResolver>>,
    youtube_program: Option<PathBuf>,
}

fn browse_interface(
    root: Option<&Path>,
    config_file: Option<PathBuf>,
    settings: &Settings,
    session: SessionResolver,
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
        // Bit-perfect first, as in the file interface (T17); mono starts
        // only when the launch let it (issue #8).
        app.set_bit_perfect(settings.bit_perfect);
        app.set_mono(settings.mono);
        app.set_replay_gain(settings.replay_gain);
        app.set_volume(settings.volume);
        // The theme library (issue #10): the built-in, the `themes/` folder,
        // and the `[theme]` keys of the config file over both (S2).
        let themes_dir = config_file.as_deref().map(config::themes_dir);
        app.set_theme_library(
            config::select_theme(themes_dir.as_deref(), &settings.theme),
            settings.theme.clone(),
        );
        app.set_ui(settings.ui);
        // The media keys (issue #12, S6), as the file interface starts them:
        // best effort, one status line at most, never a wait.
        match velo::media_keys::MediaKeys::spawn() {
            Ok(media) => app.set_media_keys(media),
            Err(message) => app.note(format!("velo: {message}")),
        }
        app.show_artwork(picker);
        // The one-shot update check (issue #69, slice 1), as the file
        // interface starts it: off the event loop, honouring
        // `[update] check`, silent on failure — before the config file's
        // path moves into the app.
        start_update_check(&mut app, settings, config_file.as_deref());
        app.set_config_file(config_file);
        // The output device the config names (issue #44, S3), as the file
        // interface starts with.
        app.set_audio_device(settings.audio_device.clone());
        // The session comes back before anything else can move (S3), and
        // remembers the volume above as what the file says.
        app.set_session(settings.session.clone());
        // The shell opens on the first screen (issue #3): the sources, one
        // level above the library and the radio. The file-launch path
        // (`run_interface`) has no sources to choose from.
        app.open_sources();
        // Remote rows play through the cache (T12c); without a server this
        // stays `None` and every entry already is a file.
        app.set_resolver(session.resolver);
        // The tool the launch's `PATH` scan found (issue #70): the sources
        // row's fact reads it, and no process is spawned to learn what it
        // already says. With none, the row's `enter` says how to install.
        app.set_youtube_program(session.youtube_program);
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

/// Starts the one-shot update check (issue #69, slice 1), honouring
/// `[update] check`: `never` means no request at all, and a check whose
/// stored `[update] last_check` is younger than the mode's interval means
/// no request either. A launch with no config file has nowhere to record
/// the timestamp — the event loop records it after the answer lands — so
/// it does not check: an unchecked launch must not become a check on every
/// launch. The clock is read here, at the edge, so the decision itself
/// stays a pure function. Best effort and silent: a transport or thread
/// that cannot be had is a launch without the check, never an error line
/// (issue #69).
fn start_update_check(app: &mut App<'_>, settings: &Settings, config_file: Option<&Path>) {
    if settings.update_check == velo::update::CheckMode::Never {
        return;
    }
    if config_file.is_none() {
        return;
    }
    let now = std::time::SystemTime::now();
    let last = settings
        .update_last_check
        .map(std::time::Duration::from_secs)
        .and_then(|age| std::time::UNIX_EPOCH.checked_add(age));
    if !velo::update::check_due(settings.update_check, now, last) {
        return;
    }
    // The radio client's transport does the asking: one blocking GET with
    // the required User-Agent and a ten-second bound, no new dependency.
    let Ok(transport) = velo::radio::client::HttpTransport::new() else {
        return;
    };
    // A thread that cannot start is silence, not a status line (issue #69).
    // The thread does the asking only: it never writes the config file. The
    // event loop records `[update] last_check` itself, when it consumes the
    // answer, through the same seam every other config write uses.
    if let Ok(check) = velo::update::UpdateCheck::spawn(
        Arc::new(transport) as Arc<dyn velo::radio::client::Transport + Send + Sync>
    ) {
        app.set_update_check(check);
    }
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

/// Stops the process the way a shell's `ctrl+z` does, and restores the
/// terminal on both sides of the stop (issue #66).
///
/// The sequence mirrors [`with_terminal`], taken down in the order it was
/// put up: raw mode off, alternate screen left, cursor shown. Then the stop
/// itself — `kill_process(getpid(), SIGTSTP)`, the exact signal the shell's
/// job control sends, through rustix's safe API so the crate keeps its
/// `unsafe_code = "forbid"`. That call does not return until the process is
/// resumed (typically `fg`), so the guard goes back up straight after it,
/// in this same function: no signal handler, no `SIGCONT` handler.
///
/// The order decides the failure edges. Raw mode is off *before* the stop,
/// so a failed stop never leaves it on: the error returns up through
/// [`event_loop`] and [`with_terminal`] restores the rest again on the way
/// out. A platform where the call cannot work therefore ends velo with a
/// reason and an intact terminal — the worst outcome is never a wedged
/// screen.
///
/// A suspended process runs no threads, so the audio stream stops by
/// itself: there is no pause/resume logic here, and none is wanted — that
/// is the daemon's problem, and velo has none.
///
/// The terminal itself is not testable here: every step below writes ANSI
/// or asks the OS for job control. What is testable is the keymap guard
/// that routes `ctrl+z` here, pinned in `tests/ui_state.rs`.
fn suspend_and_resume(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> std::io::Result<()> {
    terminal::disable_raw_mode()?;
    execute!(
        std::io::stdout(),
        terminal::LeaveAlternateScreen,
        crossterm::cursor::Show
    )?;
    kill_process(getpid(), Signal::Tstp).map_err(std::io::Error::from)?;

    // Resumed: put the guard back and force a whole frame repaint. The
    // screen's contents belong to the shell now, so a diff against the
    // buffer from before the stop would redraw nothing — `clear` marks
    // every cell dirty instead.
    terminal::enable_raw_mode()?;
    execute!(std::io::stdout(), terminal::EnterAlternateScreen)?;
    terminal.hide_cursor()?;
    terminal.clear()
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
            // `ctrl+z` hands the terminal to the shell before anything is
            // painted: a frame here would write half a screen behind the
            // suspend point (issue #66).
            if action == Action::Suspend {
                suspend_and_resume(terminal)?;
                continue;
            }
            // Paint what the key set before applying it: an action can block
            // on the network — opening a station connects and reads its
            // headers before anything plays — and a wait that shows nothing
            // reads as a key that did nothing (issue #63). The draw is cheap:
            // ratatui writes only the cells that changed, and the next turn
            // draws the result anyway.
            terminal.draw(|frame| app.draw(frame))?;
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
        settings.mono,
        settings.bit_perfect,
        // The output device the config names (issue #44, S3); `None` is the
        // system's default output.
        settings.audio_device.as_deref(),
        // A `subsonic://<host>/<id>` argument is fetched into the cache and
        // played from there, exactly as the interface does it (T12c). Without
        // a configured server it stays a file, which is what it always was.
        resolver,
        // No interface is drawing: the device error's stderr line stays, the
        // way it printed before the interface existed to own a frame.
        velo::audio::output::Screen::Headless,
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

/// The resolver the session plays through (issue #70): one dispatch over the
/// two kinds of pseudo-path the queue can carry. A `youtube://` row goes to
/// the `YouTube` half, which is always installed — with the tool the entry
/// is resolved when it plays, without it it fails with the install sentence
/// and is never opened as a file — and everything else goes to the Subsonic
/// seams' resolver, or, with no server configured, to the pass-through
/// answers no resolver at all gave. Every answer that is not a
/// `youtube://` row's is exactly what the session heard before it.
struct ComposedResolver {
    youtube: Arc<dyn PathResolver>,
    subsonic: Option<Arc<dyn PathResolver>>,
}

impl PathResolver for ComposedResolver {
    fn resolve(&self, path: &Path) -> Result<PathBuf, AudioError> {
        if velo::youtube::is_pseudo_path(path) {
            return self.youtube.resolve(path);
        }
        self.subsonic
            .as_ref()
            .map_or_else(|| Ok(path.to_path_buf()), |resolver| resolver.resolve(path))
    }

    fn cached(&self, path: &Path) -> bool {
        if velo::youtube::is_pseudo_path(path) {
            return self.youtube.cached(path);
        }
        self.subsonic
            .as_ref()
            .is_none_or(|resolver| resolver.cached(path))
    }

    fn open_streaming(&self, path: &Path) -> Result<Option<Box<dyn MediaSource>>, AudioError> {
        if velo::youtube::is_pseudo_path(path) {
            return self.youtube.open_streaming(path);
        }
        self.subsonic
            .as_ref()
            .map_or_else(|| Ok(None), |resolver| resolver.open_streaming(path))
    }

    fn open_streaming_at(
        &self,
        path: &Path,
        offset: Duration,
    ) -> Result<Option<Box<dyn MediaSource>>, AudioError> {
        if velo::youtube::is_pseudo_path(path) {
            return self.youtube.open_streaming_at(path, offset);
        }
        // Everything else is the Subsonic half's — or, with no server
        // configured, the trait default's: the plain open, the offset
        // already inside a seekable source's own bytes.
        self.subsonic.as_ref().map_or_else(
            || self.open_streaming(path),
            |resolver| resolver.open_streaming_at(path, offset),
        )
    }
}

/// The two tools the chain needs, found once (issue #70, S1): the downloader
/// and the transcoder, each `None` when it is not on `PATH`. The scan is a
/// file read and never a spawn.
fn detect_chain() -> (Option<PathBuf>, Option<PathBuf>) {
    (velo::youtube::detect(), velo::youtube::detect_ffmpeg())
}

/// Composes one session's resolver (issue #70): the Subsonic seams' resolver
/// under it, the `YouTube` half over everything the chain owns. That half is
/// both detected programs in a `resolve::Resolver`, or the degraded one
/// ([`velo::youtube::Unconfigured`]) when a tool is missing — the shape that
/// keeps S10 true, since the config and the queue survive and only `YouTube`
/// is refused. The sentence names whichever tool is missing: a machine with
/// neither needs `yt-dlp` first — the chain starts with it — and one with
/// `yt-dlp` but no `ffmpeg` needs `ffmpeg`.
fn composed_resolver(
    subsonic: Option<Arc<dyn PathResolver>>,
    chain: (Option<PathBuf>, Option<PathBuf>),
) -> Arc<dyn PathResolver> {
    let (youtube_program, ffmpeg_program) = chain;
    let youtube: Arc<dyn PathResolver> = match (youtube_program, ffmpeg_program) {
        (Some(yt_dlp), Some(ffmpeg)) => {
            Arc::new(velo::youtube::resolve::Resolver::new(yt_dlp, ffmpeg))
        }
        (youtube_program, _) => {
            let missing = youtube_program
                .as_ref()
                .map_or(Path::new(velo::youtube::PROGRAM), |_| {
                    Path::new(velo::youtube::FFMPEG)
                });
            Arc::new(velo::youtube::Unconfigured::new(missing))
        }
    };
    Arc::new(ComposedResolver { youtube, subsonic })
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;
    use std::time::Duration;

    use velo::audio::AudioError;

    /// A stand-in for the Subsonic side that records what it was asked and
    /// answers the way a real configured server's resolver behaves: files
    /// and remote rows it answers out of its own rules, and here that
    /// behavior is whatever the closure says.
    struct Side {
        resolve: Answer,
        cached: Verdict,
        streaming: Verdict,
    }

    impl PathResolver for Side {
        fn resolve(&self, path: &Path) -> Result<PathBuf, AudioError> {
            (self.resolve)(path)
        }

        fn cached(&self, path: &Path) -> bool {
            (self.cached)(path)
        }

        fn open_streaming(&self, path: &Path) -> Result<Option<Box<dyn MediaSource>>, AudioError> {
            if (self.streaming)(path) {
                return Ok(Some(Box::new(EmptySource)));
            }
            Ok(None)
        }
    }

    /// The smallest `MediaSource` there is: zero-length, and read from
    /// never. Only its type identity matters here.
    struct EmptySource;

    impl std::io::Read for EmptySource {
        fn read(&mut self, _buffer: &mut [u8]) -> Result<usize, std::io::Error> {
            Ok(0)
        }
    }

    impl std::io::Seek for EmptySource {
        fn seek(&mut self, _: std::io::SeekFrom) -> Result<u64, std::io::Error> {
            Ok(0)
        }
    }

    impl symphonia::core::io::MediaSource for EmptySource {
        fn byte_len(&self) -> Option<u64> {
            Some(0)
        }

        fn is_seekable(&self) -> bool {
            true
        }
    }

    /// What the stub's `resolve` answers.
    type Answer = Box<dyn Fn(&Path) -> Result<PathBuf, AudioError> + Send + Sync>;
    /// What the stub's two questions answer.
    type Verdict = Box<dyn Fn(&Path) -> bool + Send + Sync>;

    fn subsonic_side() -> Arc<dyn PathResolver> {
        Arc::new(Side {
            resolve: Box::new(|path| {
                Ok(PathBuf::from(format!(
                    "the cache file of {}",
                    path.display()
                )))
            }),
            cached: Box::new(|path| !velo::subsonic::cache::is_pseudo_path(path)),
            streaming: Box::new(velo::subsonic::cache::is_pseudo_path),
        })
    }

    /// A stand-in for the `YouTube` half that records the offset every
    /// `open_streaming_at` ask carried, and answers nothing: the dispatch
    /// is pinned by what was asked, never by what a device would play.
    struct YoutubeSide {
        asks: Arc<Mutex<Vec<u64>>>,
    }

    impl PathResolver for YoutubeSide {
        fn resolve(&self, path: &Path) -> Result<PathBuf, AudioError> {
            Err(AudioError::Source(format!(
                "never a file: {}",
                path.display()
            )))
        }

        fn open_streaming(&self, _path: &Path) -> Result<Option<Box<dyn MediaSource>>, AudioError> {
            Ok(None)
        }

        fn open_streaming_at(
            &self,
            _path: &Path,
            offset: Duration,
        ) -> Result<Option<Box<dyn MediaSource>>, AudioError> {
            self.asks
                .lock()
                .expect("the asks lock")
                .push(offset.as_secs());
            Ok(None)
        }
    }

    /// The dispatch is the override's whole job: a `youtube://` row's seek
    /// asks the `YouTube` half's offset form at the offset asked for, and
    /// everything else goes to the Subsonic half's — or, with no server
    /// configured, to the trait default, which is the plain open.
    #[test]
    fn a_youtube_rows_seek_asks_the_offset_form_and_everything_else_does_not() {
        let asks = Arc::new(Mutex::new(Vec::new()));
        let youtube = Arc::new(YoutubeSide {
            asks: Arc::clone(&asks),
        });
        let composed = ComposedResolver {
            youtube,
            subsonic: None,
        };

        composed
            .open_streaming_at(Path::new("youtube://abc123"), Duration::from_secs(83))
            .expect("the ask answers");
        assert_eq!(
            *asks.lock().expect("the asks lock"),
            vec![83],
            "the youtube row's seek asked the offset form at the offset asked for"
        );

        composed
            .open_streaming_at(Path::new("/music/a.flac"), Duration::from_secs(83))
            .expect("the ask answers");
        assert_eq!(
            *asks.lock().expect("the asks lock"),
            vec![83],
            "a non-youtube row never reaches the youtube half"
        );
    }

    /// A row the Subsonic half owns reaches that half's offset form — its
    /// own default, the plain open — and never the `YouTube` half's.
    #[test]
    fn a_subsonic_row_reaches_the_subsonic_half_not_the_youtube_half() {
        let asks = Arc::new(Mutex::new(Vec::new()));
        let youtube = Arc::new(YoutubeSide {
            asks: Arc::clone(&asks),
        });
        let composed = ComposedResolver {
            youtube,
            subsonic: Some(subsonic_side()),
        };

        let asked = composed
            .open_streaming_at(
                Path::new("subsonic://home.server/42"),
                Duration::from_secs(5),
            )
            .expect("the ask answers");
        assert!(
            asked.is_some(),
            "the subsonic half's own streaming answer came back"
        );
        assert!(
            asks.lock().expect("the asks lock").is_empty(),
            "the youtube half was never asked"
        );
    }

    /// S10, the machine without anything: no server configured, no tool
    /// installed. Local files and remote `http(s)://` answers keep exactly
    /// the pass-through answers they had before a resolver existed, and a
    /// `youtube://` row fails with the install sentence instead of being
    /// opened as a file.
    #[test]
    fn with_no_server_and_no_tool_everything_but_youtube_is_untouched() {
        let resolver = composed_resolver(None, (None, None));

        let local = Path::new("/music/one.flac");
        assert_eq!(
            resolver
                .resolve(local)
                .expect("a local file passes through"),
            local,
            "a local file is its own file, as no resolver always left it"
        );
        assert!(
            resolver.cached(local),
            "a local file is already on disk, as no resolver always said"
        );
        assert!(
            resolver
                .open_streaming(local)
                .expect("nothing to stream")
                .is_none(),
            "a local file streams nothing, as no resolver always answered"
        );

        let remote = Path::new("https://radio.example/stream");
        assert_eq!(
            resolver
                .resolve(remote)
                .expect("a remote row passes through"),
            remote,
            "a remote URL is its own file, as no resolver always left it"
        );
        assert!(
            resolver
                .open_streaming(remote)
                .expect("nothing to stream")
                .is_none()
        );

        let youtube = Path::new("youtube://abc123xyz_-_-_-_-_");
        let error = resolver
            .resolve(youtube)
            .expect_err("a youtube row has no file to open");
        assert!(
            error.to_string().contains("yt-dlp is not installed"),
            "the install sentence is what a youtube row fails with: {error}"
        );
        assert!(
            !resolver.cached(youtube),
            "a youtube row is never already reachable"
        );
    }

    /// The missing tool is the one the sentence names: a machine with
    /// `yt-dlp` but no `ffmpeg` is told how to install `ffmpeg` — the
    /// transcoder, not the downloader it already has — and one with neither
    /// is told about `yt-dlp`, the tool the chain starts with.
    #[test]
    fn the_missing_tool_is_the_one_the_sentence_names() {
        let resolver = composed_resolver(
            None,
            (
                Some(PathBuf::from("/velo-test/does-not-exist/yt-dlp")),
                None,
            ),
        );
        let error = resolver
            .resolve(Path::new("youtube://abc123xyz_-_-_-_-_"))
            .expect_err("nothing to play without the transcoder");
        assert!(
            error.to_string().contains("ffmpeg is not installed"),
            "the missing tool is named: {error}"
        );

        let resolver = composed_resolver(
            None,
            (
                None,
                Some(PathBuf::from("/velo-test/does-not-exist/ffmpeg")),
            ),
        );
        let error = resolver
            .resolve(Path::new("youtube://abc123xyz_-_-_-_-_"))
            .expect_err("nothing to play without the downloader");
        assert!(
            error.to_string().contains("yt-dlp is not installed"),
            "the first thing the machine needs is named: {error}"
        );
    }

    /// S10, a machine with a server and the tool: every answer that is not
    /// a `youtube://` row's is the Subsonic side's own, and a `youtube://`
    /// row is the `YouTube` side's — resolved when it plays (its `cached`
    /// says no, so the start's worker takes it), and never a file.
    #[test]
    fn with_both_sides_every_answer_goes_to_the_side_that_owns_the_row() {
        let resolver = composed_resolver(
            Some(subsonic_side()),
            (
                Some(PathBuf::from("/velo-test/does-not-exist/yt-dlp")),
                Some(PathBuf::from("/velo-test/does-not-exist/ffmpeg")),
            ),
        );

        let local = Path::new("/music/one.flac");
        assert_eq!(
            resolver.resolve(local).expect("the file is the side's own"),
            PathBuf::from("the cache file of /music/one.flac"),
        );
        let subsonic_row = Path::new("subsonic://server.example/id");
        assert_eq!(
            resolver
                .resolve(subsonic_row)
                .expect("the row is the server's own"),
            PathBuf::from("the cache file of subsonic://server.example/id"),
        );
        assert!(!resolver.cached(subsonic_row), "the server says so");
        assert!(
            resolver
                .open_streaming(subsonic_row)
                .expect("the ask is the server's own")
                .is_some(),
            "the server streams its own rows"
        );
        assert!(
            resolver
                .open_streaming(local)
                .expect("the ask passes through")
                .is_none(),
        );

        let youtube = Path::new("youtube://abc123xyz");
        assert!(!resolver.cached(youtube), "the youtube side says so");
        // A program that is not there: the entry's resolve fails with the
        // tool's own sentence, which is the point of the always-installed
        // half — never a file open in disguise.
        let error = resolver
            .open_streaming(Path::new("youtube://abc123xyz"))
            .err()
            .expect("a stream that cannot be launched says so");
        assert!(
            error.to_string().contains("yt-dlp is not installed"),
            "the tool's own sentence is what the resolve fails with: {error}"
        );
    }
}
