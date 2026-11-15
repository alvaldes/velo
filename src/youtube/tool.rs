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
use std::process::{Child, Command, ExitStatus, Stdio};
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
