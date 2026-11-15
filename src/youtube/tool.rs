//! One external process, bounded: the runner every `yt-dlp` call goes through.
//!
//! Nothing else in the tree captures a child's standard output, imposes a
//! deadline or kills a stuck child — `open_url` is fire-and-forget — so the
//! rules live here, once, and every one of them is a rule the interface
//! depends on. `yt-dlp` runs ffmpeg of its own, which is why the kill goes to
//! the process group: stopping at the parent leaves the grandchild holding the
//! pipe and the call waiting on it forever.

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

/// How long one play-time resolve may take: a full extraction of one video.
pub const RESOLVE: Deadline = Deadline::new(Duration::from_secs(30));

/// How long [`spawn`] waits for a child's first byte. A stream's first byte
/// carries the whole cost of the work before it — extraction, download, the
/// transcode — the same register of bound [`RESOLVE`] names for one
/// full extraction. Inside the bound, an exit is the verdict: a tool that
/// cannot deliver dies with a reason on stderr before its first byte, and
/// that reason — never a decoder error — is what a failing resolve quotes.
const STARTUP: Duration = Duration::from_secs(30);

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
            Self::NotInstalled(_) => write!(
                f,
                "yt-dlp is not installed — install it to play YouTube (brew install yt-dlp, \
                 or your package manager)"
            ),
            Self::Deadline { program, after } => write!(
                f,
                "{program} did not answer within {}s — the request was cancelled",
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
    let Ok(Some(stdout)) = collect(&out, until) else {
        return Err(expired(&mut child, name, deadline.after()));
    };
    let Ok(Some(stderr)) = collect(&err, until) else {
        return Err(expired(&mut child, name, deadline.after()));
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

/// Spawns `program` with `args` and hands its standard output over without
/// waiting for it: the caller reads the stream as it arrives — the shape the
/// play path needs, where the tool itself delivers the bytes (issue #70,
/// S4 as revised) and a bounded wait would be a deadline on the music.
/// Everything [`run`] guarantees still holds: arguments are arguments and
/// never a shell line, the child never inherits the terminal, and nothing
/// is left unreaped — [`Running`]'s `Drop` kills the whole process group
/// and waits, so a playback that is closed or killed never leaves a
/// downloader running behind it.
///
/// The handover happens the moment the first bytes arrive — never a fixed
/// wait — and until then the child's exit decides: a stream that never
/// starts is the tool's own failure, quoted from its stderr, so a failing
/// resolve says why `yt-dlp` could not answer, never what a decoder made of
/// an empty pipe. A child that writes nothing within [`STARTUP`] misses the
/// bound and is killed with its children, [`ToolError::Deadline`].
///
/// A child that cannot start is [`ToolError::NotInstalled`]; a spawn failure
/// is [`ToolError::Unusable`], as in [`run`].
///
/// # Errors
///
/// [`ToolError::NotInstalled`], [`ToolError::Deadline`] (no first byte
/// within [`STARTUP`]), [`ToolError::Failed`] (an exit before the first
/// byte), or [`ToolError::Unusable`].
pub fn spawn(program: &Path, args: &[OsString]) -> Result<Running, ToolError> {
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

    // stderr is drained from the start, the way `run` drains it: a chatty
    // tool must never block on a full pipe, and the drain is what an
    // immediate failure quotes. stdout is watched by one thread that takes
    // the first read and hands the pipe straight back — the bytes read
    // before the handover are none lost, they ride in [`Running`].
    let err = drain(child.stderr.take());
    let stdout_pipe = child.stdout.take();
    let (sender, receiver) = mpsc::channel::<(std::io::Result<Vec<u8>>, Option<ChildStdout>)>();
    thread::spawn(move || {
        let Some(mut pipe) = stdout_pipe else {
            let _ = sender.send((Err(std::io::Error::other("stdout was not piped")), None));
            return;
        };
        let mut first = [0u8; 4096];
        let read = pipe.read(&mut first).map(|read| first[..read].to_vec());
        let _ = sender.send((read, Some(pipe))); // the pipe comes back for the caller
    });
    let until = Instant::now() + STARTUP;

    loop {
        match receiver.try_recv() {
            Ok((Ok(prefix), stdout)) if !prefix.is_empty() => {
                return Ok(Running {
                    child,
                    stdout,
                    prefix,
                });
            }
            // An empty read — EOF before a byte — and a read error are left
            // to the exit status and the deadline: the pipe, if it came
            // back, is dropped here and its verdict is the child's.
            Ok(_) | Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => {}
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                // The exit and the first read race: give the reader thread
                // a bounded moment to hand back what it read before the
                // exit is called the verdict. Then the group kill first —
                // a grandchild may still hold stderr — and what it said.
                let handed = receiver.recv_timeout(Duration::from_secs(1));
                if let Ok((Ok(prefix), stdout)) = handed
                    && !prefix.is_empty()
                {
                    return Ok(Running {
                        child,
                        stdout,
                        prefix,
                    });
                }
                end(&mut child);
                let stderr = match collect(&err, Instant::now() + Duration::from_secs(1)) {
                    Ok(Some(bytes)) => bytes,
                    Ok(None) | Err(()) => Vec::new(),
                };
                return Err(ToolError::Failed {
                    program: name,
                    status: status.code(),
                    line: reason(&[], &stderr),
                });
            }
            Ok(None) => {}
            Err(error) => {
                end(&mut child);
                return Err(ToolError::Unusable {
                    program: name,
                    message: error.to_string(),
                });
            }
        }
        if Instant::now() >= until {
            end(&mut child);
            return Err(ToolError::Deadline {
                program: name,
                after: STARTUP,
            });
        }
        thread::sleep(POLL);
    }
}

/// A child whose stream has been handed over: owned, in a process group of
/// its own, never reading a keystroke meant for the interface. Reading it
/// serves the bytes the startup watch read first, then the pipe; dropping
/// it ends the whole group and reaps — exactly what [`end`] does for
/// [`run`] — so the [`MediaSource`][symphonia::core::io::MediaSource] a
/// resolve returns is the child's lifetime, and a killed playback takes
/// the downloader with it.
pub struct Running {
    child: Child,
    stdout: Option<ChildStdout>,
    prefix: Vec<u8>,
}

impl Running {
    /// The child's standard output, once: the raw pipe, already past the
    /// bytes the startup watch read.
    #[must_use]
    pub fn stdout(&mut self) -> Option<ChildStdout> {
        self.stdout.take()
    }

    /// Ends the child now, without waiting for the drop: the whole process
    /// group is killed and the exit reaped. Idempotent — the drop runs it
    /// again, where a kill on an already-dead child is an error nobody
    /// reads — and cheap, so the stream a resolve returns can end the
    /// downloader the moment its last reader closes.
    pub fn end(&mut self) {
        end(&mut self.child);
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
        // The pipes and the bytes already read are not printable; the pid is
        // what a reader of a debug dump can act on.
        f.debug_struct("Running")
            .field("pid", &self.child.id())
            .field("prefix_bytes", &self.prefix.len())
            .finish_non_exhaustive()
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
                end(child);
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
/// closes. A pipe that closes without a byte — or fails mid-read — is `Ok(None)`:
/// the process's own exit status already carries the verdict.
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

/// The bytes one pipe delivered, or `Ok(None)` when it delivered none within
/// `until`, or `Err(())` when it kept the call waiting past it.
type Collected = Result<Option<Vec<u8>>, ()>;

fn collect(pipe: &Receiver<std::io::Result<Vec<u8>>>, until: Instant) -> Collected {
    let left = until.saturating_duration_since(Instant::now());
    match pipe.recv_timeout(left) {
        Ok(Ok(bytes)) => Ok(Some(bytes)),
        Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => Ok(None),
        Err(RecvTimeoutError::Timeout) => Err(()),
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

/// Kills the child with its children and reaps it, so the deadline is the end
/// of the call and never a spinner over a process still running.
fn expired(child: &mut Child, program: String, after: Duration) -> ToolError {
    end(child);
    ToolError::Deadline { program, after }
}

/// Ends `child` without leaving anything behind: its whole process group is
/// killed — `yt-dlp` runs ffmpeg, and a kill that stops at the parent leaves the
/// grandchild — and the exit is waited for, because a zombie is not a
/// failure outcome.
#[cfg(unix)]
fn end(child: &mut Child) {
    if let Some(group) = i32::try_from(child.id())
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    {
        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::Kill);
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Ends `child` without leaving anything behind. Without process groups there
/// is only the child itself to stop, and it is still reaped.
#[cfg(not(unix))]
fn end(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
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
