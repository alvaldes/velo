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
    /// The cover of a file, decoded: the first picture of its primary tag,
    /// or else the cover image in its folder (`cover`, `folder` or `front`,
    /// as `.jpg`, `.jpeg` or `.png`, in any case) — the way most albums are
    /// ripped. A missing file, tag, picture or cover, and bytes that are not
    /// an image are all `None` rather than an error — a cover is decoration,
    /// and nothing about it may break the interface.
    #[must_use]
    pub fn from_path(path: &Path) -> Option<Artwork> {
        embedded(path)
            .or_else(|| folder_cover(path))
            .map(|image| Artwork { image })
    }

    /// The cover that already sits in a file of its own: a remote track's
    /// cover fetched into the covers cache (T12d) is a JPEG or PNG on disk
    /// with no audio around it, so there are no tags to read. The bytes, not
    /// the name, decide the format; bytes that are not an image are `None`,
    /// like every other cover failure — decoration must not break the
    /// interface.
    #[must_use]
    pub fn from_image_file(path: &Path) -> Option<Artwork> {
        image::ImageReader::open(path)
            .ok()?
            .with_guessed_format()
            .ok()?
            .decode()
            .ok()
            .map(|image| Artwork { image })
    }

    /// A decoded cover for the interface's own tests: the layout and the
    /// `[ui]` switches never look at the pixels, so any image will do.
    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self {
            image: DynamicImage::new_rgba8(8, 8),
        }
    }

    /// The decoded cover at its own resolution.
    #[must_use]
    pub const fn image(&self) -> &DynamicImage {
        &self.image
    }
}

/// The first picture of the file's primary tag.
fn embedded(path: &Path) -> Option<DynamicImage> {
    let tagged = lofty::read_from_path(path).ok()?;
    let tag = tagged.primary_tag()?;
    let picture = tag.pictures().first()?;
    image::load_from_memory(picture.data()).ok()
}

/// The names a folder's cover goes by, in the order they are preferred.
const COVER_STEMS: [&str; 3] = ["cover", "folder", "front"];
const COVER_EXTENSIONS: [&str; 3] = ["jpg", "jpeg", "png"];

/// The cover image beside the file, matched without regard to case. Read
/// once per track change, like the embedded picture, never per frame.
fn folder_cover(path: &Path) -> Option<DynamicImage> {
    let folder = path.parent()?;
    let mut candidates: Vec<(usize, std::path::PathBuf)> = std::fs::read_dir(folder)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            let (stem, extension) = name.rsplit_once('.')?;
            let rank = COVER_STEMS.iter().position(|known| *known == stem)?;
            COVER_EXTENSIONS
                .contains(&extension)
                .then(|| (rank, entry.path()))
        })
        .collect();
    candidates.sort();
    // Decoded by what the bytes are, not by the name: a PNG saved as
    // `cover.jpg` is common enough and still a cover.
    candidates.into_iter().find_map(|(_, cover)| {
        image::ImageReader::open(cover)
            .ok()?
            .with_guessed_format()
            .ok()?
            .decode()
            .ok()
    })
}

/// The window-size gate for the cover, with the area the first layout gave
/// it (the rightmost quarter above the footer). The interface now places the
/// cover inside the now-playing block and only asks whether this is `Some`:
/// `None` when the terminal is below the threshold, and then the interface
/// stays exactly as it is without art.
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

/// Why a cover that exists is not on screen, in one sentence.
///
/// Art that is silently absent is indistinguishable from art that is broken.
/// This is the difference: no picture, no protocol and too small a window all
/// end with nothing drawn, and only the last two are worth telling the user.
#[must_use]
pub fn hidden_reason(has_protocol: bool, has_art: bool, area: Rect) -> Option<String> {
    if !has_art {
        return None;
    }
    if !has_protocol {
        return Some(
            "this terminal reported no image protocol (kitty, sixel or iterm2)".to_owned(),
        );
    }
    if pane(area).is_some() {
        return None;
    }
    Some(format!(
        "the window is {}x{} and cover art needs {ART_MIN_COLUMNS}x{ART_MIN_ROWS}",
        area.width, area.height
    ))
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
