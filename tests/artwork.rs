//! Cover art: extraction from the playing file's tags and the layout rule
//! that keeps the track list readable (T8, S10).
//!
//! The fixtures are written by these tests themselves: a real WAV with an
//! embedded picture, round-tripped through `lofty`, so no committed asset is
//! ever depended on. The layout rule is a pure function, tested without a
//! terminal, which is what makes the "does the interface stay readable"
//! threshold a rule instead of an opinion.

use std::fs;
use std::io::Cursor;
use std::path::PathBuf;

use image::{ExtendedColorType, GenericImageView, ImageBuffer, ImageEncoder, Rgba};
use lofty::config::WriteOptions;
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::prelude::*;
use lofty::tag::Tag;
use lofty::tag::TagType;
use velo::ui::artwork::{Artwork, pane};
use velo::ui::ratatui::layout::Rect;

/// An 8×8 cover whose every pixel is addressable, encoded as PNG in memory.
fn cover_png() -> Vec<u8> {
    let image: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::from_fn(8, 8, |x, y| {
        Rgba([
            u8::try_from(x * 31).unwrap_or(0),
            u8::try_from(y * 31).unwrap_or(0),
            7,
            255,
        ])
    });
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(Cursor::new(&mut bytes))
        .write_image(image.as_raw(), 8, 8, ExtendedColorType::Rgba8)
        .expect("the fixture PNG encodes");
    bytes
}

/// Writes a short PCM WAV (0.1 s, 44.1 kHz, stereo), then attaches the given
/// picture bytes as an `ID3v2` cover with `lofty`, exactly the way a tagging
/// program would.
fn tagged_wav(dir: &str, name: &str, picture: Option<&[u8]>) -> PathBuf {
    let root = std::env::temp_dir().join(format!("velo-artwork-{dir}-{}", std::process::id()));
    let _ = fs::create_dir_all(&root);
    let path = root.join(name);

    let sample_rate: u32 = 44_100;
    let channels: u16 = 2;
    let frames: usize = 4_410;
    let data_len: u32 = u32::try_from(frames * usize::from(channels) * 2).unwrap();

    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate * u32::from(channels) * 2).to_le_bytes());
    bytes.extend_from_slice(&(channels * 2).to_le_bytes()); // block align
    bytes.extend_from_slice(&16_u16.to_le_bytes()); // bits per sample
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    bytes.extend(
        std::iter::repeat_n(0_i16, frames * usize::from(channels)).flat_map(i16::to_le_bytes),
    );
    fs::write(&path, bytes).expect("the fixture WAV writes");

    if let Some(picture) = picture {
        let mut tag = Tag::new(TagType::Id3v2);
        tag.push_picture(
            Picture::unchecked(picture.to_vec())
                .pic_type(PictureType::CoverFront)
                .mime_type(MimeType::Png)
                .build(),
        );
        let mut file = lofty::read_from_path(&path).expect("the fixture WAV opens for tagging");
        file.insert_tag(tag);
        file.save_to_path(&path, WriteOptions::default())
            .expect("the picture embeds");
    }
    path
}

/// Removes the fixture directory a test created. Every test calls this at the
/// end so a failure does not leave files behind in the system temp dir.
fn clean(dir: &str) {
    let _ = fs::remove_dir_all(
        std::env::temp_dir().join(format!("velo-artwork-{dir}-{}", std::process::id())),
    );
}

#[test]
fn the_embedded_picture_round_trips_through_the_tags_and_decodes() {
    let path = tagged_wav("roundtrip", "cover.wav", Some(&cover_png()));

    let artwork = Artwork::from_path(&path).expect("a tagged cover is extracted");

    assert_eq!(
        artwork.image().width(),
        artwork.image().height(),
        "a cover is square in the fixture",
    );
    assert_eq!(
        artwork.image().width(),
        8,
        "the decoded cover keeps the fixture's dimensions"
    );
    assert_eq!(
        artwork.image().get_pixel(7, 6).0,
        [217, 186, 7, 255],
        "the decoded cover keeps the fixture's pixels"
    );
    clean("roundtrip");
}

#[test]
fn a_file_without_a_picture_yields_nothing() {
    let path = tagged_wav("no-picture", "plain.wav", None);

    assert!(
        Artwork::from_path(&path).is_none(),
        "a file with no picture must yield nothing, not an error or a panic"
    );
    clean("no-picture");
}

#[test]
fn picture_bytes_that_are_not_an_image_yield_nothing_rather_than_panicking() {
    let bogus = b"this is not an image, it is a sentence".to_vec();
    let path = tagged_wav("bogus-picture", "bogus.wav", Some(&bogus));

    assert!(
        Artwork::from_path(&path).is_none(),
        "undecodable picture bytes must be skipped quietly"
    );
    clean("bogus-picture");
}

// ---- the layout rule ------------------------------------------------------

#[test]
fn art_is_refused_on_a_small_terminal() {
    // The scale the interface was designed for and verified at.
    assert_eq!(pane(Rect::new(0, 0, 80, 24)), None);
    // One below either bound of the threshold.
    assert_eq!(pane(Rect::new(0, 0, 99, 28)), None);
    assert_eq!(pane(Rect::new(0, 0, 100, 27)), None);
    assert_eq!(pane(Rect::new(0, 0, 99, 27)), None);
}

#[test]
fn art_is_allowed_on_a_terminal_that_meets_the_threshold() {
    assert!(pane(Rect::new(0, 0, 100, 28)).is_some());
    assert!(pane(Rect::new(0, 0, 150, 40)).is_some());
    assert!(pane(Rect::new(0, 0, 240, 60)).is_some());
}

#[test]
fn art_never_takes_more_than_a_quarter_of_the_width() {
    for width in (100..=240).step_by(4) {
        for height in [28, 30, 40, 60] {
            let Some(rect) = pane(Rect::new(0, 0, width, height)) else {
                panic!("art is refused at {width}×{height}, above the threshold");
            };
            assert!(
                u32::from(rect.width) <= u32::from(width / 4),
                "art is {rect} wide at {width} columns: more than a quarter"
            );
            assert!(
                rect.x + rect.width <= width && rect.y + rect.height <= height,
                "art pane {rect} leaves the {width}×{height} frame"
            );
        }
    }
}

#[test]
fn a_hidden_cover_says_why_instead_of_vanishing() {
    use ratatui::layout::Rect;
    use velo::ui::artwork::hidden_reason;

    // No picture at all: nothing to explain, and nothing to apologise for.
    assert!(hidden_reason(true, false, Rect::new(0, 0, 120, 40)).is_none());
    // A picture, a protocol, room for it: it is on screen, nothing to say.
    assert!(hidden_reason(true, true, Rect::new(0, 0, 120, 40)).is_none());
    // A picture and nowhere to draw it: the window is the reason.
    let too_small = hidden_reason(true, true, Rect::new(0, 0, 96, 24)).expect("a reason");
    assert!(too_small.contains("96x24"), "{too_small}");
    assert!(too_small.contains("100x28"), "{too_small}");
    // A picture and a terminal that speaks no graphics protocol.
    let no_protocol = hidden_reason(false, true, Rect::new(0, 0, 120, 40)).expect("a reason");
    assert!(no_protocol.contains("no image protocol"), "{no_protocol}");
}
