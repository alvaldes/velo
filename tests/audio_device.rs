//! The audio output picker (`d`, issue #44): the list, the marking, the
//! selection and the config write, observed over an injected device list —
//! no test enumerates a real machine's devices, and none opens one. That
//! sound actually comes out of the chosen output is the author's ears'
//! evidence; the logbook records it as pending.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use clap::Parser as _;
use velo::audio::output::DeviceList;
use velo::config::Cli;
use velo::ui::App;
use velo::ui::crossterm::event::{KeyCode, KeyEvent};
use velo::ui::ratatui::Terminal;
use velo::ui::ratatui::backend::TestBackend;
use velo::ui::ratatui::buffer::{Buffer, Cell};

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
