//! The Subsonic/Navidrome client and its authentication (T12a, S14).
//!
//! Scope of this module today: speak the protocol over a [`Transport`]
//! seam, resolve the server's secret, and answer "can I use this server?"
//! through `velo --subsonic-test`. No library and no playback yet: remote
//! tracks land in the existing SQLite index later (T12b), and playback is
//! cache-on-play (T12c).
//!
//! The four decisions this shape rests on:
//!
//! - Playback will be cache-on-play — download the original bytes, then
//!   play locally. Nothing here streams.
//! - Authentication prefers an API key when the server advertises the
//!   `apiKeyAuthentication` extension, and otherwise token + salt
//!   (`t = md5(password + salt)`, `s = salt`), because the token depends on
//!   a fresh salt and cannot be stored.
//! - The wire format is JSON (`f=json`); a server that does not speak JSON
//!   is reported as too old, never parsed as XML.
//! - Remote tracks will live in the existing SQLite index as rows with a
//!   `source` column (T12b, not here).

pub mod client;
pub mod credentials;
pub mod error;

pub use error::Error;
