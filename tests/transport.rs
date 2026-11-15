//! Transport rules (S8): the gain ramp, seeking by offset and the seek target.
//!
//! Everything here is exercised without a sound device: the ramp and the seek
//! target are pure, and a seek is observed through [`velo::audio::TrackSource`]
//! the same way the producer thread reads a track.

use std::collections::VecDeque;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use symphonia::core::io::MediaSource;
use velo::audio::AudioError;
use velo::audio::output::{
    Entry, PathResolver, SharedQueue, TrackFailure, drain_headless,
    drain_headless_with_carried_source,
};
use velo::audio::{GainRamp, TrackSource, TrackSpec, seek_target};
use velo::config::ReplayGainMode;

/// Writes a minimal PCM16 WAV file so the tests need no external tooling.
fn write_wav(path: &Path, spec: TrackSpec, frames: &[i16]) {
    let bits_per_sample: u16 = 16;
    let block_align = spec.channels * bits_per_sample / 8;
    let byte_rate = spec.sample_rate * u32::from(block_align);
    let data_len = u32::try_from(frames.len() * 2).expect("test data fits in a RIFF chunk");

    let mut out = Vec::with_capacity(44 + frames.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&spec.channels.to_le_bytes());
    out.extend_from_slice(&spec.sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits_per_sample.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for sample in frames {
        out.extend_from_slice(&sample.to_le_bytes());
    }

    std::fs::write(path, out).expect("write test wav");
}

/// A unique scratch path per test and file, removed by the caller.
fn scratch_path(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("velo-transport-{tag}-{}.wav", std::process::id()))
}

/// A per-frame ramp in a range that never reaches zero, so a wrong seek point
/// changes the sample values and cannot hide behind a constant.
fn ramp_frames(frames: usize, channels: u16, offset: i16) -> Vec<i16> {
    (0..frames)
        .flat_map(|frame| {
            let value = offset + i16::try_from(frame % 500).expect("small ramp") + 1;
            std::iter::repeat_n(value, usize::from(channels))
        })
        .collect()
}

/// The `i16` PCM the test wrote, as the `f32` samples a decoder must produce.
fn as_f32(samples: &[i16]) -> Vec<f32> {
    samples
        .iter()
        .map(|sample| f32::from(*sample) / 32_768.0)
        .collect()
}

/// Half a PCM16 step: any off-by-one frame exceeds this, float noise does not.
const PCM16_TOLERANCE: f32 = 1e-5;

/// What one audio buffer is allowed to move the gain per sample. A ramp over
/// the buffer sizes a device asks for (hundreds of samples) moves far less;
/// the bound exists to catch a gain that jumps instead of fading.
const MAX_STEP_PER_SAMPLE: f32 = 0.01;

#[test]
fn the_gain_ramp_reaches_the_target_within_one_buffer() {
    let mut ramp = GainRamp::new(0.0);
    let mut samples = vec![1.0; 512];

    ramp.ramp_to(&mut samples, 0.6);

    assert!(
        (ramp.current() - 0.6).abs() < 1e-6,
        "the ramp ends at the target, got {}",
        ramp.current()
    );
    assert!(
        (samples[511] - 0.6).abs() < 1e-5,
        "the last written sample already carries the target gain, got {}",
        samples[511]
    );
    // The fade is monotonic: every sample is louder than the one before it.
    assert!(samples.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn the_ramp_never_steps_more_than_a_small_amount_per_sample() {
    // The anti-click rule: a full silent-to-loud fade across one buffer moves
    // the gain a little per sample, never in a jump.
    let mut ramp = GainRamp::new(0.0);
    let mut samples = vec![1.0; 512];

    ramp.ramp_to(&mut samples, 1.0);

    let mut previous = 0.0_f32;
    for (index, gain) in samples.iter().enumerate() {
        assert!(
            (gain - previous).abs() <= MAX_STEP_PER_SAMPLE,
            "sample {index} moved the gain from {previous} to {gain}"
        );
        previous = *gain;
    }
}

#[test]
fn the_ramp_stays_put_once_it_arrives() {
    let mut ramp = GainRamp::new(0.0);
    let mut samples = vec![1.0; 256];
    ramp.ramp_to(&mut samples, 0.4);

    // A second buffer at the same target changes nothing: the arrived gain is
    // the state, so there is no drift and no second fade.
    let mut samples = vec![1.0; 256];
    ramp.ramp_to(&mut samples, 0.4);
    assert!((ramp.current() - 0.4).abs() < 1e-6);
    assert!(
        samples.iter().all(|gain| (gain - 0.4).abs() < 1e-5),
        "every sample carries exactly the arrived gain"
    );
}

#[test]
fn a_seek_offset_of_n_frames_leaves_total_minus_n_frames() {
    let spec = TrackSpec {
        sample_rate: 44_100,
        channels: 2,
    };
    let path = scratch_path("seek-length");
    let total_frames = 10_000;
    let written = ramp_frames(total_frames, spec.channels, 100);
    write_wav(&path, spec, &written);

    // The whole track drains to its total length.
    let mut whole = TrackSource::open(&path).expect("the track opens");
    let mut total = 0_usize;
    while let Some(block) = whole.read_block().expect("decoding works") {
        total += block.len() / usize::from(spec.channels);
    }

    // Seeking N frames into it leaves exactly the rest.
    let skipped = 4_000_u64;
    let mut source = TrackSource::open_at(&path, skipped).expect("the seeked track opens");
    let mut remaining = Vec::new();
    while let Some(block) = source.read_block().expect("decoding works") {
        remaining.extend_from_slice(&block);
    }

    let _ = std::fs::remove_file(&path);

    let remaining_frames = remaining.len() / usize::from(spec.channels);
    let expected = total - usize::try_from(skipped).expect("small offset");
    assert!(
        remaining_frames.abs_diff(expected) <= 4,
        "seeking {skipped} frames leaves {expected} frames, got {remaining_frames}"
    );

    // And the stream continues at the right place: no silence, no rewind —
    // the first remaining sample is the one written at frame `skipped`.
    let expected_samples = as_f32(
        &written[usize::try_from(skipped).expect("small offset") * usize::from(spec.channels)..],
    );
    assert!(
        remaining
            .iter()
            .zip(&expected_samples)
            .all(|(got, want)| (got - want).abs() < PCM16_TOLERANCE),
        "the seeked stream is sample-exact from the offset on"
    );
}

#[test]
fn an_offset_past_the_end_clamps_to_the_end_instead_of_failing() {
    let spec = TrackSpec {
        sample_rate: 44_100,
        channels: 2,
    };
    let path = scratch_path("seek-clamp");
    write_wav(&path, spec, &ramp_frames(1_000, spec.channels, 100));

    let mut source =
        TrackSource::open_at(&path, 1_000_000).expect("a past-end offset clamps, it never errors");

    let _ = std::fs::remove_file(&path);

    // The stream is the end of the track: nothing left to play, but the
    // source still reports its format for the device.
    assert_eq!(source.spec(), spec);
    assert!(
        source.read_block().expect("draining works").is_none(),
        "an offset clamped to the end leaves no audio"
    );
}

#[test]
fn the_seek_target_clamps_at_both_ends() {
    let track = Some(Duration::from_secs(30));

    // Seeking back past the start lands at the start.
    assert_eq!(
        seek_target(Duration::from_secs(10), -60, track),
        Duration::ZERO
    );
    // Seeking forward past the end lands at the end.
    assert_eq!(
        seek_target(Duration::from_secs(25), 60, track),
        Duration::from_secs(30)
    );
    // Inside the track, the delta is exact.
    assert_eq!(
        seek_target(Duration::from_secs(10), 5, track),
        Duration::from_secs(15)
    );
    assert_eq!(
        seek_target(Duration::from_secs(10), -5, track),
        Duration::from_secs(5)
    );
    // Without a known duration only the lower end is clamped; the player
    // clamps a past-end offset when it opens the track.
    assert_eq!(
        seek_target(Duration::from_secs(10), -5, None),
        Duration::from_secs(5)
    );
}

/// The offset a seek lands on is whole seconds (issue #70, R3-001): the
/// seek's chain starts at `--download-sections "*<sec>-"`, which takes
/// integer seconds, so the fraction is dropped once — here, where the offset
/// is taken — and the clock carries the same number the chain was asked for,
/// never a second the audio does not play.
#[test]
fn the_seek_target_is_whole_seconds_the_fraction_is_dropped_where_it_is_taken() {
    // A position carrying a fraction moves by whole seconds to a whole
    // second: 10.4 s + 5 lands at 15 s, the second the chain can start at.
    assert_eq!(
        seek_target(Duration::from_millis(10_400), 5, None),
        Duration::from_secs(15)
    );
    // Backwards the same: 10.4 s − 5 lands at 5 s.
    assert_eq!(
        seek_target(Duration::from_millis(10_400), -5, None),
        Duration::from_secs(5)
    );
    // Clamping to a duration with a fraction still lands on a whole second.
    assert_eq!(
        seek_target(
            Duration::from_secs(25),
            60,
            Some(Duration::from_millis(30_500))
        ),
        Duration::from_secs(30)
    );
}

/// A `MediaSource` over in-memory bytes: `seekable` says which, so one
/// fixture serves both halves of the carried-source rule — the ranged stream
/// a seek's carried ask hands over, with a position or without one.
struct InMemory {
    bytes: std::io::Cursor<Vec<u8>>,
    seekable: bool,
}

impl Read for InMemory {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.bytes.read(buf)
    }
}

impl Seek for InMemory {
    fn seek(&mut self, whence: SeekFrom) -> std::io::Result<u64> {
        if self.seekable {
            self.bytes.seek(whence)
        } else {
            Err(std::io::Error::other("a stream without a position"))
        }
    }
}

impl MediaSource for InMemory {
    fn is_seekable(&self) -> bool {
        self.seekable
    }

    fn byte_len(&self) -> Option<u64> {
        self.seekable
            .then(|| u64::try_from(self.bytes.get_ref().len()).expect("test bytes fit in a u64"))
    }
}

/// A resolver that streams the same in-memory bytes for every path: the
/// stand-in for a remote source whose opener did not position it — its
/// [`PathResolver::open_streaming_at`] is the trait's default, the shape a
/// future source without an override runs.
struct Streaming {
    bytes: Vec<u8>,
    seekable: bool,
}

impl PathResolver for Streaming {
    fn resolve(&self, path: &Path) -> Result<PathBuf, AudioError> {
        Ok(path.to_path_buf())
    }

    fn open_streaming(&self, _path: &Path) -> Result<Option<Box<dyn MediaSource>>, AudioError> {
        Ok(Some(Box::new(InMemory {
            bytes: std::io::Cursor::new(self.bytes.clone()),
            seekable: self.seekable,
        })))
    }
}

/// A seek over a stream the opener did not position and cannot seek is
/// refused with a sentence (issue #70, R3-002): the default
/// [`PathResolver::open_streaming_at`] opens, sees a source with no
/// position, and refuses instead of handing over a stream that would play
/// from its start while the clock said the offset. Offset zero is the plain
/// open, unchanged, and a source with a position takes the seek inside its
/// own bytes — the delegation that was always honest.
#[test]
fn the_default_open_streaming_at_refuses_the_seek_a_stream_without_a_position_cannot_honour() {
    let url = Path::new("https://wrap.example/live/song.mp3");
    let pipe = Streaming {
        bytes: b"not audio".to_vec(),
        seekable: false,
    };

    let error = pipe
        .open_streaming_at(url, Duration::from_secs(2))
        .err()
        .expect("a seek a source without a position cannot honour is refused");
    assert!(
        error.to_string().contains("cannot be seeked"),
        "the sentence names the reason: {error}"
    );

    // Offset zero asks nothing of the source: the plain open, unchanged.
    assert!(
        pipe.open_streaming_at(url, Duration::ZERO)
            .expect("zero is the plain open")
            .is_some(),
        "the open the default always answered"
    );

    // A source with a position takes the offset inside its own bytes:
    // the delegation that was always honest stays.
    let ranged = Streaming {
        bytes: b"not audio".to_vec(),
        seekable: true,
    };
    assert!(
        ranged
            .open_streaming_at(url, Duration::from_secs(2))
            .expect("a seekable source takes the offset")
            .is_some(),
        "the reader positions it inside its own bytes"
    );
}

/// The Subsonic-with-carried-source case (issue #70, R3-002): a carried
/// stream that can be seeked — the ranged answer a server's streaming open
/// hands over — takes the rebuild's offset inside its own bytes, so what
/// plays and what the clock says are the same second. Its non-seekable
/// counterpart never reaches the player: the default refuses it (the test
/// above), so the producer's zero-offset rule is left to the one source the
/// opener positioned — the seek's own chain.
#[test]
fn a_seekable_carried_source_takes_the_seek_offset_inside_its_own_bytes() {
    let spec = TrackSpec {
        sample_rate: 44_100,
        channels: 1,
    };
    let path = scratch_path("carried-seekable");
    let written = write_indexed_wav(&path, 3 * 44_100, spec);
    let bytes = std::fs::read(&path).expect("the wav bytes are read");
    let _ = std::fs::remove_file(&path);

    let source: Box<dyn MediaSource> = Box::new(InMemory {
        bytes: std::io::Cursor::new(bytes),
        seekable: true,
    });

    // The rebuild at one second in, the way a seek's handover arrives: the
    // carried source, the offset riding with it.
    let stop = AtomicBool::new(false);
    let mut samples = Vec::new();
    drain_headless_with_carried_source(
        Arc::new(Mutex::new(vec![Entry::File(PathBuf::from(
            "subsonic://server.example/42",
        ))])),
        0,
        44_100,
        spec,
        ReplayGainMode::default(),
        source,
        &stop,
        &mut |block: &[f32]| samples.extend_from_slice(block),
        &mut |_failure: TrackFailure| {},
    )
    .expect("the sequence runs");

    let expected = as_f32(&written[44_100..]);
    assert_eq!(
        samples.len(),
        expected.len(),
        "one second was not replayed behind the seek"
    );
    assert!(
        samples
            .iter()
            .zip(&expected)
            .all(|(got, want)| (got - want).abs() < PCM16_TOLERANCE),
        "the carried seekable stream is sample-exact from the offset on"
    );
}

/// A PCM16 WAV where every frame carries its own index as its sample value, so
/// a decoded frame can be identified exactly.
fn write_indexed_wav(path: &Path, frames: usize, spec: TrackSpec) -> Vec<i16> {
    let bits: u16 = 16;
    let block_align = spec.channels * bits / 8;
    let data_len = u32::try_from(frames * usize::from(spec.channels) * 2).expect("fits");
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&spec.channels.to_le_bytes());
    out.extend_from_slice(&spec.sample_rate.to_le_bytes());
    out.extend_from_slice(&(spec.sample_rate * u32::from(block_align)).to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    let mut written = Vec::with_capacity(frames * usize::from(spec.channels));
    for frame in 0..frames {
        let value = i16::try_from(frame % 3_000).expect("small ramp") - 1_500;
        for _ in 0..spec.channels {
            out.extend_from_slice(&value.to_le_bytes());
            written.push(value);
        }
    }
    std::fs::write(path, out).expect("write wav");
    written
}

fn sample_f32(sample: i16) -> f32 {
    f32::from(sample) / 32_768.0
}

#[test]
fn a_seek_starts_at_the_requested_frame_not_at_the_beginning() {
    let spec = TrackSpec {
        sample_rate: 44_100,
        channels: 2,
    };
    let path = std::env::temp_dir().join(format!("velo-seek-{}.wav", std::process::id()));
    let total = 4_410;
    let offset: u64 = 1_000;
    let offset_index = usize::try_from(offset).expect("fits");
    let written = write_indexed_wav(&path, total, spec);

    let mut source = TrackSource::open_at(&path, offset).expect("open at an offset");
    let mut decoded = Vec::new();
    while let Some(block) = source.read_block().expect("decodes") {
        decoded.extend(block);
    }
    let _ = std::fs::remove_file(&path);

    let channels = usize::from(spec.channels);
    let first_expected = sample_f32(written[offset_index * channels]);
    let first_got = decoded[0];
    assert!(
        (first_got - first_expected).abs() < 1e-5,
        "the first sample after the seek must be frame {offset}'s ({first_expected}), got {first_got}"
    );

    let expected_len = (total - offset_index) * channels;
    assert!(
        decoded.len().abs_diff(expected_len) <= channels * 4,
        "the tail must keep its length: expected {expected_len} samples, got {}",
        decoded.len()
    );
}

/// RMS of an interleaved mono stream.
fn rms(samples: &[f32]) -> f32 {
    let energy: f32 = samples.iter().map(|s| s * s).sum();
    #[expect(
        clippy::cast_precision_loss,
        reason = "a test window holds far fewer samples than f32 represents exactly"
    )]
    let count = samples.len() as f32;
    (energy / count).sqrt()
}

fn decode_all(mut source: TrackSource) -> Vec<f32> {
    let mut out = Vec::new();
    while let Some(block) = source.read_block().expect("decodes") {
        out.extend(block);
    }
    out
}

/// The library is MP3, so a seek that only works on WAV would pass every other
/// test in this file and still fail the user. This fixture is one second of
/// silence followed by one second of tone: after a seek past the halfway mark
/// there must be sound, and from the start there must not.
// ---- mid-queue skips (T19) ----------------------------------------------

#[test]
fn a_corrupt_track_mid_queue_is_skipped_without_silence_and_is_named() {
    use velo::audio::output::{TrackFailure, drain_headless};

    let spec = TrackSpec {
        sample_rate: 44_100,
        channels: 2,
    };
    let good_a = scratch_path("skip-a");
    let broken = scratch_path("skip-broken");
    let good_b = scratch_path("skip-c");

    write_indexed_wav(&good_a, 1_000, spec);
    // A file with an audio extension whose bytes are not a container: what a
    // truncated download or a corrupted write looks like to the decoder.
    std::fs::write(&broken, b"definitely not an audio container").expect("write broken file");
    write_indexed_wav(&good_b, 900, spec);

    let mut samples: Vec<f32> = Vec::new();
    let mut failures: Vec<TrackFailure> = Vec::new();
    let stop = AtomicBool::new(false);
    let result = drain_headless(
        std::sync::Arc::new(std::sync::Mutex::new(vec![
            Entry::File(good_a.clone()),
            Entry::File(broken.clone()),
            Entry::File(good_b.clone()),
        ])),
        0,
        0,
        spec,
        &stop,
        &mut |block: &[f32]| samples.extend_from_slice(block),
        &mut |failure: TrackFailure| failures.push(failure),
    );
    assert!(
        result.is_ok(),
        "a skipped track is not a sequence error: {result:?}"
    );

    // The queue is exactly the two good tracks back to back: no silence
    // stands where the broken file was, and nothing else moved.
    let mut expected: Vec<f32> = Vec::new();
    for path in [&good_a, &good_b] {
        let mut source = TrackSource::open(path).expect("the good tracks open");
        while let Some(block) = source.read_block().expect("decodes") {
            expected.extend_from_slice(&block);
        }
    }
    assert_eq!(
        samples, expected,
        "the drained stream must be the exact concatenation of the good tracks"
    );

    // And the failure announces the file that broke, by the name the
    // listener sees.
    assert_eq!(failures.len(), 1, "exactly one track failed: {failures:?}");
    assert_eq!(
        failures[0].label,
        broken.file_name().unwrap().to_string_lossy().into_owned()
    );
    assert!(!failures[0].message.is_empty(), "the failure says why");

    let _ = std::fs::remove_file(&good_a);
    let _ = std::fs::remove_file(&broken);
    let _ = std::fs::remove_file(&good_b);
}

#[test]
fn a_queue_whose_only_track_is_broken_fails_instead_of_finishing_silently() {
    use velo::audio::output::{TrackFailure, drain_headless};

    let spec = TrackSpec {
        sample_rate: 44_100,
        channels: 2,
    };
    let broken = scratch_path("skip-only");
    std::fs::write(&broken, b"definitely not an audio container").expect("write broken file");

    let mut samples: Vec<f32> = Vec::new();
    let mut failures: Vec<TrackFailure> = Vec::new();
    let stop = AtomicBool::new(false);
    let result = drain_headless(
        std::sync::Arc::new(std::sync::Mutex::new(vec![Entry::File(broken.clone())])),
        0,
        0,
        spec,
        &stop,
        &mut |block: &[f32]| samples.extend_from_slice(block),
        &mut |failure: TrackFailure| failures.push(failure),
    );

    let _ = std::fs::remove_file(&broken);

    // The requested track's failure is returned, not swallowed into an empty
    // success: this is the error the interface reports at selection time.
    assert!(
        result.is_err(),
        "the only track is broken, the drain must fail: {result:?}"
    );
    assert!(samples.is_empty(), "no sample can come from a broken file");
    assert!(
        failures.is_empty(),
        "the requested track's failure is returned, not announced as a skip"
    );
}

#[test]
fn an_mp3_seek_lands_past_the_silence() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/silence-then-tone.mp3");

    let from_start = decode_all(TrackSource::open(&path).expect("opens"));
    let start_rms = rms(&from_start[..from_start.len() / 4]);

    let halfway = 66_150; // 1.5 s at 44.1 kHz, inside the tone
    let after_seek = decode_all(TrackSource::open_at(&path, halfway).expect("opens at offset"));
    // An MP3 seek is frame-approximate: an MPEG frame is ~26 ms and the decoder
    // needs its own warm-up, so the landing can be up to ~0.1 s early. Measure
    // past that, which is the guarantee the format actually offers.
    let warm_up = 8_820; // 0.2 s
    let window = 4_410; // 0.1 s
    let seek_rms = rms(&after_seek[warm_up..warm_up + window]);

    assert!(
        start_rms < 0.01,
        "the fixture starts with silence, measured {start_rms}"
    );
    // The fixture's tone measures ~0.084 RMS (a 32 kbps mono encode of a pure
    // sine), so the rule is "the tone, not silence", not a level guess.
    assert!(
        seek_rms > 0.05 && seek_rms > start_rms * 10.0,
        "a seek past the silence must land on the tone: silence {start_rms}, after seek {seek_rms}"
    );
}

#[test]
fn a_track_appended_while_it_plays_is_played_next() {
    // The queue is shared, so an append made in the middle of the first track
    // is what plays after it. This is the test that makes T18 a fact: if the
    // producer took a snapshot, the second track would never be heard.
    let spec = TrackSpec {
        sample_rate: 44_100,
        channels: 2,
    };
    let first = scratch_path("append-first");
    let second = scratch_path("append-second");
    write_indexed_wav(&first, 500, spec);
    write_indexed_wav(&second, 700, spec);

    let queue = std::sync::Arc::new(std::sync::Mutex::new(vec![Entry::File(first)]));
    let for_append = std::sync::Arc::clone(&queue);
    let stop = AtomicBool::new(false);
    let mut samples: Vec<f32> = Vec::new();
    let mut failures: Vec<TrackFailure> = Vec::new();
    let mut appended = false;
    let mut on_block = |block: &[f32]| {
        samples.extend_from_slice(block);
        if !appended {
            appended = true;
            for_append
                .lock()
                .expect("queue")
                .push(Entry::File(second.clone()));
        }
    };

    drain_headless(
        queue,
        0,
        0,
        spec,
        &stop,
        &mut on_block,
        &mut |failure: TrackFailure| failures.push(failure),
    )
    .expect("the sequence runs");

    assert!(failures.is_empty(), "nothing failed: {failures:?}");
    assert_eq!(samples.len(), (500 + 700) * 2, "both tracks were heard");
}

#[test]
fn a_queue_that_shrinks_to_nothing_ends_cleanly() {
    let spec = TrackSpec {
        sample_rate: 44_100,
        channels: 2,
    };
    let first = scratch_path("shrink-first");
    write_indexed_wav(&first, 400, spec);

    let queue = std::sync::Arc::new(std::sync::Mutex::new(vec![Entry::File(first)]));
    let for_clear = std::sync::Arc::clone(&queue);
    let stop = AtomicBool::new(false);
    let mut samples: Vec<f32> = Vec::new();
    let mut cleared = false;
    let mut on_block = |block: &[f32]| {
        samples.extend_from_slice(block);
        if !cleared {
            cleared = true;
            for_clear.lock().expect("queue").clear();
        }
    };

    drain_headless(
        queue,
        0,
        0,
        spec,
        &stop,
        &mut on_block,
        &mut |_failure: TrackFailure| panic!("nothing failed here"),
    )
    .expect("an emptied queue is an ending, not an error");

    // The track that was already playing finishes; there is nothing after it.
    assert_eq!(samples.len(), 400 * 2);
}

// ---- next and previous (S8) -------------------------------------------------

use velo::audio::skip_target;

#[test]
fn next_moves_one_track_on_and_refuses_past_the_last() {
    let at = Duration::from_secs(40);
    assert_eq!(skip_target(0, 3, at, true), Some(1));
    assert_eq!(skip_target(1, 3, at, true), Some(2));
    assert_eq!(skip_target(2, 3, at, true), None, "nothing after the last");
}

#[test]
fn previous_restarts_a_track_already_under_way_and_otherwise_goes_back() {
    // Three seconds in or more: back to the start of this track, the way
    // every player's previous button works.
    assert_eq!(skip_target(2, 3, Duration::from_secs(3), false), Some(2));
    assert_eq!(skip_target(2, 3, Duration::from_secs(90), false), Some(2));
    // Just started: the track before it.
    assert_eq!(
        skip_target(2, 3, Duration::from_millis(2_999), false),
        Some(1)
    );
    // The first track has nothing before it, so it starts over.
    assert_eq!(skip_target(0, 3, Duration::ZERO, false), Some(0));
}

// ---- repeat (S8) -------------------------------------------------------------

use velo::audio::output::{
    Repeat, RepeatCell, drain_headless_repeating, drain_headless_with_removals, next_track,
};

#[test]
fn the_next_track_follows_the_repeat_mode() {
    assert_eq!(next_track(0, 3, Repeat::Off), Some(1));
    assert_eq!(
        next_track(2, 3, Repeat::Off),
        None,
        "off ends after the last"
    );
    assert_eq!(next_track(2, 3, Repeat::All), Some(0), "all wraps around");
    assert_eq!(next_track(1, 3, Repeat::One), Some(1), "one plays it again");
    assert_eq!(
        next_track(0, 0, Repeat::All),
        None,
        "an empty queue has nothing"
    );
    assert_eq!(Repeat::Off.next(), Repeat::All);
    assert_eq!(Repeat::All.next(), Repeat::One);
    assert_eq!(Repeat::One.next(), Repeat::Off);
}

/// Two short tracks with different lengths, so whatever plays third tells
/// which rule picked it.
fn two_tracks(tag: &str) -> (TrackSpec, PathBuf, PathBuf) {
    let spec = TrackSpec {
        sample_rate: 44_100,
        channels: 2,
    };
    let first = scratch_path(&format!("{tag}-first"));
    let second = scratch_path(&format!("{tag}-second"));
    write_indexed_wav(&first, 500, spec);
    write_indexed_wav(&second, 700, spec);
    (spec, first, second)
}

/// Drains `queue` under `repeat` until `enough` samples were heard.
fn drain_until(
    queue: Vec<PathBuf>,
    offset_frames: u64,
    spec: TrackSpec,
    repeat: &RepeatCell,
    enough: usize,
    mut also: impl FnMut(usize),
) -> (Vec<f32>, Vec<TrackFailure>) {
    let queue = std::sync::Arc::new(std::sync::Mutex::new(
        queue.into_iter().map(Entry::from).collect(),
    ));
    let stop = AtomicBool::new(false);
    let mut samples: Vec<f32> = Vec::new();
    let mut failures: Vec<TrackFailure> = Vec::new();
    drain_headless_repeating(
        queue,
        0,
        offset_frames,
        spec,
        repeat,
        &stop,
        &mut |block: &[f32]| {
            samples.extend_from_slice(block);
            also(samples.len());
            if samples.len() >= enough {
                stop.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        },
        &mut |failure: TrackFailure| failures.push(failure),
    )
    .expect("the sequence runs");
    (samples, failures)
}

/// Removing a played entry shifts the producer's own position with the
/// queue: the track that plays keeps playing and the entry that followed it
/// is the one that plays next — never a lost entry, never a repeat. The
/// queue here is `[played, playing, next]`, started at the middle track,
/// and the removal lands while that track is being produced, the way an
/// `x` on the queue view lands while it sounds.
#[test]
fn removing_a_played_entry_shifts_the_producer_onto_the_entry_that_followed() {
    let spec = TrackSpec {
        sample_rate: 44_100,
        channels: 2,
    };
    let played = scratch_path("shift-played");
    let playing = scratch_path("shift-playing");
    let next = scratch_path("shift-next");
    write_indexed_wav(&played, 500, spec); // 1_000 samples
    write_indexed_wav(&playing, 700, spec); // 1_400 samples
    write_indexed_wav(&next, 900, spec); // 1_800 samples

    let queue: SharedQueue = Arc::new(Mutex::new(
        vec![played, playing, next]
            .into_iter()
            .map(Entry::from)
            .collect(),
    ));
    let removals = Mutex::new(VecDeque::new());
    let stop = AtomicBool::new(false);
    let mut samples: Vec<f32> = Vec::new();
    let mut failures: Vec<TrackFailure> = Vec::new();
    let mut removed = false;
    drain_headless_with_removals(
        Arc::clone(&queue),
        1, // the middle track plays; the first one already did
        0,
        spec,
        &RepeatCell::new(Repeat::Off),
        &removals,
        &stop,
        &mut |block: &[f32]| {
            samples.extend_from_slice(block);
            if !removed {
                removed = true;
                // The edit and its record go out together, in the order the
                // UI publishes them (queue, then removals): two separate
                // acquisitions let the producer cross a turn boundary in
                // between and read a shrunk queue against a stale index, which
                // makes this test flaky instead of proving the shift.
                let mut queued = queue.lock().expect("the queue locks");
                queued.remove(0);
                removals.lock().expect("the removals lock").push_back(0);
                drop(queued);
            }
        },
        &mut |failure: TrackFailure| failures.push(failure),
    )
    .expect("the sequence runs");

    let _ = std::fs::remove_file(scratch_path("shift-played"));
    let _ = std::fs::remove_file(scratch_path("shift-playing"));
    let _ = std::fs::remove_file(scratch_path("shift-next"));

    assert!(failures.is_empty(), "{failures:?}");
    // The middle track's own 1_400 samples, then the third track's 1_800:
    // a lost next entry drains 1_400, a repeated playing entry 4_600.
    assert_eq!(
        samples.len(),
        1_400 + 1_800,
        "the track that plays keeps playing and the entry that followed it plays next"
    );
}

#[test]
fn repeat_all_plays_the_queue_again_from_its_first_track() {
    let (spec, first, second) = two_tracks("repeat-all");
    let repeat = RepeatCell::new(Repeat::All);
    let (samples, failures) = drain_until(vec![first, second], 0, spec, &repeat, 3_400, |_| ());

    assert!(failures.is_empty(), "{failures:?}");
    assert_eq!(
        samples[2_400..3_400],
        samples[..1_000],
        "after both tracks the first one plays again"
    );
}

#[test]
fn repeat_one_plays_the_same_track_again() {
    let (spec, first, second) = two_tracks("repeat-one");
    let repeat = RepeatCell::new(Repeat::One);
    let (samples, _) = drain_until(vec![first, second], 0, spec, &repeat, 2_000, |_| ());

    assert_eq!(
        samples[1_000..2_000],
        samples[..1_000],
        "the first track twice"
    );
}

#[test]
fn turning_repeat_off_while_it_plays_lets_the_queue_end() {
    let (spec, first, second) = two_tracks("repeat-switch");
    let repeat = RepeatCell::new(Repeat::One);
    // Switched off during the first track: the producer reads the mode at the
    // next boundary, so the second track follows and the queue ends there.
    let (samples, _) = drain_until(vec![first, second], 0, spec, &repeat, usize::MAX, |_| {
        repeat.set(Repeat::Off);
    });

    assert_eq!(samples.len(), (500 + 700) * 2);
}

#[test]
fn a_seek_offset_applies_to_the_first_turn_only() {
    let (spec, first, _) = two_tracks("repeat-seek");
    let repeat = RepeatCell::new(Repeat::One);
    // 200 frames into the track, then the same track from its beginning.
    let (samples, _) = drain_until(vec![first.clone()], 200, spec, &repeat, 1_600, |_| ());
    let whole = as_f32(&write_indexed_wav(&first, 500, spec));

    assert_eq!(
        samples[..600],
        whole[400..1_000],
        "the first turn starts at 200"
    );
    assert_eq!(
        samples[600..1_600],
        whole[..1_000],
        "the second starts at 0"
    );
}

#[test]
fn a_repeating_queue_whose_every_track_fails_stops_instead_of_spinning() {
    let (spec, first, _) = two_tracks("repeat-fail");
    let gone_a = scratch_path("repeat-fail-gone-a");
    let gone_b = scratch_path("repeat-fail-gone-b");
    let repeat = RepeatCell::new(Repeat::All);
    let to_delete = first.clone();
    // The first track opens and plays, then its file disappears; on the next
    // pass all three fail in a row and the producer gives up.
    let (_, failures) = drain_until(
        vec![first, gone_a, gone_b],
        0,
        spec,
        &repeat,
        usize::MAX,
        |_| {
            let _ = std::fs::remove_file(&to_delete);
        },
    );

    assert_eq!(failures.len(), 3, "{failures:?}");
}

#[test]
fn a_track_with_no_audio_is_not_repeated_forever() {
    // A header-only WAV opens and decodes nothing. Repeating it would reopen
    // it as fast as the disk allows with nothing ever reaching the ring to
    // slow the loop down; it counts as a turn that failed instead.
    let spec = TrackSpec {
        sample_rate: 44_100,
        channels: 2,
    };
    let empty = scratch_path("repeat-empty");
    write_indexed_wav(&empty, 0, spec);
    let queue = std::sync::Arc::new(std::sync::Mutex::new(vec![Entry::File(empty)]));
    let repeat = RepeatCell::new(Repeat::One);
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let guard = std::sync::Arc::clone(&stop);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        guard.store(true, std::sync::atomic::Ordering::Relaxed);
    });
    let mut turns = 0;

    drain_headless_repeating(
        queue,
        0,
        0,
        spec,
        &repeat,
        &stop,
        &mut |_: &[f32]| turns += 1,
        &mut |_: TrackFailure| {},
    )
    .expect("the sequence runs");

    assert!(turns <= 1, "the empty track was replayed {turns} times");
}
