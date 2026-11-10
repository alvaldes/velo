//! The visualizers (issue #40): the nine shapes the spectrum's band area can
//! take, from the filled bars of the old spectrum to the mirrored bars.
//!
//! Every shape is a pure function of (bands, waveform window, elapsed time,
//! area) — issue #40, S2 — so the same input always draws the same frame and
//! every shape is testable without a terminal. The falling peaks and the
//! rain read the elapsed time the session began at, never a random source.
//!
//! The band geometry is the spectrum's own: as many bands as the row holds
//! at two columns each, each bar resolving eight steps per row.

use std::time::Duration;

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use super::theme::Theme;

/// How many samples of the raw mix the `scope` shape looks at: the newest
/// window of the tap, oldest first — about 21 ms at 48 kHz, enough to see
/// the shape of a beat and short enough to follow the music.
pub const WAVEFORM_WINDOW: usize = 1024;

/// How far above a bar the flame's peak starts, in rows.
const FLAME_PEAK_ROWS: usize = 2;

/// How often the flame's peak falls one row: from its start above the bar
/// it rests on the bar after `FLAME_PEAK_ROWS` times this.
const FLAME_FALL_MILLIS: u128 = 500;

/// How often the matrix rain advances one step.
const RAIN_STEP_MILLIS: u128 = 80;

/// Which shape the band area draws (issue #40). `none` draws nothing and
/// gives the rows back, exactly as low power does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Visualizer {
    /// Nothing: the band's rows go back to the list.
    None,
    /// Filled bars per band — the spectrum as it has always been drawn.
    #[default]
    Bars,
    /// The same bars as an outline: only each bar's top edge.
    BarsOutline,
    /// One line undulating with the envelope.
    Wave,
    /// A dot per band, at its height.
    Scatter,
    /// Each band's peak falling back over time.
    Flame,
    /// Columns raining down with the band's trail.
    Matrix,
    /// The raw waveform.
    Scope,
    /// Bars mirrored from the centre, growing outward.
    Mirror,
}

impl Visualizer {
    /// Every visualizer, in the stable order the picker lists them (issue
    /// #40, S3): `none` first, then the eight.
    pub const ALL: [Self; 9] = [
        Self::None,
        Self::Bars,
        Self::BarsOutline,
        Self::Wave,
        Self::Scatter,
        Self::Flame,
        Self::Matrix,
        Self::Scope,
        Self::Mirror,
    ];

    /// The visualizer's name, as the config file and the picker write it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Bars => "bars",
            Self::BarsOutline => "bars-outline",
            Self::Wave => "wave",
            Self::Scatter => "scatter",
            Self::Flame => "flame",
            Self::Matrix => "matrix",
            Self::Scope => "scope",
            Self::Mirror => "mirror",
        }
    }

    /// The visualizer a config-file name names; `None` for a name no
    /// visualizer answers.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.trim();
        Self::ALL
            .into_iter()
            .find(|visualizer| visualizer.name() == name)
    }

    /// The next visualizer in the picker's order, wrapping through `none`.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::None => Self::Bars,
            Self::Bars => Self::BarsOutline,
            Self::BarsOutline => Self::Wave,
            Self::Wave => Self::Scatter,
            Self::Scatter => Self::Flame,
            Self::Flame => Self::Matrix,
            Self::Matrix => Self::Scope,
            Self::Scope => Self::Mirror,
            Self::Mirror => Self::None,
        }
    }

    /// The previous visualizer in the picker's order, wrapping through `none`.
    #[must_use]
    pub const fn previous(self) -> Self {
        match self {
            Self::None => Self::Mirror,
            Self::Bars => Self::None,
            Self::BarsOutline => Self::Bars,
            Self::Wave => Self::BarsOutline,
            Self::Scatter => Self::Wave,
            Self::Flame => Self::Scatter,
            Self::Matrix => Self::Flame,
            Self::Scope => Self::Matrix,
            Self::Mirror => Self::Scope,
        }
    }

    /// Draws the visualizer into `area`: exactly `area.height` lines. Pure —
    /// the same bands, waveform, elapsed time and area always draw the same
    /// frame (issue #40, S2). `levels` holds one level per band, `0.0..=1.0`,
    /// lowest band first; `waveform` the newest tap samples, oldest first.
    #[must_use]
    pub fn draw(
        self,
        levels: &[f32],
        waveform: &[f32],
        elapsed: Duration,
        area: Rect,
        theme: &Theme,
    ) -> Vec<Line<'static>> {
        let height = usize::from(area.height);
        if height == 0 {
            return Vec::new();
        }
        let width = usize::from(area.width);
        let bands = width.div_ceil(2);
        let levels = fitted(levels, bands);
        match self {
            Self::None => blank_grid(height, width),
            Self::Bars => super::spectrum_lines(&levels, area.height, theme),
            Self::BarsOutline => outline_lines(&levels, height, bands, theme),
            Self::Wave => wave_lines(&levels, height, width, bands, theme),
            Self::Scatter => scatter_lines(&levels, height, width, bands, theme),
            Self::Flame => flame_lines(&levels, elapsed, height, bands, theme),
            Self::Matrix => matrix_lines(&levels, elapsed, height, bands, theme),
            Self::Scope => scope_lines(waveform, height, width, theme),
            Self::Mirror => mirror_lines(&levels, height, bands, theme),
        }
    }
}

/// A drawn cell: the character and its colour; `None` leaves the cell blank.
type Cell = Option<(char, Color)>;

/// `levels` sized to exactly `bands`: truncated or padded with silence.
fn fitted(levels: &[f32], bands: usize) -> Vec<f32> {
    let mut levels = levels.to_vec();
    levels.resize(bands, 0.0);
    levels
}

/// `height` blank rows of `width` blank cells.
fn blank_grid(height: usize, width: usize) -> Vec<Line<'static>> {
    vec![vec![None; width]; height].into_lines()
}

/// Renders one shape's grid: every drawn cell its character in its colour,
/// every blank cell one space.
trait IntoLines {
    fn into_lines(self) -> Vec<Line<'static>>;
}

impl IntoLines for Vec<Vec<Cell>> {
    fn into_lines(self) -> Vec<Line<'static>> {
        self.into_iter()
            .map(|row| {
                Line::from(
                    row.into_iter()
                        .map(|cell| match cell {
                            Some((character, color)) => {
                                Span::styled(character.to_string(), Style::new().fg(color))
                            }
                            None => Span::raw(" "),
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .collect()
    }
}

/// Whether everything is silent: every band at zero draws nothing at all
/// (issue #40) — no baseline, no dot, no rain.
fn silent(levels: &[f32]) -> bool {
    levels.iter().all(|&level| level <= 0.0)
}

/// A level as filled eighths of `steps`, clamped to the area.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "a level is 0..=1 and a bar a few dozen steps"
)]
fn filled_steps(level: f32, steps: usize) -> usize {
    (level.clamp(0.0, 1.0) * steps as f32).round() as usize
}

/// A level as a row index, `0` at the top of `height` rows.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "a level is 0..=1 and the area a few rows tall"
)]
fn row_of(level: f32, height: usize) -> usize {
    let span = height.saturating_sub(1);
    height - 1 - ((level.clamp(0.0, 1.0) * span as f32).round() as usize).min(span)
}

/// The bars as an outline: only each bar's top edge, in the bar's colour,
/// with the spectrum's quiet baseline under the bands that are silent while
/// others sound.
fn outline_lines(levels: &[f32], height: usize, bands: usize, theme: &Theme) -> Vec<Line<'static>> {
    let steps = height * 8;
    let quiet = silent(levels);
    let mut cells = vec![vec![None; bands * 2]; height];
    for (band, &level) in levels.iter().enumerate() {
        let filled = filled_steps(level, steps);
        let column = band * 2;
        if filled == 0 {
            if !quiet {
                cells[height - 1][column] = Some(('▁', theme.border));
            }
            continue;
        }
        let top = (filled - 1) / 8;
        let remainder = filled - top * 8;
        cells[height - 1 - top][column] = Some((
            super::EIGHTHS[remainder],
            super::bar_color(top, height, theme),
        ));
    }
    cells.into_lines()
}

/// One line undulating with the envelope: a dot per column, its height the
/// level of the band the column stands in.
fn wave_lines(
    levels: &[f32],
    height: usize,
    width: usize,
    bands: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut cells = vec![vec![None; width]; height];
    for column in 0..width {
        let level = levels[column * bands / width.max(1)];
        if level <= 0.0 {
            continue;
        }
        cells[row_of(level, height)][column] = Some(('•', theme.accent));
    }
    cells.into_lines()
}

/// A dot per band at its height, on the band's own column — no air column,
/// so the dots sit at every other cell across the width.
fn scatter_lines(
    levels: &[f32],
    height: usize,
    width: usize,
    bands: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut cells = vec![vec![None; width]; height];
    for (band, &level) in levels.iter().enumerate() {
        if level <= 0.0 {
            continue;
        }
        let column = band * width / bands.max(1);
        cells[row_of(level, height)][column] = Some(('●', theme.accent_bright));
    }
    cells.into_lines()
}

/// Each band's bar with a peak marker above it: at the session's start the
/// peak sits [`FLAME_PEAK_ROWS`] rows above the bar and falls one row every
/// [`FLAME_FALL_MILLIS`] until it rests on the bar. Elapsed time drives it —
/// never a random source (issue #40, S2).
fn flame_lines(
    levels: &[f32],
    elapsed: Duration,
    height: usize,
    bands: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let steps = height * 8;
    let quiet = silent(levels);
    let fallen = usize::try_from(elapsed.as_millis() / FLAME_FALL_MILLIS).unwrap_or(usize::MAX);
    let above = FLAME_PEAK_ROWS.saturating_sub(fallen);
    let mut cells = vec![vec![None; bands * 2]; height];
    for (band, &level) in levels.iter().enumerate() {
        let filled = filled_steps(level, steps);
        let column = band * 2;
        if filled == 0 {
            if !quiet {
                cells[height - 1][column] = Some(('▁', theme.border));
            }
            continue;
        }
        // The bar itself, filling from the bottom like `bars`.
        for from_bottom in 0..height {
            let here = filled.saturating_sub(from_bottom * 8).min(8);
            if here > 0 {
                cells[height - 1 - from_bottom][column] = Some((
                    super::EIGHTHS[here],
                    super::bar_color(from_bottom, height, theme),
                ));
            }
        }
        // The peak above the bar's top, falling back over time.
        let top = (filled - 1) / 8;
        let peak = (top + above).min(height - 1);
        cells[height - 1 - peak][column] = Some(('▄', theme.accent_bright));
    }
    cells.into_lines()
}

/// Columns raining down: each loud band's head at its height, with a trail
/// below it that advances with elapsed time. The trail is derived from the
/// row, the band and the elapsed step — deterministic, never random (issue
/// #40, S2).
fn matrix_lines(
    levels: &[f32],
    elapsed: Duration,
    height: usize,
    bands: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let step = usize::try_from(elapsed.as_millis() / RAIN_STEP_MILLIS).unwrap_or(usize::MAX);
    let mut cells = vec![vec![None; bands * 2]; height];
    for (band, &level) in levels.iter().enumerate() {
        if level <= 0.0 {
            continue;
        }
        let column = band * 2;
        let head = row_of(level, height);
        cells[head][column] = Some(('█', theme.accent));
        for (row, trail) in cells.iter_mut().enumerate().skip(head + 1).take(height) {
            if (row * 7 + band * 13 + step) % 5 < 2 {
                trail[column] = Some(('▒', theme.border));
            }
        }
    }
    cells.into_lines()
}

/// The raw waveform: one dot per column, up for positive samples and down
/// for negative ones, about the middle row.
fn scope_lines(waveform: &[f32], height: usize, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let mut cells = vec![vec![None; width]; height];
    if waveform.is_empty() || waveform.iter().all(|&sample| sample.abs() <= f32::EPSILON) {
        return cells.into_lines();
    }
    let centre = height / 2;
    for column in 0..width {
        let sample = waveform[column * waveform.len() / width.max(1)];
        if sample.abs() <= f32::EPSILON {
            continue;
        }
        let span = f32::from(u16::try_from(height.saturating_sub(1)).unwrap_or(u16::MAX)) / 2.0;
        let offset = ((sample * span).clamp(-span, span)).round();
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the offset is clamped to +-span, half the area's rows"
        )]
        let offset = offset.abs() as usize;
        // The sign was read before the absolute value: positive samples
        // rise above the centre row, negative ones fall below it.
        let row = if sample >= 0.0 {
            centre.saturating_sub(offset)
        } else {
            centre.saturating_add(offset)
        }
        .min(height - 1);
        cells[row][column] = Some(('●', theme.accent));
    }
    cells.into_lines()
}

/// Bars mirrored from the centre: the two rows about the middle fill first
/// and the shape grows outward symmetrically.
fn mirror_lines(levels: &[f32], height: usize, bands: usize, theme: &Theme) -> Vec<Line<'static>> {
    let centre = height / 2;
    // The half the bars grow through: from the centre row outward.
    let steps = height.div_ceil(2) * 8;
    let mut cells = vec![vec![None; bands * 2]; height];
    for (band, &level) in levels.iter().enumerate() {
        let filled = filled_steps(level, steps);
        if filled == 0 {
            continue;
        }
        let column = band * 2;
        for (row, cells) in cells.iter_mut().enumerate().take(height) {
            let distance = if row < centre {
                centre - 1 - row
            } else {
                row - centre
            };
            let here = filled.saturating_sub(distance * 8).min(8);
            if here > 0 {
                cells[column] = Some((
                    super::EIGHTHS[here],
                    super::bar_color(distance, height, theme),
                ));
            }
        }
    }
    cells.into_lines()
}

#[cfg(test)]
mod tests {
    use super::Visualizer;

    #[test]
    fn cycling_wraps_through_none_in_both_directions() {
        assert_eq!(Visualizer::Mirror.next(), Visualizer::None);
        assert_eq!(Visualizer::None.next(), Visualizer::Bars);
        assert_eq!(Visualizer::Bars.previous(), Visualizer::None);
        assert_eq!(Visualizer::None.previous(), Visualizer::Mirror);
    }
}
