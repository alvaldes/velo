//! `YouTube`, played through `yt-dlp` (issue #70).
//!
//! `yt-dlp` is not in the repository and never will be: it is an optional
//! external tool, found at launch and absent without a launch failure. velo
//! spawns it, reads its output, and never talks to `YouTube` itself.

pub mod tool;
