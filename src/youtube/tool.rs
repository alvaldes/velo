//! One external process — and one chain of two — bounded: the runner every
//! `yt-dlp` call goes through.
//!
//! Nothing else in the tree captures a child's standard output, imposes a
//! deadline or kills a stuck child — `open_url` is fire-and-forget — so the
//! rules live here, once, and every one of them is a rule the interface
//! depends on. The play path runs a **chain**: `yt-dlp` streams the best
//! audio it can get, and a second child, `ffmpeg`, decodes whatever that is
//! and writes MP3 to stdout. Both children share one process group — velo
//! puts `ffmpeg` into `yt-dlp`'s own group — because a kill that stops at
//! one stage leaves the other holding the pipe and the call waiting on it
//! forever.

use std::ffi::OsString;
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

/// How often the child is asked whether it has exited. Short enough that a
/// deadline is not noticeably longer than the bound it names, long enough that
/// a waiting parent costs nothing.
const POLL: Duration = Duration::from_millis(10);

/// How long a search may take: ten entries off the search page, no extraction.
pub const SEARCH: Deadline = Deadline::new(Duration::from_secs(20));

/// How long one play-time resolve may take: the chain starting and its
/// pre-buffer filling. A stream's first byte is the fill's first read, so one
/// bound governs both — the same register of bound the search names, and the
/// bound a stalled stream misses.
pub const RESOLVE: Deadline = Deadline::new(Duration::from_secs(30));

/// How many bytes are read into memory before the stream is handed to the
/// decoder: 256 KiB, about sixteen seconds of 128 kbps MP3. The pipe is
/// filled ahead of playback so that a hiccup in the tool's delivery — a busy
/// machine, a slow network — does not empty the device's buffer and underrun
/// (`A buffer underrun or overrun occurred.` was observed without it). It is
/// a floor, not a measured optimum: enough that a busy machine keeps
/// playing, and small enough that filling it stays inside [`RESOLVE`].
pub const PREBUFFER: usize = 256 * 1024;

/// How long a child may take before it is killed with its children.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Deadline(Duration);

impl Deadline {
    /// A bound of `after`.
    #[must_use]
    pub const fn new(after: Duration) -> Self {
        Self(after)
    }

    /// The bound itself.
    #[must_use]
    pub const fn after(self) -> Duration {
        self.0
    }
}

/// Why a call produced no output.
#[derive(Debug)]
pub enum ToolError {
    /// The program is not where it was looked for.
    NotInstalled(PathBuf),
    /// The program outlived its deadline and was killed with its children.
    Deadline {
        /// The program's file name, for the sentence.
        program: String,
        /// The bound it missed.
        after: Duration,
    },
    /// The program ran and refused, in its own words.
    Failed {
        /// The program's file name, for the sentence.
        program: String,
        /// Its exit code, when it exited rather than died of a signal.
        status: Option<i32>,
        /// The first line it wrote — what a user can act on.
        line: String,
    },
    /// The stream never filled its pre-buffer: no first byte, or fewer than
    /// the fill, and no end, within [`RESOLVE`]. The honest sentence names
    /// the stall — it never looks like an empty track.
    Stalled {
        /// The program whose bytes were awaited (`ffmpeg` — the chain's
        /// second stage, whose bytes are what the decoder reads).
        program: String,
        /// The bound the fill missed.
        after: Duration,
    },
    /// The program could not be run, or became unmanageable once it was.
    Unusable {
        /// The program's file name, for the sentence.
        program: String,
        /// What the operating system said.
        message: String,
    },
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotInstalled(path) => {
                // The sentence names the tool that is missing: `yt-dlp` when
                // the downloader is not there, `ffmpeg` when only the
                // transcoder is — a machine that has one must not be told to
                // install it again.
                let name = file_name(path);
                write!(
                    f,
                    "{name} is not installed — install it to play YouTube (brew install {name}, \
                     or your package manager)"
                )
            }
            Self::Deadline { program, after } => write!(
                f,
                "{program} did not answer within {}s — the request was cancelled",
                after.as_secs()
            ),
            Self::Stalled { program, after } => write!(
                f,
                "{program} did not deliver its first {} KiB within {}s — the stream stalled \
                 before anything could play",
                PREBUFFER / 1024,
                after.as_secs()
            ),
            Self::Failed {
                program,
                status,
                line,
            } => match status {
                Some(code) => write!(f, "{program} could not answer: {line} (exit {code})"),
                None => write!(f, "{program} could not answer: {line}"),
            },
            Self::Unusable { program, message } => {
                write!(f, "{program} could not run: {message}")
            }
        }
    }
}

impl std::error::Error for ToolError {}

/// Runs `program` with `args` and returns its standard output.
///
/// Arguments are passed as arguments: nothing here goes through a shell, so a
/// query holding quotes, `;` or a leading `-` is a query and never a command,
/// and the child never reads a keystroke meant for the interface.
///
/// # Errors
///
/// [`ToolError::NotInstalled`] when there is no such program,
/// [`ToolError::Deadline`] when it misses `deadline` — killed with its process
/// group, so a child of its own cannot keep the pipe open — [`ToolError::Failed`]
/// when it exits non-zero, and [`ToolError::Unusable`] when it cannot be started.
pub fn run(program: &Path, args: &[OsString], deadline: Deadline) -> Result<String, ToolError> {
    let name = file_name(program);
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    own_group(&mut command);

    let mut child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            ToolError::NotInstalled(program.to_path_buf())
        } else {
            ToolError::Unusable {
                program: name.clone(),
                message: error.to_string(),
            }
        }
    })?;

    // Both pipes are drained from the start: a child whose output outgrows the
    // pipe buffer blocks forever otherwise, and that hang would be ours.
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let until = Instant::now() + deadline.after();

    let Some(status) = wait(&mut child, until, &name)? else {
        return Err(expired(&mut child, name, deadline.after()));
    };

    // The status and both pipes share one budget: a grandchild that keeps
    // stdout open is a call that missed its deadline, not a call that waits.
    let stdout = match collect(&out, until) {
        Collected::Bytes(bytes) => bytes,
        Collected::ReadFailed(error) => {
            end_all(&mut [&mut child]);
            return Err(ToolError::Unusable {
                program: name,
                message: format!("its stdout could not be read: {error}"),
            });
        }
        Collected::TimedOut => return Err(expired(&mut child, name, deadline.after())),
    };
    let stderr = match collect(&err, until) {
        Collected::Bytes(bytes) => bytes,
        Collected::ReadFailed(error) => {
            end_all(&mut [&mut child]);
            return Err(ToolError::Unusable {
                program: name,
                message: format!("its stderr could not be read: {error}"),
            });
        }
        Collected::TimedOut => return Err(expired(&mut child, name, deadline.after())),
    };

    if status.success() {
        return Ok(String::from_utf8_lossy(&stdout).into_owned());
    }
    Err(ToolError::Failed {
        program: name,
        status: status.code(),
        line: reason(&stdout, &stderr),
    })
}

/// Spawns the play path's two-stage chain and hands its stream over without
/// waiting for it to end: `yt_dlp` streams the best audio it can get, and
/// `ffmpeg` — whose stdin is the tool's own stdout (`Stdio::from`), so no
/// pipe of ours sits in the middle and no byte is copied — decodes whatever
/// that is and writes MP3 to stdout. The caller reads that stream as it
/// arrives — the shape the play path needs, where the tool itself delivers
/// the bytes (issue #70, S4 as revised) and a bounded wait would be a
/// deadline on the music.
///
/// Both children are put into **one process group**: `yt_dlp` gets a group of
/// its own and `ffmpeg` joins it, so [`Running`]'s drop — and every deadline
/// and failure path — kills the pair with one group kill. A grandchild either
/// leaves behind dies with the group too.
///
/// Before the handover, the second stage's stdout is read into memory up to
/// [`PREBUFFER`] — the pre-buffer that keeps a hiccup from emptying the
/// device — under `deadline`, the [`RESOLVE`] register: a stream's first byte
/// is the fill's first read, so until bytes arrive a child's exit is still
/// the verdict, and the sentence quotes the failing tool's own stderr line
/// (`yt-dlp` when the downloader failed, `ffmpeg` when only the transcoder
/// did). A stream shorter than the fill hands over everything it had when it
/// ends. One that never fills within the bound — stalled, not ended — is
/// killed with the group and answers [`ToolError::Stalled`], naming the
/// miss, never looking like an empty track.
///
/// A child that cannot start is [`ToolError::NotInstalled`] — the sentence
/// names whichever tool is missing; a spawn failure is
/// [`ToolError::Unusable`], as in [`run`].
///
/// # Panics
///
/// When the downloader's stdout pipe is missing at the second stage's spawn
/// — a programming error, since [`spawn`] asked for it as a pipe; neither
/// this nor anything else here can happen through the public surface.
///
/// # Errors
///
/// [`ToolError::NotInstalled`], [`ToolError::Stalled`] (the pre-buffer never
/// filled within `deadline`), [`ToolError::Failed`] (an exit before the
/// first byte), or [`ToolError::Unusable`].
pub fn spawn(
    yt_dlp: &Path,
    yt_args: &[OsString],
    ffmpeg: &Path,
    ffmpeg_args: &[OsString],
    deadline: Deadline,
) -> Result<Running, ToolError> {
    let yt_name = file_name(yt_dlp);
    let ff_name = file_name(ffmpeg);

    // Stage one: the downloader, in a process group of its own — the group
    // both stages share, so one kill takes the pair and anything they start.
    let mut yt_command = Command::new(yt_dlp);
    yt_command
        .args(yt_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    own_group(&mut yt_command);
    let mut yt = spawn_command(&mut yt_command, yt_dlp)?;

    // Stage two: the transcoder. Its stdin is the tool's stdout, and it
    // joins the tool's process group — a grandchild of the tool and the
    // transcoder are the same kind of leak, and the group is what stops
    // both.
    let tool_out = yt
        .stdout
        .take()
        .expect("the tool's stdout was asked for as a pipe");
    let mut ff_command = Command::new(ffmpeg);
    ff_command
        .args(ffmpeg_args)
        .stdin(Stdio::from(tool_out))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    join_group(&mut ff_command, yt.id());
    let mut ff = match spawn_command(&mut ff_command, ffmpeg) {
        Ok(ff) => ff,
        Err(error) => {
            end_all(&mut [&mut yt]);
            return Err(error);
        }
    };

    // stderr of both is drained from the start, the way `run` drains it: a
    // chatty tool must never block on a full pipe, and the drains are what
    // an immediate failure quotes.
    let yt_err = drain(yt.stderr.take());
    let ff_err = drain(ff.stderr.take());

    // One thread reads the transcoder's stdout until the pre-buffer fills or
    // the stream ends — the first read is the first-byte watch, the rest is
    // the fill — and hands the pipe straight back. A stream shorter than the
    // fill hands over everything it had when it ends; the bytes read before
    // the handover are none lost, they ride in [`Running`].
    let receiver = fill_watch(ff.stdout.take());
    let until = Instant::now() + deadline.after();

    loop {
        match receiver.try_recv() {
            Ok((Ok(prefix), stdout)) if !prefix.is_empty() => {
                return Ok(Running {
                    yt_dlp: yt,
                    ffmpeg: ff,
                    stdout,
                    prefix,
                });
            }
            // An empty read — EOF before a byte — and a read error are left
            // to the exit statuses and the deadline: the pipe, if it came
            // back, is dropped here and its verdict is the children's.
            Ok(_) | Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => {}
        }
        let yt_status = match yt.try_wait() {
            Ok(status) => status,
            Err(error) => {
                end_all(&mut [&mut yt, &mut ff]);
                return Err(ToolError::Unusable {
                    program: yt_name,
                    message: error.to_string(),
                });
            }
        };
        let ff_status = match ff.try_wait() {
            Ok(status) => status,
            Err(error) => {
                end_all(&mut [&mut yt, &mut ff]);
                return Err(ToolError::Unusable {
                    program: ff_name,
                    message: error.to_string(),
                });
            }
        };
        if let (Some(yt_status), Some(ff_status)) = (yt_status, ff_status) {
            // The exits and the fill race: give the reader thread a bounded
            // moment to hand back what it read before the exits are called
            // the verdict.
            if let Ok((Ok(prefix), stdout)) = receiver.recv_timeout(Duration::from_secs(1))
                && !prefix.is_empty()
            {
                return Ok(Running {
                    yt_dlp: yt,
                    ffmpeg: ff,
                    stdout,
                    prefix,
                });
            }
            return Err(exit_verdict(
                yt_status, ff_status, &yt_name, &ff_name, &yt_err, &ff_err,
            ));
        }
        if Instant::now() >= until {
            end_all(&mut [&mut yt, &mut ff]);
            return Err(ToolError::Stalled {
                program: ff_name,
                after: deadline.after(),
            });
        }
        thread::sleep(POLL);
    }
}

/// Spawns the watcher of the transcoder's stdout: one thread reads until the
/// pre-buffer fills or the stream ends and hands the bytes — and the pipe,
/// for the caller to keep streaming — back once.
fn fill_watch(
    pipe: Option<ChildStdout>,
) -> Receiver<(std::io::Result<Vec<u8>>, Option<ChildStdout>)> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let Some(mut pipe) = pipe else {
            let _ = sender.send((Err(std::io::Error::other("stdout was not piped")), None));
            return;
        };
        let mut fill = Vec::new();
        let mut chunk = [0u8; 8192];
        let result = loop {
            match pipe.read(&mut chunk) {
                Ok(0) => break Ok(fill), // the stream ended: hand over what it had
                Ok(read) => {
                    fill.extend_from_slice(&chunk[..read]);
                    if fill.len() >= PREBUFFER {
                        break Ok(fill); // the floor is reached: hand over and stream the rest
                    }
                }
                Err(error) => break Err(error),
            }
        };
        let _ = sender.send((result, Some(pipe))); // the pipe comes back for the caller
    });
    receiver
}

/// Spawns a prepared command, mapping the two spawn failures the runner
/// reports: a program that is not there, and a program that could not be
/// started.
fn spawn_command(command: &mut Command, program: &Path) -> Result<Child, ToolError> {
    command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            ToolError::NotInstalled(program.to_path_buf())
        } else {
            ToolError::Unusable {
                program: file_name(program),
                message: error.to_string(),
            }
        }
    })
}

/// The verdict once both stages have exited and no byte came back: the
/// failing tool's own stderr line — the downloader's when it failed, the
/// transcoder's when the downloader did not — is the reason a user can act
/// on, never what a decoder made of an empty pipe. Two clean exits with no
/// bytes are a failure all the same: an empty stream is not a track.
fn exit_verdict(
    yt_status: ExitStatus,
    ff_status: ExitStatus,
    yt_name: &str,
    ff_name: &str,
    yt_err: &Receiver<std::io::Result<Vec<u8>>>,
    ff_err: &Receiver<std::io::Result<Vec<u8>>>,
) -> ToolError {
    let (program, status, err) = if !yt_status.success() {
        (yt_name, yt_status.code(), yt_err)
    } else if !ff_status.success() {
        (ff_name, ff_status.code(), ff_err)
    } else {
        // Both exits were clean and nothing was delivered: the transcoder's
        // words are the closest reason there is.
        (ff_name, None, ff_err)
    };
    let stderr = match collect(err, Instant::now() + Duration::from_secs(1)) {
        Collected::Bytes(bytes) => bytes,
        // A stderr read failure or a bound that ran out leaves nothing to
        // quote: the tool exited without a readable word, and `reason` says
        // so.
        Collected::ReadFailed(_) | Collected::TimedOut => Vec::new(),
    };
    ToolError::Failed {
        program: program.to_owned(),
        status,
        line: reason(&[], &stderr),
    }
}

/// A chain whose stream has been handed over: both children owned, both in
/// one process group, neither reading a keystroke meant for the interface.
/// Reading it serves the pre-buffer bytes the fill read first, then the
/// pipe; dropping it kills the whole group and reaps both — exactly what
/// [`end_all`] does — so the
/// [`MediaSource`][symphonia::core::io::MediaSource] a resolve returns is
/// the chain's lifetime, and a killed playback takes the downloader and the
/// transcoder with it.
pub struct Running {
    yt_dlp: Child,
    ffmpeg: Child,
    stdout: Option<ChildStdout>,
    prefix: Vec<u8>,
}

impl Running {
    /// Ends the chain now, without waiting for the drop: the process group
    /// both children share is killed and both exits are reaped. Idempotent —
    /// the drop runs it again, where a kill on an already-dead child is an
    /// error nobody reads — and cheap, so the stream a resolve returns can
    /// end the chain the moment its last reader closes.
    pub fn end(&mut self) {
        end_all(&mut [&mut self.yt_dlp, &mut self.ffmpeg]);
    }
}

impl Read for Running {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.prefix.is_empty() {
            match self.stdout.as_mut() {
                Some(pipe) => pipe.read(buf),
                None => Ok(0),
            }
        } else {
            let read = self.prefix.len().min(buf.len());
            buf[..read].copy_from_slice(&self.prefix[..read]);
            self.prefix.drain(..read);
            Ok(read)
        }
    }
}

impl fmt::Debug for Running {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The pipes and the bytes already read are not printable; the pids
        // are what a reader of a debug dump can act on.
        f.debug_struct("Running")
            .field("yt_dlp_pid", &self.yt_dlp.id())
            .field("ffmpeg_pid", &self.ffmpeg.id())
            .field("prefix_bytes", &self.prefix.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pipe whose read fails mid-way is a failure naming the IO error —
    /// never an empty answer a caller would read as "the tool said nothing"
    /// (R4-3). A real child's pipe read is forced here through the channel
    /// `drain` hands back, because no child script can make `read_to_end`
    /// fail on demand.
    #[test]
    fn a_read_error_is_a_failure_naming_the_io_error() {
        let (sender, receiver) = mpsc::channel::<std::io::Result<Vec<u8>>>();
        sender
            .send(Err(std::io::Error::other("the pipe burst")))
            .expect("the reader answers");

        match collect(&receiver, Instant::now() + Duration::from_secs(1)) {
            Collected::ReadFailed(message) => {
                assert!(
                    message.contains("the pipe burst"),
                    "the IO error is named: {message}"
                );
            }
            other => panic!("expected a read failure, got {other:?}"),
        }
    }

    /// A reader that never sends at all — a panic, or a pipe dropped under it
    /// — is a lost answer, not an empty one.
    #[test]
    fn a_reader_that_never_answered_is_a_failure_not_an_empty_answer() {
        // `drain` always sends exactly one result, so a channel that closes
        // without one is a reader that died: the tool's answer was lost, and
        // reporting it as empty would turn a lost read into a success.
        let (sender, receiver) = mpsc::channel();
        drop(sender);

        match collect(&receiver, Instant::now() + Duration::from_secs(1)) {
            Collected::ReadFailed(message) => {
                assert!(
                    message.contains("stopped before it could answer"),
                    "the failure names the lost reader: {message}"
                );
            }
            other => panic!("expected a read failure, got {other:?}"),
        }
    }

    /// A pipe that closes with no bytes at all stays an empty success — the
    /// closed pipe is not a failure, and the caller keeps its behaviour for
    /// it (R4-3).
    #[test]
    fn a_closed_pipe_with_no_bytes_is_an_empty_success() {
        let (sender, receiver) = mpsc::channel::<std::io::Result<Vec<u8>>>();
        sender.send(Ok(Vec::new())).expect("the reader answers");

        match collect(&receiver, Instant::now() + Duration::from_secs(1)) {
            Collected::Bytes(bytes) => {
                assert!(bytes.is_empty(), "no bytes came back: {bytes:?}");
            }
            other => panic!("expected an empty answer, got {other:?}"),
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.end();
    }
}

/// The program's own file name: what a sentence names instead of a path.
fn file_name(program: &Path) -> String {
    program.file_name().map_or_else(
        || program.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Waits for `child` to exit, up to `until`.
///
/// `Ok(None)` is the deadline: the caller kills it. `Ok(Some(status))` is an
/// exit, already reaped by the wait that reported it.
fn wait(child: &mut Child, until: Instant, program: &str) -> Result<Option<ExitStatus>, ToolError> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(Some(status)),
            Ok(None) => {}
            Err(error) => {
                end_all(&mut [child]);
                return Err(ToolError::Unusable {
                    program: program.to_owned(),
                    message: error.to_string(),
                });
            }
        }
        if Instant::now() >= until {
            return Ok(None);
        }
        thread::sleep(POLL);
    }
}

/// Reads one pipe to its end on its own thread and hands the bytes back when it
/// closes. A pipe that closes without a byte hands back an empty `Vec` — no
/// bytes is an answer, and the exit status says whether it is one. A read that
/// fails mid-way hands back the error itself: truncated output is never an
/// answer a caller can mistake for what the tool said (R4-3).
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> Receiver<std::io::Result<Vec<u8>>> {
    let (sender, receiver) = mpsc::channel();
    if let Some(mut pipe) = pipe {
        thread::spawn(move || {
            let mut buffer = Vec::new();
            let read = pipe.read_to_end(&mut buffer).map(|_| buffer);
            let _ = sender.send(read);
        });
    }
    receiver
}

/// What one pipe's wait ended as: the bytes it delivered — possibly none, a
/// pipe that closed empty is an empty answer, not a failure — why the read
/// itself failed, naming the IO error, or that the bound ran out first.
///
/// "Closed empty" is a reader that finished and sent nothing back; a reader
/// that never sent at all is a *lost* reader, and that is a failure, not an
/// empty answer: `drain` always sends exactly one result, so silence means the
/// thread died before it could.
#[derive(Debug)]
enum Collected {
    Bytes(Vec<u8>),
    ReadFailed(String),
    TimedOut,
}

fn collect(pipe: &Receiver<std::io::Result<Vec<u8>>>, until: Instant) -> Collected {
    let left = until.saturating_duration_since(Instant::now());
    match pipe.recv_timeout(left) {
        Ok(Ok(bytes)) => Collected::Bytes(bytes),
        Ok(Err(error)) => Collected::ReadFailed(error.to_string()),
        // The reader is gone without a word — a panic, or a pipe the runtime
        // dropped under it. Presenting that as the tool's empty answer would
        // turn a lost read into a successful one.
        Err(RecvTimeoutError::Disconnected) => {
            Collected::ReadFailed(String::from("the reader stopped before it could answer"))
        }
        Err(RecvTimeoutError::Timeout) => Collected::TimedOut,
    }
}

/// The line a failure quotes: the program's own reason, never a wall of detail.
fn reason(stdout: &[u8], stderr: &[u8]) -> String {
    for stream in [stderr, stdout] {
        let text = String::from_utf8_lossy(stream);
        if let Some(line) = text.lines().map(str::trim).find(|line| !line.is_empty()) {
            return line.to_owned();
        }
    }
    String::from("it exited without saying why")
}

/// Kills the children with their children and reaps them, so the deadline is
/// the end of the call and never a spinner over a process still running.
fn expired(child: &mut Child, program: String, after: Duration) -> ToolError {
    end_all(&mut [child]);
    ToolError::Deadline { program, after }
}

/// Ends every child without leaving anything behind: the process group the
/// first child owns is killed — the chain's two stages share one group, and
/// a kill that stops at one stage leaves the other holding the pipe — and
/// every exit is waited for, because a zombie is not a failure outcome.
#[cfg(unix)]
fn end_all(children: &mut [&mut Child]) {
    if let Some(group) = children
        .first()
        .and_then(|child| i32::try_from(child.id()).ok())
        .and_then(rustix::process::Pid::from_raw)
    {
        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::Kill);
    }
    for child in children.iter_mut() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Ends every child without leaving anything behind. Without process groups
/// there are only the children themselves to stop, and they are still reaped.
#[cfg(not(unix))]
fn end_all(children: &mut [&mut Child]) {
    for child in children.iter_mut() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Puts `command` in a process group of its own, so the kill above reaches
/// whatever it starts. Its stdin is null, so nothing it starts can be stopped
/// by the terminal for reading one, and it never becomes a keyboard's target.
#[cfg(unix)]
fn own_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    command.process_group(0);
}

/// Without process groups there is nothing to put in one.
#[cfg(not(unix))]
fn own_group(_command: &mut Command) {}

/// Puts `command` into the group `pgid` owns: the chain's second stage joins
/// the downloader's process group, so one kill reaches both. A group id that
/// does not fit an `i32` — no real pid is that large — leaves the child where
/// it is, and [`end_all`] still reaps it directly.
#[cfg(unix)]
fn join_group(command: &mut Command, pgid: u32) {
    use std::os::unix::process::CommandExt;

    if let Ok(pgid) = i32::try_from(pgid) {
        command.process_group(pgid);
    }
}

/// Without process groups there is no group to join.
#[cfg(not(unix))]
fn join_group(_command: &mut Command, _pgid: u32) {}
