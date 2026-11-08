//! ICY metadata demux tests (spec S4): length-prefixed metadata blocks,
//! `StreamTitle` extraction, and passthrough without `Icy-Metaint`.
//!
//! All fixtures are built in-process with `Cursor`; no network, no new
//! dependencies.

use std::io::{Cursor, Read};

use velo::radio::icy::{IcyReader, parse_stream_title};

/// One ICY block: `audio` bytes, then a length byte and the already padded
/// `meta` payload (multiple of 16). An empty `meta` is the `L = 0` case.
fn raw_block(audio: &[u8], meta: &[u8]) -> Vec<u8> {
    assert_eq!(meta.len() % 16, 0, "test helper expects padded metadata");
    let mut out = audio.to_vec();
    out.push(
        u8::try_from(meta.len() / 16).expect("test fixtures never exceed 255 metadata chunks"),
    );
    out.extend_from_slice(meta);
    out
}

/// Pads a metadata payload with NUL bytes up to a multiple of 16.
fn padded(payload: &str) -> Vec<u8> {
    let mut meta = payload.as_bytes().to_vec();
    meta.resize(meta.len().div_ceil(16) * 16, 0);
    meta
}

/// A block whose metadata announces `StreamTitle='<title>';`.
fn titled_block(audio: &[u8], title: &str) -> Vec<u8> {
    raw_block(audio, &padded(&format!("StreamTitle='{title}';")))
}

/// Distinct audio bytes so leaked metadata can never pass for audio.
fn audio(n: usize, seed: u8) -> Vec<u8> {
    let mut bytes = vec![seed; n];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = byte.wrapping_add(u8::try_from(i).unwrap_or(u8::MAX));
    }
    bytes
}

fn read_all<R: Read>(reader: &mut R) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    reader.read_to_end(&mut out)?;
    Ok(out)
}

#[test]
fn no_metaint_is_a_byte_for_byte_passthrough() {
    let data: Vec<u8> = (0..=u8::MAX).collect();
    let mut reader = IcyReader::new(Cursor::new(data.clone()), None);
    assert_eq!(read_all(&mut reader).unwrap(), data);
    assert_eq!(reader.title(), None);
    assert_eq!(reader.title_seq(), 0);
}

#[test]
fn zero_length_block_yields_the_next_audio() {
    // After `audio_a` comes a bare L=0 byte, then `audio_b` immediately: the
    // whole block is 1 + 0 * 16 bytes, so any extra skip corrupts the audio.
    let (a, b) = (audio(8, 1), audio(8, 100));
    let stream = [raw_block(&a, &[]), raw_block(&b, &[])].concat();
    let mut reader = IcyReader::new(Cursor::new(stream), Some(8));
    assert_eq!(read_all(&mut reader).unwrap(), [a, b].concat());
    assert_eq!(reader.title(), None);
    assert_eq!(reader.title_seq(), 0);
}

#[test]
fn a_title_block_is_stripped_from_the_audio() {
    let (a, b) = (audio(8, 1), audio(8, 200));
    let stream = [titled_block(&a, "Artist - Title"), raw_block(&b, &[])].concat();
    let mut reader = IcyReader::new(Cursor::new(stream), Some(8));
    assert_eq!(read_all(&mut reader).unwrap(), [a, b].concat());
    assert_eq!(reader.title(), Some("Artist - Title"));
    assert_eq!(reader.title_seq(), 1);
}

#[test]
fn a_repeated_title_does_not_bump_the_counter() {
    let blocks: Vec<Vec<u8>> = [audio(4, 1), audio(4, 50), audio(4, 99)]
        .iter()
        .map(|a| titled_block(a, "Same Song"))
        .collect();
    let stream = blocks.concat();
    let mut reader = IcyReader::new(Cursor::new(stream), Some(4));
    assert_eq!(read_all(&mut reader).unwrap().len(), 12);
    assert_eq!(reader.title(), Some("Same Song"));
    assert_eq!(reader.title_seq(), 1);
}

#[test]
fn a_changed_title_bumps_the_counter() {
    let stream = [
        titled_block(&audio(4, 1), "First"),
        titled_block(&audio(4, 2), "Second"),
    ]
    .concat();
    let mut reader = IcyReader::new(Cursor::new(stream), Some(4));
    assert_eq!(read_all(&mut reader).unwrap().len(), 8);
    assert_eq!(reader.title(), Some("Second"));
    assert_eq!(reader.title_seq(), 2);
}

#[test]
fn nul_padding_is_trimmed() {
    let mut meta = b"StreamTitle='Pad';".to_vec();
    meta.resize(32, 0); // two full chunks, the tail all NULs
    let stream = [raw_block(&audio(4, 7), &meta)].concat();
    let mut reader = IcyReader::new(Cursor::new(stream), Some(4));
    assert_eq!(read_all(&mut reader).unwrap().len(), 4);
    assert_eq!(reader.title(), Some("Pad"));
}

#[test]
fn a_block_without_stream_title_is_ignored() {
    let (a, b) = (audio(4, 1), audio(4, 2));
    let stream = [
        raw_block(&a, &[]),
        raw_block(&b, &padded("ice-bitrate='128';")),
    ]
    .concat();
    let mut reader = IcyReader::new(Cursor::new(stream), Some(4));
    assert_eq!(read_all(&mut reader).unwrap(), [a, b].concat());
    assert_eq!(reader.title(), None);
    assert_eq!(reader.title_seq(), 0);
}

#[test]
fn stream_title_is_found_among_other_keys() {
    let mut meta = b"StreamTitle='Song';StreamURL='http://example';".to_vec();
    meta.resize(64, 0);
    let stream = [raw_block(&audio(4, 3), &meta)].concat();
    let mut reader = IcyReader::new(Cursor::new(stream), Some(4));
    assert_eq!(read_all(&mut reader).unwrap().len(), 4);
    assert_eq!(reader.title(), Some("Song"));
}

#[test]
fn a_truncated_metadata_block_is_an_io_error() {
    let mut stream = audio(8, 1);
    stream.push(2); // claims 32 metadata bytes ...
    stream.extend_from_slice(b"StreamTitle='Cut"); // ... sends 16, then EOF
    let mut reader = IcyReader::new(Cursor::new(stream), Some(8));
    let err = read_all(&mut reader).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
}

#[test]
fn the_maximum_metadata_block_is_accepted() {
    // L = 255 -> 4080 metadata bytes, exactly the sane bound.
    let mut meta = b"StreamTitle='Big';".to_vec();
    meta.resize(4080, 0);
    let stream = [raw_block(&audio(4, 1), &meta), raw_block(&audio(4, 2), &[])].concat();
    let mut reader = IcyReader::new(Cursor::new(stream), Some(4));
    assert_eq!(read_all(&mut reader).unwrap().len(), 8);
    assert_eq!(reader.title(), Some("Big"));
}

#[test]
fn reading_into_a_one_byte_buffer_never_leaks_metadata() {
    let (a, b) = (audio(4, 1), audio(4, 2));
    let stream = [titled_block(&a, "X"), raw_block(&b, &[])].concat();
    let mut reader = IcyReader::new(Cursor::new(stream), Some(4));
    let mut out = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte).unwrap() {
            0 => break,
            n => out.extend_from_slice(&byte[..n]),
        }
    }
    assert_eq!(out, [a, b].concat());
    assert_eq!(reader.title(), Some("X"));
}

/// An inner reader that hands back at most one byte per call.
struct Trickle<R: Read>(R);

impl<R: Read> Read for Trickle<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        self.0.read(&mut buf[..1])
    }
}

#[test]
fn a_short_read_inner_still_delivers_all_audio() {
    let (a, b) = (audio(4, 1), audio(4, 2));
    let stream = [titled_block(&a, "X"), raw_block(&b, &[])].concat();
    let mut reader = IcyReader::new(Trickle(Cursor::new(stream)), Some(4));
    let mut out = vec![0u8; 1024];
    let mut filled = 0;
    loop {
        let n = reader.read(&mut out[filled..]).unwrap();
        if n == 0 {
            break;
        }
        filled += n;
    }
    assert_eq!(&out[..filled], &[a, b].concat()[..]);
    assert_eq!(reader.title(), Some("X"));
}

#[test]
fn parse_stream_title_edge_cases() {
    assert_eq!(parse_stream_title(b"StreamTitle='A';"), Some("A"));
    assert_eq!(parse_stream_title(b"StreamTitle='A';\0\0"), Some("A"));
    assert_eq!(parse_stream_title(b"StreamTitle='';"), None);
    assert_eq!(parse_stream_title(b"ice-bitrate='128';"), None);
    assert_eq!(parse_stream_title(b""), None);
    assert_eq!(parse_stream_title(&[0xff, 0xfe]), None);
}
