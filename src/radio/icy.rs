//! ICY (Shoutcast) metadata demultiplexing for live radio streams.
//!
//! The client asks for metadata with the request header `Icy-MetaData: 1`;
//! the server then answers with `Icy-Metaint: N` and interleaves the audio
//! with metadata blocks: `N` audio bytes, one length byte `L`, then `L × 16`
//! metadata bytes. `L = 0` means "nothing changed": the block is a single
//! byte and carries no payload at all. The payload is ASCII like
//! `StreamTitle='Artist - Title';`, NUL-padded to a multiple of 16.
//!
//! [`IcyReader`] strips the metadata so a decoder only ever sees audio, while
//! keeping the latest [`StreamTitle`](IcyReader::title) observable for the
//! UI. Without `Icy-Metaint` the reader is a pure passthrough.

use std::io::{self, Read};

/// Extracts the `StreamTitle` value from one ICY metadata payload.
///
/// Trims the NUL padding, the surrounding quotes and the trailing `;`.
/// Returns `None` when the payload carries no `StreamTitle` (other keys
/// exist in the wild) or when the title is empty.
#[must_use]
pub fn parse_stream_title(payload: &[u8]) -> Option<&str> {
    let end = payload
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(payload.len());
    let text = std::str::from_utf8(&payload[..end]).ok()?;
    let value = &text[text.find("StreamTitle=")? + "StreamTitle=".len()..];
    let value = value.strip_prefix('\'')?;
    let title = &value[..value.find('\'')?];
    if title.is_empty() { None } else { Some(title) }
}

/// A [`Read`] adapter that removes ICY metadata blocks from a live stream.
///
/// Only audio bytes are ever returned; metadata is consumed internally and
/// the latest title is kept for the UI. Short reads from the inner reader and
/// short reads handed back to the caller are both fine.
pub struct IcyReader<R: Read> {
    inner: R,
    /// Audio bytes per block; `None` means the stream carries no metadata.
    metaint: Option<usize>,
    /// Audio bytes still owed before the next length byte.
    audio_left: usize,
    title: Option<String>,
    title_seq: u64,
}

impl<R: Read> IcyReader<R> {
    #[must_use]
    pub fn new(inner: R, metaint: Option<usize>) -> Self {
        // A zero metaint would loop forever; treat it as no metadata.
        let metaint = metaint.filter(|&n| n > 0);
        Self {
            inner,
            metaint,
            audio_left: metaint.unwrap_or(0),
            title: None,
            title_seq: 0,
        }
    }

    /// The latest `StreamTitle`, or `None` before the first one arrives.
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Changes only when [`title`](Self::title) changes, so a station that
    /// repeats the same title block after block never repaints the UI.
    #[must_use]
    pub fn title_seq(&self) -> u64 {
        self.title_seq
    }

    /// Reads the single length byte that opens a metadata block.
    /// `Ok(None)` is a clean end of stream at a block boundary.
    fn read_length_byte(&mut self) -> io::Result<Option<u8>> {
        let mut byte = [0u8; 1];
        loop {
            match self.inner.read(&mut byte) {
                Ok(0) => return Ok(None),
                Ok(_) => return Ok(Some(byte[0])),
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    /// Consumes one metadata block and arms the next audio block.
    ///
    /// The block size needs no validation above this layer: `L` is one wire
    /// byte, so `L × 16` cannot exceed 4080. A corrupt `Icy-Metaint` header is
    /// the caller's to reject.
    fn next_block(&mut self, metaint: usize) -> io::Result<()> {
        let Some(len_byte) = self.read_length_byte()? else {
            return Ok(());
        };
        let meta_len = usize::from(len_byte) * 16;
        if meta_len > 0 {
            let mut payload = vec![0u8; meta_len];
            self.inner.read_exact(&mut payload)?;
            if let Some(title) = parse_stream_title(&payload)
                && self.title.as_deref() != Some(title)
            {
                self.title_seq += 1;
                self.title = Some(title.to_owned());
            }
        }
        self.audio_left = metaint;
        Ok(())
    }
}

impl<R: Read> Read for IcyReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let Some(metaint) = self.metaint else {
            return self.inner.read(buf);
        };
        if buf.is_empty() {
            return Ok(0);
        }
        if self.audio_left == 0 {
            self.next_block(metaint)?;
            // A clean end of stream at a block boundary still reads as EOF.
            if self.audio_left == 0 {
                return Ok(0);
            }
        }
        let want = buf.len().min(self.audio_left);
        let n = self.inner.read(&mut buf[..want])?;
        self.audio_left -= n;
        Ok(n)
    }
}
