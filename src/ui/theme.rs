//! The Gentleman Programming palette, as constants.

use ratatui::style::Color;

/// The app's own background.
pub const BACKGROUND: Color = Color::Rgb(0x1A, 0x12, 0x18);
/// A raised surface: the title bar, the selected row, pills and buttons.
pub const SURFACE: Color = Color::Rgb(0x2A, 0x1E, 0x26);
/// The window frame and the unfilled part of every bar.
pub const BORDER: Color = Color::Rgb(0x45, 0x34, 0x3E);
/// The accent: the wordmark, the playing row, focus, filled bars.
pub const ACCENT: Color = Color::Rgb(0xF0, 0x95, 0xC8);
/// Secondary text: track counts, ordinary information.
pub const SECONDARY: Color = Color::Rgb(0xD7, 0xA0, 0xB8);
/// Dimmed text: parent directories, hints that can be forgotten.
pub const DIM: Color = Color::Rgb(0x6B, 0x55, 0x60);
/// The spectrum's gradient, bottom to top, as RGB triples.
pub const BAR_LOW: (u8, u8, u8) = (0xB0, 0x4E, 0x8C);
pub const BAR_HIGH: (u8, u8, u8) = (0xFF, 0xC8, 0xE4);
/// Ordinary text.
pub const TEXT: Color = Color::Rgb(0xED, 0xE4, 0xE9);
