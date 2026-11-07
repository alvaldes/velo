//! Indexing the library in the background (library-refresh T3, S3).
//!
//! The interface keeps drawing and answering keys while a scan runs: the
//! scan happens on a thread that opens its own [`Library`] connection —
//! SQLite connections are not shared across threads — and hands its outcome
//! back once, through a channel the event loop polls every turn.
//!
//! Nothing half-written is ever read: [`Library::index`] writes in one
//! transaction and the index runs in WAL mode, so the interface's connection
//! keeps reading the last committed snapshot until the commit, and the lists
//! are re-read only after the outcome arrives. Dropping an [`Indexer`] (for
//! example on quit) detaches the thread; an uncommitted transaction is rolled
//! back by SQLite, so the index is left as it was.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use crate::library::{IndexReport, Library, LibraryError};

/// What one background index ends with: its counts, or why it failed.
pub type IndexOutcome = Result<IndexReport, String>;

/// One index running on its own thread.
pub struct Indexer {
    root: PathBuf,
    outcome: Receiver<IndexOutcome>,
}

impl Indexer {
    /// Starts indexing `root` into the library `open` returns. `open` runs on
    /// the new thread, so the connection is created where it is used.
    pub fn spawn<F>(root: PathBuf, open: F) -> Self
    where
        F: FnOnce() -> Result<Library, LibraryError> + Send + 'static,
    {
        let (sender, outcome) = mpsc::channel();
        let scanned = root.clone();
        thread::spawn(move || {
            let result = open()
                .map_err(|error| format!("cannot open the index: {error}"))
                .and_then(|library| library.index(&scanned).map_err(|error| error.to_string()));
            // The interface may already be gone; nobody is left to tell.
            let _ = sender.send(result);
        });
        Self { root, outcome }
    }

    /// The folder being indexed.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The outcome once the thread is done, `None` while it still runs.
    /// A thread that died without answering is reported as a failure, never
    /// as an index that runs forever.
    #[must_use]
    pub fn poll(&self) -> Option<IndexOutcome> {
        match self.outcome.try_recv() {
            Ok(outcome) => Some(outcome),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(STOPPED.to_owned())),
        }
    }

    /// Blocks until the thread is done and returns its outcome.
    ///
    /// # Errors
    ///
    /// Returns why the index failed, as [`Indexer::poll`] would.
    pub fn wait(self) -> IndexOutcome {
        self.outcome
            .recv()
            .unwrap_or_else(|_| Err(STOPPED.to_owned()))
    }
}

const STOPPED: &str = "the indexing thread stopped without an answer";
