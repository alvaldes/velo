//! The Subsonic/Navidrome client and its authentication (T12a, S14), plus
//! the two read endpoints the sync needs (T12b) and the two play-history
//! pieces: `getCoverArt` into the covers cache and `scrobble` on a track
//! change (T12d).
//!
//! Scope of this module today: speak the protocol over a [`Transport`]
//! seam, resolve the server's secret, answer "can I use this server?"
//! through `velo --subsonic-test`, serve `getAlbumList2`/`getAlbum` to
//! the sync in `crate::library::sync`, and — from T12c and T12d — own the
//! on-disk caches the interface plays and draws from: the track cache and
//! resolver in [`cache`], the cover-art cache behind the [`cache::CoverArt`]
//! seam, and the scrobble decision behind the [`scrobble::Scrobbles`] seam.
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
pub mod scrobble;

pub use error::Error;
