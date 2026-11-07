//! The Gentleman Programming palette, as constants.

use ratatui::style::Color;

/// The app's own background.
pub const BACKGROUND: Color = Color::Rgb(0x1A, 0x12, 0x18);
/// The accent: the wordmark, the selected row, focus.
pub const ACCENT: Color = Color::Rgb(0xF0, 0x95, 0xC8);
/// Secondary text: track counts, ordinary information.
pub const SECONDARY: Color = Color::Rgb(0xD7, 0xA0, 0xB8);
/// Dimmed text: parent directories, hints that can be forgotten.
pub const DIM: Color = Color::Rgb(0x6B, 0x55, 0x60);
/// Ordinary text.
pub const TEXT: Color = Color::Rgb(0xED, 0xE4, 0xE9);
