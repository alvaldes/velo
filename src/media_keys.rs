//! Media keys (issue #12): velo as a now-playing client of the operating
//! system.
//!
//! The OS's play/pause, next and previous reach the same actions the
//! interface keys already perform (S4), and velo publishes what is playing
//! — title and artist for a track, the station's own name (or the ICY title
//! it is announcing) for a live stream, nothing when nothing plays (S5).
//!
//! The whole client is best effort (S6): it lives on its own thread, every
//! fallible call is answered with "ignored", a media session that refuses
//! or is absent costs at most one status line and never a wait, and a tick
//! only ever drains a channel — no network, no polling inside the loop.
//! Nothing here touches the keyboard: the media keys are the OS's, not the
//! terminal's (S7).

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use souvlaki::{MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, PlatformConfig};

/// What the OS media keys ask of velo (S4): the three commands the
/// interface keys already answer. Everything else the OS may send — seek,
/// volume, quit — is not velo's to take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaCommand {
    /// Play, pause, or whatever the OS's toggle means today: the space
    /// key's action.
    PlayPause,
    /// The `n` key's action.
    Next,
    /// The `p` key's action.
    Previous,
}

/// What velo publishes about one thing playing (S5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NowPlaying {
    /// A track's title — its tags when the index knows them, its file name
    /// when it does not — or the station's ICY title, else its own name.
    pub title: String,
    /// The track's artist when the index knows one; never invented. A
    /// station has none.
    pub artist: Option<String>,
    /// The track's length when it is known; a live station has none.
    pub duration: Option<Duration>,
}

/// The whole answer to "what is the OS allowed to show right now?" (S5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaState {
    /// What plays, or `None` when nothing does — the OS then shows nothing.
    pub now: Option<NowPlaying>,
    /// Whether it is playing, as opposed to paused.
    pub playing: bool,
}

/// Maps one OS media event onto velo's command for it (S4). Events the
/// interface keys have no answer for — seek, volume, quit — are left for
/// the OS to forget: `None`.
#[must_use]
pub fn command_of(event: &MediaControlEvent) -> Option<MediaCommand> {
    match event {
        MediaControlEvent::Play | MediaControlEvent::Pause | MediaControlEvent::Toggle => {
            Some(MediaCommand::PlayPause)
        }
        MediaControlEvent::Next => Some(MediaCommand::Next),
        MediaControlEvent::Previous => Some(MediaCommand::Previous),
        MediaControlEvent::Stop
        | MediaControlEvent::Seek(_)
        | MediaControlEvent::SeekBy(_, _)
        | MediaControlEvent::SetPosition(_)
        | MediaControlEvent::SetVolume(_)
        | MediaControlEvent::OpenUri(_)
        | MediaControlEvent::Raise
        | MediaControlEvent::Quit => None,
    }
}

/// The interface action a media command stands for (S4): the same actions
/// the keys already perform, through the same [`crate::ui::App::apply`].
#[must_use]
pub fn action_of(command: MediaCommand) -> crate::ui::Action {
    match command {
        MediaCommand::PlayPause => crate::ui::Action::TogglePause,
        MediaCommand::Next => crate::ui::Action::Next,
        MediaCommand::Previous => crate::ui::Action::Previous,
    }
}

/// The interface's handle on the media session: commands arrive from it and
/// what is playing goes to it, both across channels the tick drains (S6) —
/// the thread behind them is the only one that talks to the OS.
#[derive(Debug)]
pub struct MediaKeys {
    updates: Sender<MediaState>,
    commands: Receiver<MediaCommand>,
    /// What starting the client had to say, taken at most once (S6): one
    /// status line, or nothing.
    init: Receiver<Result<(), String>>,
    init_taken: bool,
}

impl MediaKeys {
    /// Starts the client on its own thread. The thread creation is the only
    /// failure the caller hears about; everything the OS client itself can
    /// refuse is said once, through [`MediaKeys::take_init_error`], and a
    /// client that never starts costs a session nothing but its silence.
    ///
    /// # Errors
    ///
    /// Fails when the client's thread cannot be spawned, which is not a
    /// reason to refuse a session: the caller says one line and moves on.
    pub fn spawn() -> Result<Self, String> {
        let (updates_tx, updates_rx) = mpsc::channel();
        let (commands_tx, commands_rx) = mpsc::channel();
        let (init_tx, init_rx) = mpsc::channel();
        let handle = std::thread::Builder::new()
            .name("velo-media-keys".to_owned())
            .spawn(move || worker(updates_rx, commands_tx, init_tx))
            .map_err(|error| format!("cannot start the media-keys thread: {error}"))?;
        // Dropping the handle detaches the thread: it serves the session
        // for the interface's length and ends when the channels close.
        drop(handle);
        Ok(Self {
            updates: updates_tx,
            commands: commands_rx,
            init: init_rx,
            init_taken: false,
        })
    }

    /// What starting the client had to say, at most once (S6). An error
    /// here is a status line for the user, not a failure of the session.
    pub fn take_init_error(&mut self) -> Option<String> {
        if self.init_taken {
            return None;
        }
        self.init_taken = true;
        match self.init.try_recv() {
            Ok(Err(message)) => Some(message),
            _ => None,
        }
    }

    /// The commands the OS sent since the last drain, in order. Never
    /// blocks: a silent session answers an empty list.
    #[must_use]
    pub fn drain_commands(&self) -> Vec<MediaCommand> {
        let mut commands = Vec::new();
        while let Ok(command) = self.commands.try_recv() {
            commands.push(command);
        }
        commands
    }

    /// Publishes the state of the session to the OS. Ignored when the
    /// client is gone: what the OS shows is nobody's playback.
    pub fn publish(&self, state: &MediaState) {
        let _ = self.updates.send(state.clone());
    }

    /// A pair wired to plain channels instead of an OS client: the tests
    /// inject commands and observe what would be published, without a
    /// media session (S6's contract, exercised headlessly).
    #[doc(hidden)]
    #[must_use]
    #[expect(
        clippy::type_complexity,
        reason = "the tests' loopback wiring is one tuple of channels by design"
    )]
    pub fn loopback() -> (
        Self,
        Sender<MediaCommand>,
        Receiver<MediaState>,
        Sender<Result<(), String>>,
    ) {
        let (updates_tx, updates_rx) = mpsc::channel();
        let (commands_tx, commands_rx) = mpsc::channel();
        let (init_tx, init_rx) = mpsc::channel();
        (
            Self {
                updates: updates_tx,
                commands: commands_rx,
                init: init_rx,
                init_taken: false,
            },
            commands_tx,
            updates_rx,
            init_tx,
        )
    }
}

/// The client's thread (S6): registers velo as a now-playing client, then
/// serves published state for as long as the session sends any. Every
/// fallible step is answered and ignored — a client that cannot start, a
/// handler that cannot attach, a status the OS refuses — so a launch that
/// cannot have media keys is a launch without them, not a launch that
/// failed. The thread detaches the handler when the channel closes, which
/// is the session ending.
fn worker(
    updates: Receiver<MediaState>,
    commands: Sender<MediaCommand>,
    init: Sender<Result<(), String>>,
) {
    let config = PlatformConfig {
        dbus_name: "velo",
        display_name: "velo",
        hwnd: None,
    };
    let refuse = move |why: &str| {
        let _ = init.send(Err(format!(
            "media keys unavailable ({why}) — the keyboard keeps the keys"
        )));
    };
    let mut controls = match MediaControls::new(config) {
        Ok(controls) => controls,
        Err(error) => {
            refuse(&error.to_string());
            return;
        }
    };
    let handler = move |event: MediaControlEvent| {
        if let Some(command) = command_of(&event) {
            let _ = commands.send(command);
        }
    };
    if let Err(error) = controls.attach(handler) {
        refuse(&error.to_string());
        return;
    }
    for state in updates {
        match &state.now {
            Some(now) => {
                let metadata = MediaMetadata {
                    title: Some(&now.title),
                    album: None,
                    artist: now.artist.as_deref(),
                    cover_url: None,
                    duration: now.duration,
                };
                let _ = controls.set_metadata(metadata);
                let playback = if state.playing {
                    MediaPlayback::Playing { progress: None }
                } else {
                    MediaPlayback::Paused { progress: None }
                };
                let _ = controls.set_playback(playback);
            }
            // Nothing plays: the OS shows nothing.
            None => {
                let _ = controls.set_playback(MediaPlayback::Stopped);
            }
        }
    }
    let _ = controls.detach();
}
