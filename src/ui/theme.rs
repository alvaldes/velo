//! The interface's colors, as roles over the palette the user configured.
//!
//! Nothing here hard-codes a value: every role comes from the `[theme]`
//! table of the config file, resolved by [`ThemeColors`], and the defaults
//! reproduce the ANSI look velo has always drawn with — the background and
//! the text are the terminal's (`Reset`), so a translucent terminal stays
//! translucent, and every other role is the fixed color its ANSI entry
//! stood for. The same build looks like Gentleman in a Gentleman terminal
//! and like Everforest in an Everforest one (ui-redesign T8, S5).
//!
//! No ANSI color can stand for a raised surface — many themes set color 0 to
//! the background itself — so the interface marks the selection and the
//! primary button with the accent, and lets everything else sit flat.

use ratatui::style::Color;

use crate::config::{ThemeColor, ThemeColors};

/// The palette the interface draws with (S4): the seven configured roles
/// plus the accent's bright twin, which is always derived from the accent
/// and never configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// The screen behind everything.
    pub background: Color,
    /// The body text and the values in a list.
    pub text: Color,
    /// Hints, section headers and everything that should recede.
    pub dim: Color,
    /// Card borders, titles, the focused row and the selection.
    pub accent: Color,
    /// The accent's bright twin, for the top of the spectrum: the accent one
    /// lighter step, derived, never configured.
    pub accent_bright: Color,
    /// The keys and labels next to the values.
    pub secondary: Color,
    /// The panes that are not the one in focus.
    pub border: Color,
    /// The text drawn on top of `accent`.
    pub ink: Color,
}

impl From<ThemeColors> for Theme {
    fn from(colors: ThemeColors) -> Self {
        let color = |colour: ThemeColor| match colour {
            ThemeColor::Terminal => Color::Reset,
            ThemeColor::Rgb { red, green, blue } => Color::Rgb(red, green, blue),
        };
        let accent = color(colors.accent);
        Self {
            background: color(colors.background),
            text: color(colors.text),
            dim: color(colors.dim),
            accent,
            accent_bright: brighten(accent),
            secondary: color(colors.secondary),
            border: color(colors.border),
            ink: color(colors.ink),
        }
    }
}

impl Default for Theme {
    /// The palette velo drew with before colours were configurable.
    fn default() -> Self {
        ThemeColors::default().into()
    }
}

/// The accent's bright twin: each channel one lighter step, saturating at
/// full. A terminal's own colour has no lighter step, so it stays as it is.
fn brighten(color: Color) -> Color {
    match color {
        Color::Rgb(red, green, blue) => Color::Rgb(
            red.saturating_add(0x28),
            green.saturating_add(0x28),
            blue.saturating_add(0x28),
        ),
        color => color,
    }
}
