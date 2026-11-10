//! The built-in theme catalogue (issue #37, S3): eleven classic colour
//! schemes embedded in the binary, so the picker has something to offer
//! before the user writes a single theme file — plus the terminal's own
//! (issue #54, S3), whose seven slots are all `reset`. Every scheme maps its
//! own canonical colours onto velo's seven slots — `background`, `text`,
//! `dim`, `accent`, `secondary`, `border`, `ink` — with the text kept
//! readable: no theme pairs an invisible foreground with its background.
//! The factory theme `velo` is not here; it is velo's own palette,
//! [`ThemeColors::default`], which [`catalogue`] puts first.

use crate::config::{
    DEFAULT_THEME_NAME, LoadedTheme, TERMINAL_THEME_NAME, ThemeColor, ThemeColors,
};

const fn rgb(red: u8, green: u8, blue: u8) -> ThemeColor {
    ThemeColor::Rgb { red, green, blue }
}

/// The terminal's own palette (issue #54, S3): every slot is `reset`, so
/// velo draws with exactly the colours the terminal's user already chose —
/// theme and terminal agree by construction. It ships beside the config as
/// `themes/default terminal.toml`.
const DEFAULT_TERMINAL: ThemeColors = ThemeColors {
    background: ThemeColor::Terminal,
    text: ThemeColor::Terminal,
    dim: ThemeColor::Terminal,
    accent: ThemeColor::Terminal,
    secondary: ThemeColor::Terminal,
    border: ThemeColor::Terminal,
    ink: ThemeColor::Terminal,
};

/// Catppuccin Latte (<https://catppuccin.com>): the flavour's mauve accent,
/// blue for the labels, overlay grey for what recedes.
const CATPPUCCIN_LATTE: ThemeColors = ThemeColors {
    background: rgb(0xef, 0xf1, 0xf5),
    text: rgb(0x4c, 0x4f, 0x69),
    dim: rgb(0x9c, 0xa0, 0xb0),
    accent: rgb(0x88, 0x39, 0xef),
    secondary: rgb(0x1e, 0x66, 0xf5),
    border: rgb(0xcc, 0xd0, 0xda),
    ink: rgb(0xef, 0xf1, 0xf5),
};

/// Catppuccin Mocha (<https://catppuccin.com>): the flavour's mauve accent,
/// blue for the labels, overlay grey for what recedes.
const CATPPUCCIN_MOCHA: ThemeColors = ThemeColors {
    background: rgb(0x1e, 0x1e, 0x2e),
    text: rgb(0xcd, 0xd6, 0xf4),
    dim: rgb(0x7f, 0x84, 0x9c),
    accent: rgb(0xcb, 0xa6, 0xf7),
    secondary: rgb(0x89, 0xb4, 0xfa),
    border: rgb(0x31, 0x32, 0x44),
    ink: rgb(0x1e, 0x1e, 0x2e),
};

/// Dracula (<https://draculatheme.com>, by Zeno Rocha and friends): the
/// scheme's purple, with cyan for the labels and the selection tone for the
/// unfocused panes.
const DRACULA: ThemeColors = ThemeColors {
    background: rgb(0x28, 0x2a, 0x36),
    text: rgb(0xf8, 0xf8, 0xf2),
    dim: rgb(0x62, 0x72, 0xa4),
    accent: rgb(0xbd, 0x93, 0xf9),
    secondary: rgb(0x8b, 0xe9, 0xfd),
    border: rgb(0x44, 0x47, 0x5a),
    ink: rgb(0x28, 0x2a, 0x36),
};

/// Gruvbox Dark (<https://github.com/morhetz/gruvbox>, by Pavel Pertsev): the
/// scheme's orange accent, faded blue for the labels, bg1 for the panes.
const GRUVBOX_DARK: ThemeColors = ThemeColors {
    background: rgb(0x28, 0x28, 0x28),
    text: rgb(0xeb, 0xdb, 0xb2),
    dim: rgb(0x92, 0x83, 0x74),
    accent: rgb(0xfe, 0x80, 0x19),
    secondary: rgb(0x83, 0xa5, 0x98),
    border: rgb(0x3c, 0x38, 0x36),
    ink: rgb(0x28, 0x28, 0x28),
};

/// Monokai (by Wimer Hazenberg, the Sublime Text classic): the green of the
/// scheme's syntax strings as the accent, cyan for the labels.
const MONOKAI: ThemeColors = ThemeColors {
    background: rgb(0x27, 0x28, 0x22),
    text: rgb(0xf8, 0xf8, 0xf2),
    dim: rgb(0x75, 0x71, 0x5e),
    accent: rgb(0xa6, 0xe2, 0x2e),
    secondary: rgb(0x66, 0xd9, 0xef),
    border: rgb(0x49, 0x48, 0x3e),
    ink: rgb(0x27, 0x28, 0x22),
};

/// Nord (<https://nordtheme.com>): the frost's ice blue as the accent, its
/// neighbour for the labels, polar-night tones for what recedes — the dim
/// grey lifted one step from nord3 so the hints stay readable.
const NORD: ThemeColors = ThemeColors {
    background: rgb(0x2e, 0x34, 0x40),
    text: rgb(0xec, 0xef, 0xf4),
    dim: rgb(0x7b, 0x88, 0xa1),
    accent: rgb(0x88, 0xc0, 0xd0),
    secondary: rgb(0x81, 0xa1, 0xc1),
    border: rgb(0x43, 0x4c, 0x5e),
    ink: rgb(0x2e, 0x34, 0x40),
};

/// Atom One Dark (<https://github.com/atom/one-dark-syntax>): the scheme's
/// blue accent, purple for the labels, comment grey for what recedes.
const ONE_DARK: ThemeColors = ThemeColors {
    background: rgb(0x28, 0x2c, 0x34),
    text: rgb(0xab, 0xb2, 0xbf),
    dim: rgb(0x5c, 0x63, 0x70),
    accent: rgb(0x61, 0xaf, 0xef),
    secondary: rgb(0xc6, 0x78, 0xdd),
    border: rgb(0x3e, 0x44, 0x51),
    ink: rgb(0x28, 0x2c, 0x34),
};

/// Atom One Light: the dark scheme's blue and purple against paper, the
/// comment grey for what recedes.
const ONE_LIGHT: ThemeColors = ThemeColors {
    background: rgb(0xfa, 0xfa, 0xfa),
    text: rgb(0x38, 0x3a, 0x42),
    dim: rgb(0xa0, 0xa1, 0xa7),
    accent: rgb(0x01, 0x84, 0xbc),
    secondary: rgb(0xa6, 0x26, 0xa4),
    border: rgb(0xd4, 0xd4, 0xd4),
    ink: rgb(0xfa, 0xfa, 0xfa),
};

/// Solarized Dark (<https://ethanschoonover.com/solarized>, by Ethan
/// Schoonover): the scheme's blue accent, cyan for the labels, base01 for
/// what recedes and base02 for the unfocused panes.
const SOLARIZED_DARK: ThemeColors = ThemeColors {
    background: rgb(0x00, 0x2b, 0x36),
    text: rgb(0x83, 0x94, 0x96),
    dim: rgb(0x58, 0x6e, 0x75),
    accent: rgb(0x26, 0x8b, 0xd2),
    secondary: rgb(0x2a, 0xa1, 0x98),
    border: rgb(0x07, 0x36, 0x42),
    ink: rgb(0xfd, 0xf6, 0xe3),
};

/// Solarized Light: the same roles against base3, base00 for the body text.
const SOLARIZED_LIGHT: ThemeColors = ThemeColors {
    background: rgb(0xfd, 0xf6, 0xe3),
    text: rgb(0x65, 0x7b, 0x83),
    dim: rgb(0x93, 0xa1, 0xa1),
    accent: rgb(0x26, 0x8b, 0xd2),
    secondary: rgb(0x2a, 0xa1, 0x98),
    border: rgb(0xee, 0xe8, 0xd5),
    ink: rgb(0xfd, 0xf6, 0xe3),
};

/// Tokyo Night (<https://github.com/folke/tokyonight.nvim>, by Folke
/// Lemaitre): the storm palette's blue accent, purple for the labels,
/// comment blue for what recedes.
const TOKYO_NIGHT: ThemeColors = ThemeColors {
    background: rgb(0x1a, 0x1b, 0x26),
    text: rgb(0xc0, 0xca, 0xf5),
    dim: rgb(0x56, 0x5f, 0x89),
    accent: rgb(0x7a, 0xa2, 0xf7),
    secondary: rgb(0xbb, 0x9a, 0xf7),
    border: rgb(0x29, 0x2e, 0x42),
    ink: rgb(0x1a, 0x1b, 0x26),
};

/// The catalogue, sorted by name (issue #37, S3; issue #54, S3): what the
/// picker offers after the factory theme `velo`.
pub(crate) const BUILT_INS: [(&str, ThemeColors); 12] = [
    ("catppuccin-latte", CATPPUCCIN_LATTE),
    ("catppuccin-mocha", CATPPUCCIN_MOCHA),
    (TERMINAL_THEME_NAME, DEFAULT_TERMINAL),
    ("dracula", DRACULA),
    ("gruvbox-dark", GRUVBOX_DARK),
    ("monokai", MONOKAI),
    ("nord", NORD),
    ("one-dark", ONE_DARK),
    ("one-light", ONE_LIGHT),
    ("solarized-dark", SOLARIZED_DARK),
    ("solarized-light", SOLARIZED_LIGHT),
    ("tokyo-night", TOKYO_NIGHT),
];

/// The picker's starting library (issue #37, S2; issue #54, S1): the
/// factory theme `velo` — velo's own palette, never in the catalogue —
/// first, then the twelve built-ins in name order. The launch paths pass
/// this through `select_theme`, which replaces a row whose name a
/// `themes/*.toml` file carries, and appends the files that name nothing
/// built-in. The alias `default` gets no row (issue #54, S2).
pub(crate) fn catalogue() -> Vec<LoadedTheme> {
    let mut themes = Vec::with_capacity(1 + BUILT_INS.len());
    themes.push(LoadedTheme {
        name: DEFAULT_THEME_NAME.to_owned(),
        colors: ThemeColors::default(),
    });
    themes.extend(BUILT_INS.iter().map(|(name, colors)| LoadedTheme {
        name: (*name).to_owned(),
        colors: *colors,
    }));
    themes
}
