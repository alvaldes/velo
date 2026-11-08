//! MP4 container priming (T16).
//!
//! An AAC encoder emits `delay` warm-up frames before the music and `padding`
//! flush frames after it. symphonia's isomp4 reader has never populated the
//! track's `delay`/`padding` (re-measured on 0.6.1; unlike its MP3 reader,
//! which trims both when the decoder's `gapless` option is on), so an M4A join
//! carries the previous track's warm-up into the seam. This module reads the
//! priming the container declares so [`crate::audio::TrackSource`] can trim it
//! where symphonia would.
//!
//! Two sources, in this order of trust:
//!
//! 1. `iTunSMPB` — a freeform `----:com.apple.iTunes:iTunSMPB` tag whose second
//!    and third hex fields state the delay and the padding explicitly.
//! 2. the `elst` edit list of the audio track — a positive `media_time` is the
//!    delay, in the media timescale; an edit list says nothing about padding.
//!
//! This is a focused reader for exactly those atoms, not a general ISO-BMFF
//! library: it walks the box tree, refuses anything that is not an MP4 file
//! (the first top-level box must be `ftyp`), and bails out to `None` on
//! anything unexpected.

use std::fs::File;
use std::io::Read;
use std::path::Path;

/// Maximum box payload the reader will hold in memory. A `moov` is at most a
/// few KiB per track; anything larger is not a file worth trimming.
const MAX_BOX_BYTES: usize = 16 * 1024 * 1024;

/// Maximum box-tree depth. `moov/trak/edts/elst` and `moov/udta/meta/ilst/…`
/// never nest deeper than this.
const MAX_DEPTH: usize = 6;

/// Encoder priming the container declares, in media-timescale frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Priming {
    /// Warm-up frames before the first content frame.
    pub(crate) delay: u64,
    /// Flush frames after the last content frame.
    pub(crate) padding: u64,
}

/// A box: four-byte big-endian size (the classic header only; this reader does
/// not need 64-bit sizes), four-byte type, payload bounds inside the buffer.
struct Box<'a> {
    kind: [u8; 4],
    payload: &'a [u8],
}

/// Parses the boxes directly inside `data`.
fn children(data: &[u8]) -> Vec<Box<'_>> {
    let mut boxes = Vec::new();
    let mut pos = 0usize;
    while pos + 8 <= data.len() {
        let size = u32::from_be_bytes(data[pos..pos + 4].try_into().expect("4 bytes")) as usize;
        let mut kind = [0u8; 4];
        kind.copy_from_slice(&data[pos + 4..pos + 8]);
        if size < 8 || pos + size > data.len() {
            break;
        }
        boxes.push(Box {
            kind,
            payload: &data[pos + 8..pos + size],
        });
        pos += size;
    }
    boxes
}

fn child<'a>(boxes: &[Box<'a>], kind: [u8; 4]) -> Option<&'a [u8]> {
    boxes
        .iter()
        .find(|box_| box_.kind == kind)
        .map(|box_| box_.payload)
}

/// The numeric value of a full box's 4-byte version/flags field.
fn version_flags(payload: &[u8]) -> Option<u32> {
    payload
        .first_chunk::<4>()
        .map(|bytes| u32::from_be_bytes(*bytes))
}

/// A string payload up to its NUL terminator, if it is printable.
fn text(payload: &[u8]) -> Option<&str> {
    let end = payload
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(payload.len());
    std::str::from_utf8(&payload[..end]).ok()
}

/// Parses the delay and padding out of an `iTunSMPB` value, e.g.
/// `00000000 00000400 00000064 …`: field 0 is a version, field 1 the delay,
/// field 2 the padding, all hexadecimal.
fn parse_itunsmpb(value: &str) -> Option<Priming> {
    let mut fields = value.split_ascii_whitespace();
    let _version = u64::from_str_radix(fields.next()?, 16).ok()?;
    let delay = u64::from_str_radix(fields.next()?, 16).ok()?;
    let padding = u64::from_str_radix(fields.next()?, 16).ok()?;
    Some(Priming { delay, padding })
}

/// Parses one `elst` payload and returns the delay its first non-negative
/// `media_time` states, if any.
fn parse_elst(payload: &[u8]) -> Option<Priming> {
    let flags = version_flags(payload)?;
    let version = flags >> 24;
    let entry_size = if version == 1 { 20 } else { 12 };
    let entries = u32::from_be_bytes(payload.get(4..8)?.try_into().ok()?) as usize;
    let mut pos = 8usize;
    for _ in 0..entries {
        let entry = payload.get(pos..pos + entry_size)?;
        // An entry is [segment_duration, media_time, media_rate]: the delay
        // is the second field, not the first.
        let media_time = if version == 1 {
            i64::from_be_bytes(entry[8..16].try_into().ok()?)
        } else {
            i64::from(i32::from_be_bytes(entry[4..8].try_into().ok()?))
        };
        if media_time > 0 {
            return Some(Priming {
                delay: u64::try_from(media_time).ok()?,
                padding: 0,
            });
        }
        pos += entry_size;
    }
    None
}

/// Finds the payload of the first audio `trak` (its `hdlr` says `soun`).
fn find_audio_trak(payload: &[u8], depth: usize) -> Option<&[u8]> {
    if depth > MAX_DEPTH {
        return None;
    }
    children(payload).into_iter().find_map(|box_| {
        if box_.kind != *b"trak" {
            return None;
        }
        // The handler type lives in `mdia/hdlr`, one level below the `trak`.
        let is_audio = children(box_.payload)
            .into_iter()
            .filter(|box_| box_.kind == *b"mdia")
            .any(|mdia| {
                children(mdia.payload).into_iter().any(|box_| {
                    box_.kind == *b"hdlr"
                        && box_.payload.get(8..12).is_some_and(|kind| kind == *b"soun")
                })
            });
        is_audio.then_some(box_.payload)
    })
}

/// Reads the priming an audio `trak`'s edit list declares: the first
/// non-negative `media_time` is the delay. An edit list says nothing about
/// padding.
fn trak_elst(trak: &[u8]) -> Option<Priming> {
    let edts = child(&children(trak), *b"edts")?;
    let elst = child(&children(edts), *b"elst")?;
    parse_elst(elst)
}

/// Finds the `iTunSMPB` freeform tag's `----` box, if one is present under the
/// usual metadata ancestors.
fn find_itunsmpb_box(payload: &[u8], depth: usize) -> Option<&[u8]> {
    if depth > MAX_DEPTH {
        return None;
    }
    children(payload)
        .into_iter()
        .find_map(|box_| match &box_.kind {
            b"----" => Some(box_.payload),
            b"udta" | b"ilst" => find_itunsmpb_box(box_.payload, depth + 1),
            b"meta" => {
                // `meta` is a full box: its children start after 4
                // version/flags bytes.
                let inner = box_.payload.get(4..).unwrap_or(&[]);
                find_itunsmpb_box(inner, depth + 1)
            }
            _ => None,
        })
}

/// Reads the `iTunSMPB` value out of an `ilst` entry: `----` containing `mean`
/// (`com.apple.iTunes`), `name` (`iTunSMPB`) and `data` (UTF-8 text).
fn parse_freeform(payload: &[u8]) -> Option<Priming> {
    let boxes = children(payload);
    // `mean` and `name` are full boxes: the text starts after 4 version/flags
    // bytes.
    let mean = child(&boxes, *b"mean").and_then(|payload| text(payload.get(4..)?))?;
    let name = child(&boxes, *b"name").and_then(|payload| text(payload.get(4..)?))?;
    if mean != "com.apple.iTunes" || name != "iTunSMPB" {
        return None;
    }
    let data = child(&boxes, *b"data")?;
    // data: version/flags (4), locale (4), then the value text.
    let value = text(data.get(8..)?)?;
    parse_itunsmpb(value)
}

/// Reads the priming an MP4 file declares, or `None` when the file is not MP4
/// or declares none.
///
/// The gate is deliberate: an ISO-BMFF file begins with `ftyp`, so anything
/// else (MP3, FLAC, WAV, Ogg) is refused before a single nested box is read.
pub(crate) fn read_priming(path: &Path) -> Option<Priming> {
    let mut file = File::open(path).ok()?;

    // Refuse non-MP4 files on the first box header alone.
    let mut header = [0u8; 8];
    file.read_exact(&mut header).ok()?;
    if &header[4..8] != b"ftyp" {
        return None;
    }
    let ftyp_size = u32::from_be_bytes(header[0..4].try_into().ok()?) as usize;
    if !(8..=MAX_BOX_BYTES).contains(&ftyp_size) {
        return None;
    }
    let mut ftyp = vec![0u8; ftyp_size - 8];
    file.read_exact(&mut ftyp).ok()?;

    let mut moov = None;
    // Top-level boxes after `ftyp`, looking for `moov`.
    loop {
        let mut header = [0u8; 8];
        match file.read_exact(&mut header) {
            Ok(()) => {}
            Err(_) => break,
        }
        let size = u32::from_be_bytes(header[0..4].try_into().ok()?) as usize;
        let kind: [u8; 4] = header[4..8].try_into().ok()?;
        if size < 8 {
            break;
        }
        if kind == *b"moov" {
            if size - 8 > MAX_BOX_BYTES {
                return None;
            }
            let mut payload = vec![0u8; size - 8];
            file.read_exact(&mut payload).ok()?;
            moov = Some(payload);
            break;
        }
        // Skip every other top-level box (`mdat` can be the whole file).
        let skip = u64::try_from(size - 8).ok()?;
        std::io::copy(&mut file.by_ref().take(skip), &mut std::io::sink()).ok()?;
    }

    let moov = moov?;
    // Order of trust: the explicit tag, then the edit list.
    let itunsmpb = find_itunsmpb_box(&moov, 0).and_then(parse_freeform);
    let elst = find_audio_trak(&moov, 0).and_then(trak_elst);
    itunsmpb.or(elst)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    #[test]
    fn tiny_m4a_declares_1024_frames_of_priming_in_its_elst() {
        let priming = read_priming(&fixture("tiny.m4a")).expect("the elst declares a delay");
        assert_eq!(
            priming,
            Priming {
                delay: 1024,
                padding: 0
            }
        );
    }

    #[test]
    fn itunsmpb_wins_over_the_elst_when_present() {
        let priming = read_priming(&fixture("priming-itunsmpb.m4a"))
            .expect("the tag declares delay and padding");
        assert_eq!(
            priming,
            Priming {
                delay: 512,
                padding: 100
            }
        );
    }

    #[test]
    fn non_mp4_files_declare_no_priming() {
        // The gate is the first box: an MP3, a WAV and a FLAC all refuse before
        // any nested box is read.
        assert_eq!(read_priming(&fixture("silence-then-tone.mp3")), None);
        assert_eq!(read_priming(&fixture("mono-tone.wav")), None);
    }

    #[test]
    fn a_truncated_file_declares_no_priming() {
        // Not even a full first header: no panic, no priming.
        let path = std::env::temp_dir().join(format!("velo-mp4-trunc-{}", std::process::id()));
        std::fs::write(&path, [0u8, 0, 0, 0, b'f', b't', b'y']).expect("write scratch");
        let priming = read_priming(&path);
        let _ = std::fs::remove_file(&path);
        assert_eq!(priming, None);
    }
}
