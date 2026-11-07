//! The Subsonic/Navidrome client and its authentication (T12a, S14), plus
//! the two read endpoints the sync needs (T12b).
//!
//! Scope of this module today: speak the protocol over a [`Transport`]
//! seam, resolve the server's secret, answer "can I use this server?"
//! through `velo --subsonic-test`, and serve `getAlbumList2`/`getAlbum` to
//! the sync in `crate::library::sync`. No playback yet: playing a remote
//! row is cache-on-play (T12c): `src/subsonic/cache.rs` owns the fetch and
//! the on-disk cache the producer opens.
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

pub mod cache;
pub mod client;
pub mod credentials;
pub mod error;

pub use error::Error;
