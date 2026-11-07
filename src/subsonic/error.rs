//! The errors a Subsonic server (or the credential resolver) can produce.
//!
//! Every error carries what actually happened plus a human sentence. The
//! sentences are the contract `velo --subsonic-test` prints, so they are
//! pinned by tests: a wrong password never masquerades as an unreachable
//! server, and an unreachable server is never reported as a credential
//! problem.

use std::fmt;

/// Everything the Subsonic layer fails with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The server answered with a Subsonic error envelope: its own code and
    /// message, kept verbatim.
    Server {
        /// The server's `error.code` (0, 10, 20, 30, 40, 50, 70, …).
        code: u32,
        /// The server's own `error.message`.
        message: String,
    },
    /// The server could not be reached at all: refused connection, DNS
    /// failure, a non-200 HTTP answer. Never a credential problem.
    Unreachable {
        /// What the transport observed, e.g. `connection refused`.
        detail: String,
    },
    /// The server answered but does not speak the JSON wire format: a
    /// classic XML-only Subsonic, too old for velo.
    TooOld {
        /// What the answer looked like, for the one-line reason.
        detail: String,
    },
    /// The local secret could not be resolved: nothing configured, an
    /// unreadable or malformed key file. No request was sent.
    Credentials(String),
    /// A value in the `[subsonic]` table (or its absence) does not make a
    /// usable server definition.
    Config(String),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Server { code, message } => {
                formatter.write_str(&sentence_for_code(*code, message))
            }
            Self::Unreachable { detail } => {
                write!(formatter, "the server could not be reached: {detail}")
            }
            Self::TooOld { detail } => write!(
                formatter,
                "the server is too old to speak JSON — velo needs an \
                 OpenSubsonic-compatible server ({detail})"
            ),
            Self::Credentials(message) | Self::Config(message) => write!(formatter, "{message}"),
        }
    }
}

impl std::error::Error for Error {}

/// The human sentence for the server's own `error.code`, with the server's
/// message kept verbatim after it.
///
/// Codes worth naming (the Subsonic API's own table): 40 wrong credentials,
/// 30 server too old for the protocol version, 50 not authorised, 70 not
/// found, 0 and 10 generic server failures. Every other code still carries
/// the code and the message.
#[must_use]
pub fn sentence_for_code(code: u32, message: &str) -> String {
    let sentence = match code {
        0 => "the server failed",
        10 => "the server could not handle the request",
        30 => "the server is too old for this client",
        40 => "the credentials were rejected",
        50 => "the user is not authorised to do that",
        70 => "the requested item was not found",
        _ => return format!("the server reported error {code}: {message}"),
    };
    format!("{sentence}: {message}")
}
