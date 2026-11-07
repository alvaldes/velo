//! The interface's colors, as roles over the terminal's own palette.
//!
//! Nothing here is a fixed RGB value: the background and the text are the
//! terminal's (`Reset`), so a translucent terminal stays translucent, and
//! every other role is one of the sixteen ANSI colors the user's theme
//! defines. The same build looks like Gentleman in a Gentleman terminal and
//! like Everforest in an Everforest one (ui-redesign T8, S5).
//!
//! No ANSI color can stand for a raised surface — many themes set color 0 to
//! the background itself — so the interface marks the selection and the
//! primary button with the accent, and lets everything else sit flat.

use ratatui::style::Color;

/// The terminal's own background.
pub const BACKGROUND: Color = Color::Reset;
/// The window frame and the unfilled part of every bar (ANSI 8).
pub const BORDER: Color = Color::DarkGray;
/// The accent: the wordmark, the playing row, focus, filled bars (ANSI 5).
pub const ACCENT: Color = Color::Magenta;
/// The accent's bright twin, for the top of the spectrum (ANSI 13).
pub const ACCENT_BRIGHT: Color = Color::LightMagenta;
/// Text set on the accent: the theme's darkest color (ANSI 0).
pub const INK: Color = Color::Black;
/// Secondary text: keys, counts, the transport buttons (ANSI 4).
pub const SECONDARY: Color = Color::Blue;
/// Dimmed text: folders, notes, hints that can be forgotten (ANSI 8).
pub const DIM: Color = Color::DarkGray;
/// Ordinary text: the terminal's own foreground.
pub const TEXT: Color = Color::Reset;
