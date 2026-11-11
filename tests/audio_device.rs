//! The audio output picker (`d`, issue #44): the list, the marking, the
//! selection and the config write, observed over an injected device list —
//! no test enumerates a real machine's devices, and none opens one. That
//! sound actually comes out of the chosen output is the author's ears'
//! evidence; the logbook records it as pending.

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use clap::Parser as _;
use velo::audio::AudioError;
use velo::audio::output::{DeviceList, Entry, PathResolver, device_error_notice, take_oldest};
use velo::config::Cli;
use velo::ui::crossterm::event::{KeyCode, KeyEvent};
use velo::ui::ratatui::Terminal;
use velo::ui::ratatui::backend::TestBackend;
use velo::ui::ratatui::buffer::{Buffer, Cell};
use velo::ui::{Action, App, PlayState};

fn list(names: &[&str], default: Option<&str>) -> impl Fn() -> DeviceList {
    let names: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
    let default = default.map(str::to_owned);
    move || DeviceList {
        names: names.clone(),
        default: default.clone(),
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::from(code)
}

fn press(app: &mut App<'_>, code: KeyCode) {
    let action = app.on_key(key(code));
    app.apply(action);
}

fn render(app: &App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(90, 12)).expect("a test terminal");
    terminal
        .draw(|frame| app.draw(frame))
        .expect("the frame draws");
    let buffer: &Buffer = terminal.backend().buffer();
    buffer
        .content()
        .iter()
        .map(Cell::symbol)
        .collect::<String>()
}

fn temp_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.subsec_nanos());
    let dir = std::env::temp_dir().join(format!(
        "velo-audio-device-{name}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("the temp dir creates");
    dir
}

/// A config file with lines worth keeping, so the surgical write is proven.
fn config_file(dir: &Path) -> PathBuf {
    let path = dir.join("config.toml");
    fs::write(
        &path,
        "bit_perfect = false\n\n[state]\nvolume = 70\nshuffle = false\n",
    )
    .expect("the config writes");
    path
}

/// A queue entry that is a live station: the one kind of start whose open
/// (connect, headers, first bytes) used to be a cut plus silence (issue
/// #65), and the one kind the crossfade handover exists for.
fn station() -> Entry {
    Entry::Station {
        url: "https://example.org/stream".to_owned(),
        name: "Example FM".to_owned(),
    }
}

/// A queue entry that is a file on disk: a start that opens in milliseconds
/// and keeps the plain switch.
fn file_entry() -> Entry {
    Entry::File(std::path::PathBuf::from("/music/track.flac"))
}

/// A queue entry the network has to deliver first: a cliamp channel's song,
/// which the resolver fetches before any player can open it (issue #60).
fn remote_entry() -> Entry {
    Entry::File(std::path::PathBuf::from("https://radio.example/song.mp3"))
}

/// The handover decision (issue #65): a station start over a sounding
/// stream is a crossfade, not a cut — and every start that opens in
/// milliseconds keeps the plain switch. The old source keeps producing
/// until the new one can take over; a file opens in milliseconds and gains
/// nothing from a fade.
#[test]
fn a_station_start_over_a_sounding_stream_is_a_crossfade_not_a_cut() {
    let mut app = App::new(Vec::new());
    app.set_play_state_for_tests(PlayState::Playing);

    assert!(
        app.handover_wanted_for_tests(Some(&station()), false),
        "a station start over a sounding stream crossfades"
    );
    assert!(
        !app.handover_wanted_for_tests(Some(&file_entry()), false),
        "a file start keeps the plain switch — the gapless rule is untouched"
    );
    assert!(
        !app.handover_wanted_for_tests(None, false),
        "a start with no first entry is not a handover"
    );
}

/// A start that has to fetch is the same wait a station's open is (issue
/// #65, issue #60): the stream that is sounding keeps playing while the
/// download runs, and the handover happens when the file is finally on
/// disk — never a cut followed by seconds of silence.
#[test]
fn a_fetch_the_network_owes_keeps_the_sounding_stream() {
    let mut app = App::new(Vec::new());
    app.set_play_state_for_tests(PlayState::Playing);

    assert!(
        app.handover_wanted_for_tests(Some(&remote_entry()), true),
        "an uncached remote track is a download, and the old stream waits it out"
    );
    assert!(
        !app.handover_wanted_for_tests(Some(&remote_entry()), false),
        "a remote file already on disk opens in milliseconds: the plain switch"
    );
}

/// A stream that is not sounding has nothing to hand over from: paused and
/// stopped starts keep today's plain switch, silence and all (issue #65).
#[test]
fn a_stream_that_is_not_sounding_switches_plainly() {
    let mut paused = App::new(Vec::new());
    paused.set_play_state_for_tests(PlayState::Paused);
    assert!(
        !paused.handover_wanted_for_tests(Some(&station()), false),
        "a paused stream is not handed over from"
    );
    assert!(
        !paused.handover_wanted_for_tests(Some(&remote_entry()), true),
        "nor is a paused stream kept through a fetch"
    );

    let stopped = App::new(Vec::new());
    assert!(
        !stopped.handover_wanted_for_tests(Some(&station()), false),
        "a fresh start with no stream to hand over from never fades"
    );
}

/// Bit-perfect refuses the crossfade (issue #65): a fade multiplies
/// samples, and bit-perfect means the decoder's samples reach the device
/// unmodified — so it keeps the plain switch, exactly as it refuses the
/// volume curve and `ReplayGain`.
#[test]
fn bit_perfect_keeps_the_plain_switch_a_fade_never_touches_its_samples() {
    let mut app = App::new(Vec::new());
    app.set_play_state_for_tests(PlayState::Playing);
    app.set_bit_perfect(true);

    assert!(
        !app.handover_wanted_for_tests(Some(&station()), false),
        "bit-perfect takes the plain path, never the fade"
    );
    assert!(
        !app.handover_wanted_for_tests(Some(&remote_entry()), true),
        "bit-perfect cannot keep a second stream alive at another rate"
    );
}

/// A resolver for which every entry is a fetch the network still owes (the
/// seam issue #60 decided on): `cached` answers no, so every remote start
/// waits, and `resolve` answers the path itself, the way a cache hit lands.
struct EveryFetchOwes;

impl PathResolver for EveryFetchOwes {
    fn resolve(&self, path: &Path) -> Result<PathBuf, AudioError> {
        Ok(path.to_path_buf())
    }

    fn cached(&self, _path: &Path) -> bool {
        false
    }
}

/// The failed-fetch answer (`poll_pending_resolve`'s `Err` arm, issue #65):
/// a stream that exists survives the failure — it was never touched, so
/// the line says both facts. One that is sounding keeps playing; one that
/// is paused keeps its pause and can be resumed. Only no stream at all is
/// the plain stop a start that fails on this thread always was.
#[test]
fn a_failed_fetch_spares_the_stream_that_exists() {
    let (line, keeps) = App::fetch_failure_for_tests(Some(PlayState::Playing), "the fetch failed");
    assert!(
        keeps,
        "a stream that is sounding survives a failed fetch untouched"
    );
    assert!(
        line.contains("the stream that was playing goes on"),
        "the line says both facts, got: {line:?}"
    );
    assert!(
        line.starts_with("velo: the fetch failed"),
        "the line names the reason first, got: {line:?}"
    );

    let (line, keeps) = App::fetch_failure_for_tests(Some(PlayState::Paused), "the fetch failed");
    assert!(
        keeps,
        "a paused stream survives a failed fetch too — it can be resumed"
    );
    assert!(
        line.starts_with("velo: the fetch failed"),
        "the line names the reason first, got: {line:?}"
    );
    assert!(
        line.contains("paused"),
        "the line says the stream is still there, paused, got: {line:?}"
    );
    assert!(
        line.contains("resumed"),
        "the line says the paused stream can be resumed, got: {line:?}"
    );
    assert!(
        line.chars().all(|c| !c.is_ascii_digit()),
        "the line carries no number — no countdown, no deadline: got {line:?}"
    );

    let (line, keeps) = App::fetch_failure_for_tests(None, "the fetch failed");
    assert!(!keeps, "no stream at all is the plain stop");
    assert_eq!(line, "velo: the fetch failed");
}

/// The waiting line of a handover whose new stream has not sounded yet:
/// it names the entry being waited for and says the stream that was
/// playing goes on — and carries no timer, no counter and no deadline,
/// because the wait may be as long as a slow connection takes.
#[test]
fn the_waiting_line_names_the_entry_and_says_the_old_stream_goes_on() {
    let line = App::handover_waiting_line_for_tests(Some("Example FM"))
        .expect("a waiting handover has an entry to name");
    assert!(
        line.contains("Example FM"),
        "the line names the entry being waited for, got: {line:?}"
    );
    assert!(
        line.contains("the stream that was playing goes on"),
        "the line says the old stream plays on, got: {line:?}"
    );
    assert!(
        line.chars().all(|c| !c.is_ascii_digit()),
        "the line carries no number — no countdown, no seconds left: got {line:?}"
    );
    assert!(
        App::handover_waiting_line_for_tests(None).is_none(),
        "no entry to name, no line"
    );
}

/// The pause guard at the handover's open (issue #65, L35): a paused
/// stream holds its fade where it was, so the old player is stopped
/// outright instead of being faded out — a paused fade would never
/// advance and would strand the player in the handover's slot.
#[test]
fn a_pause_at_the_open_stops_the_old_stream_instead_of_fading() {
    assert!(
        App::handover_fades_under_for_tests(PlayState::Playing),
        "a sounding stream is faded out, the ordinary handover"
    );
    assert!(
        !App::handover_fades_under_for_tests(PlayState::Paused),
        "a paused stream is stopped outright — its fade would never advance"
    );
}

/// The handover answer travels with the start across its fetch (issue
/// #65): the decision is made once, before the old stream could have
/// changed, and carried in the pending start — never re-derived at the
/// open, where the wait's answer would be a different one.
#[test]
fn the_handover_answer_is_carried_across_a_fetch() {
    let mut app = App::new(Vec::new());
    app.set_play_state_for_tests(PlayState::Playing);
    app.queue_song_for_tests("https://radio.example/song.mp3");
    app.set_resolver(Some(Arc::new(EveryFetchOwes)));

    app.apply(Action::PlayQueue { from: 0 });

    assert_eq!(
        app.pending_start_handover_for_tests(),
        Some(true),
        "a sounding stream's fetch carries the handover answer across the wait"
    );

    let mut paused = App::new(Vec::new());
    paused.set_play_state_for_tests(PlayState::Paused);
    paused.queue_song_for_tests("https://radio.example/song.mp3");
    paused.set_resolver(Some(Arc::new(EveryFetchOwes)));

    paused.apply(Action::PlayQueue { from: 0 });

    assert_eq!(
        paused.pending_start_handover_for_tests(),
        Some(false),
        "a paused stream's fetch carries the plain answer across the wait"
    );
}

/// A device error is said in the app's own words — the same family of fact
/// as "audio device \"X\" is not present — using the default output", the
/// sentence the interface puts on its status line instead of the stderr
/// write that used to splice into the frame it was drawing.
#[test]
fn a_device_error_is_said_in_the_app_s_own_words() {
    assert_eq!(
        device_error_notice("Device sample rate changed"),
        "audio device error: Device sample rate changed",
        "the sentence names the device problem, cpal's own fact inside it"
    );
}

/// The take is the once: an empty notice queue answers nothing twice over —
/// taking never invents a message — and a message that was taken is gone,
/// so the same fact is never said twice on the status line.
#[test]
fn an_empty_notice_queue_stays_empty_and_a_notice_is_taken_once() {
    let notices = Mutex::new(VecDeque::new());
    assert_eq!(take_oldest(&notices), None, "empty stays empty");
    assert_eq!(
        take_oldest(&notices),
        None,
        "still empty: nothing was invented"
    );

    notices
        .lock()
        .expect("the test queue locks")
        .push_back("bit-perfect unavailable — the device has no 44100 Hz output".to_owned());
    assert_eq!(
        take_oldest(&notices).as_deref(),
        Some("bit-perfect unavailable — the device has no 44100 Hz output"),
        "the notice is answered"
    );
    assert_eq!(take_oldest(&notices), None, "taken once: it is gone");
}

#[test]
fn d_opens_a_picker_listing_the_machine_s_outputs() {
    let mut app = App::new(Vec::new());
    app.set_device_lister(list(&["Speakers", "Headphones", "HDMI"], Some("Speakers")));

    press(&mut app, KeyCode::Char('d'));

    assert!(app.device_picker_open(), "`d` must open the device picker");
    assert_eq!(
        app.device_picker_names(),
        ["Speakers", "Headphones", "HDMI"],
        "the picker lists every output the lister offered"
    );
}

#[test]
fn a_machine_with_one_device_still_lists_it() {
    let mut app = App::new(Vec::new());
    app.set_device_lister(list(&["Speakers"], Some("Speakers")));

    press(&mut app, KeyCode::Char('d'));

    assert!(app.device_picker_open());
    assert_eq!(app.device_picker_names(), ["Speakers"]);
}

#[test]
fn the_picker_marks_the_device_in_use_and_starts_on_it() {
    let mut app = App::new(Vec::new());
    app.set_device_lister(list(&["Speakers", "Headphones"], Some("Speakers")));
    app.set_audio_device(Some("Headphones".to_owned()));

    press(&mut app, KeyCode::Char('d'));

    assert_eq!(app.device_picker_cursor(), 1, "the cursor starts on it");
    assert_eq!(
        app.device_picker_in_use().as_deref(),
        Some("Headphones"),
        "the in-use device is known"
    );
    assert!(
        render(&app).contains("● Headphones"),
        "the drawing marks the in-use row"
    );
}

#[test]
fn j_k_and_the_arrows_move_and_wrap() {
    let mut app = App::new(Vec::new());
    app.set_device_lister(list(&["Speakers", "Headphones", "HDMI"], Some("Speakers")));
    press(&mut app, KeyCode::Char('d'));

    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.device_picker_cursor(), 1);
    press(&mut app, KeyCode::Down);
    assert_eq!(app.device_picker_cursor(), 2);
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.device_picker_cursor(), 0, "`j` wraps at the end");
    press(&mut app, KeyCode::Char('k'));
    assert_eq!(app.device_picker_cursor(), 2, "`k` wraps at the start");
    press(&mut app, KeyCode::Up);
    assert_eq!(app.device_picker_cursor(), 1);
}

#[test]
fn esc_closes_without_changing_anything() {
    let dir = temp_dir("esc");
    let path = config_file(&dir);
    let before = fs::read_to_string(&path).expect("the config reads");
    let mut app = App::new(Vec::new());
    app.set_device_lister(list(&["Speakers", "Headphones"], Some("Speakers")));
    app.set_config_file(Some(path.clone()));
    press(&mut app, KeyCode::Char('d'));
    press(&mut app, KeyCode::Char('j'));

    press(&mut app, KeyCode::Esc);

    assert!(!app.device_picker_open(), "`esc` closes the picker");
    assert_eq!(app.audio_device(), None, "the session keeps its device");
    assert_eq!(
        fs::read_to_string(&path).expect("the config reads"),
        before,
        "`esc` writes nothing"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn enter_switches_and_writes_the_choice_into_the_config() {
    let dir = temp_dir("enter");
    let path = config_file(&dir);
    let mut app = App::new(Vec::new());
    app.set_device_lister(list(&["Speakers", "Headphones"], Some("Speakers")));
    app.set_config_file(Some(path.clone()));
    press(&mut app, KeyCode::Char('d'));
    press(&mut app, KeyCode::Char('j'));

    press(&mut app, KeyCode::Enter);

    assert!(!app.device_picker_open(), "`enter` closes the picker");
    assert_eq!(
        app.audio_device(),
        Some("Headphones"),
        "the session opens the chosen device from now on"
    );
    assert!(
        app.status().contains("Headphones"),
        "the switch says where the sound went, got: {:?}",
        app.status()
    );
    let after = fs::read_to_string(&path).expect("the config reads");
    assert!(
        after.contains("[audio]\ndevice = \"Headphones\""),
        "the choice is remembered as [audio] device:\n{after}"
    );
    assert!(
        after.contains("bit_perfect = false\n") && after.contains("volume = 70\n"),
        "the surgical write keeps every other line:\n{after}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_choice_is_written_only_when_it_changed() {
    let dir = temp_dir("changed");
    let path = config_file(&dir);
    let mut app = App::new(Vec::new());
    app.set_device_lister(list(&["Speakers", "Headphones"], Some("Speakers")));
    app.set_audio_device(Some("Speakers".to_owned()));
    app.set_config_file(Some(path.clone()));
    let before = fs::read_to_string(&path).expect("the config reads");
    press(&mut app, KeyCode::Char('d'));

    press(&mut app, KeyCode::Enter);

    assert_eq!(app.audio_device(), Some("Speakers"));
    assert_eq!(
        fs::read_to_string(&path).expect("the config reads"),
        before,
        "switching to the device already in use writes nothing"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn without_a_config_file_the_switch_still_happens() {
    let mut app = App::new(Vec::new());
    app.set_device_lister(list(&["Speakers", "Headphones"], Some("Speakers")));
    press(&mut app, KeyCode::Char('d'));
    press(&mut app, KeyCode::Char('j'));

    press(&mut app, KeyCode::Enter);

    assert_eq!(
        app.audio_device(),
        Some("Headphones"),
        "the choice is the session's even with nowhere to write it"
    );
}

#[test]
fn a_machine_without_outputs_says_so_instead_of_opening_an_empty_card() {
    let mut app = App::new(Vec::new());
    app.set_device_lister(list(&[], None));

    press(&mut app, KeyCode::Char('d'));

    assert!(!app.device_picker_open());
    assert!(
        app.status().contains("no audio output"),
        "the refusal says why, got: {:?}",
        app.status()
    );
}

#[test]
fn the_config_s_audio_table_selects_the_launch_device() {
    let dir = temp_dir("parse");
    let path = config_file(&dir);
    let text = fs::read_to_string(&path).expect("the config reads")
        + "\n[audio]\ndevice = \"Headphones\"\n";
    fs::write(&path, text).expect("the config writes");

    let settings = velo::config::Config::load_from(&path)
        .expect("the [audio] table parses")
        .merged(&Cli::parse_from(["velo"]));

    assert_eq!(
        settings.audio_device.as_deref(),
        Some("Headphones"),
        "a launch with a configured device opens it"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn an_empty_device_key_is_no_device() {
    let dir = temp_dir("blank");
    let path = config_file(&dir);
    let text =
        fs::read_to_string(&path).expect("the config reads") + "\n[audio]\ndevice = \"  \"\n";
    fs::write(&path, text).expect("the config writes");

    let settings = velo::config::Config::load_from(&path)
        .expect("the [audio] table parses")
        .merged(&Cli::parse_from(["velo"]));

    assert_eq!(
        settings.audio_device, None,
        "a blank name is the default output, the way a blank theme name is"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_lister_is_asked_once_per_opening() {
    static ASKS: AtomicUsize = AtomicUsize::new(0);
    let mut app = App::new(Vec::new());
    app.set_device_lister(|| {
        ASKS.fetch_add(1, Ordering::Relaxed);
        DeviceList {
            names: vec!["Speakers".to_owned()],
            default: Some("Speakers".to_owned()),
        }
    });

    press(&mut app, KeyCode::Char('d'));

    assert_eq!(
        ASKS.load(Ordering::Relaxed),
        1,
        "opening the picker asks the machine once"
    );
}
