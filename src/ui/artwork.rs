//! Cover art (T8, S10): the picture of the track that is playing, shown in
//! the terminal through kitty, sixel or iTerm2.
//!
//! Two halves, both testable without a terminal:
//!
//! * [`Artwork`] extracts the first picture of a file's primary tag with
//!   `lofty` and decodes it with `image`, once per track change. It is never
//!   re-read per frame and never touched inside the audio callback.
//! * [`pane`] is the pure layout rule: it refuses art on a terminal too small
//!   to keep the track list readable, and caps the art at a quarter of the
//!   width when it does allow it.
//!
//! A file with no picture, undecodable picture bytes, and a terminal that
//! speaks no graphics protocol all leave the interface exactly as it draws
//! without art — half-block characters would only fake a cover, so velo does
//! not do that either.

use std::path::Path;

use image::DynamicImage;
use lofty::file::TaggedFileExt;
use ratatui::layout::Rect;
use ratatui_image::picker::ProtocolType;

pub use ratatui_image::StatefulImage;
pub use ratatui_image::picker::Picker;
pub use ratatui_image::protocol::StatefulProtocol;

/// The two rows the footer takes; the art spans everything above them.
const FOOTER_ROWS: u16 = 2;

/// The smallest terminal that can still keep the track list readable beside
/// the art. Every rendering rule below this threshold was designed and
/// verified at 80×24, so art may only appear well above it.
const ART_MIN_COLUMNS: u16 = 100;
const ART_MIN_ROWS: u16 = 28;

/// The decoded cover of the track that is playing.
pub struct Artwork {
    image: DynamicImage,
}

impl Artwork {
    /// The first picture of the file's primary tag, decoded. A missing file,
    /// a missing tag, a missing picture, and picture bytes that are not an
    /// image are all `None` rather than an error — a cover is decoration, and
    /// nothing about it may break the interface.
    #[must_use]
    pub fn from_path(path: &Path) -> Option<Artwork> {
        let tagged = lofty::read_from_path(path).ok()?;
        let tag = tagged.primary_tag()?;
        let picture = tag.pictures().first()?;
        let image = image::load_from_memory(picture.data()).ok()?;
        Some(Artwork { image })
    }

    /// The decoded cover at its own resolution.
    #[must_use]
    pub const fn image(&self) -> &DynamicImage {
        &self.image
    }
}

/// Where the art sits beside the header and the body: the rightmost quarter
/// of the width, spanning the rows above the footer. `None` when the
/// terminal is below the threshold — then the interface stays exactly as it
/// is without art, and the track list keeps its full width.
#[must_use]
pub fn pane(area: Rect) -> Option<Rect> {
    if area.width < ART_MIN_COLUMNS || area.height < ART_MIN_ROWS {
        return None;
    }
    let width = area.width / 4;
    let height = area.height - FOOTER_ROWS;
    (width > 0).then(|| Rect {
        x: area.x + area.width - width,
        y: area.y,
        width,
        height,
    })
}

/// One-time detection at startup: asks the terminal which graphics protocol
/// it speaks. `None` means draw no art — not a terminal, or a terminal that
/// supports none of the protocols; the half-block fallback of `ratatui-image`
/// would fake a cover with block characters, and velo declines to do that.
#[cfg(unix)]
#[must_use]
pub fn detect() -> Option<Picker> {
    let mut picker = Picker::from_termios().ok()?;
    (picker.guess_protocol() != ProtocolType::Halfblocks).then_some(picker)
}

/// Windows must compile; art detection simply has no terminal query there.
#[cfg(not(unix))]
#[must_use]
pub fn detect() -> Option<Picker> {
    None
}
