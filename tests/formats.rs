//! Format coverage for the decoder (S5).
//!
//! Containers differ in how much they reveal before the first packet is
//! decoded. An M4A/AAC track, for instance, does not declare its channel layout
//! in the container: it only appears once the decoder has parsed the audio
//! specific config. Opening a track must still report a usable spec, because the
//! device is configured before the first sample is played.

use std::path::PathBuf;

use velo::audio::TrackSource;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn an_aac_track_reports_its_layout_at_open() {
    let path = fixture("tiny.m4a");

    let source = TrackSource::open(&path).expect("an M4A/AAC file must open");
    let spec = source.spec();

    assert_eq!(
        spec.sample_rate, 44_100,
        "the sample rate comes from the file"
    );
    assert_eq!(
        spec.channels, 2,
        "the channel layout must be known before a device is configured"
    );
}

#[test]
fn an_aac_track_decodes_every_frame_it_reports() {
    let path = fixture("tiny.m4a");

    let mut source = TrackSource::open(&path).expect("an M4A/AAC file must open");
    let spec = source.spec();
    let mut frames = 0;

    while let Some(block) = source.read_block().expect("decoding must not fail") {
        assert_eq!(
            block.len() % usize::from(spec.channels),
            0,
            "every block holds whole frames"
        );
        assert!(!block.is_empty(), "a returned block is never empty");
        frames += block.len() / usize::from(spec.channels);
    }

    // 0.3s in exact integer arithmetic: no float rounding to argue about.
    let expected = spec.sample_rate as usize * 3 / 10;
    // symphonia 0.5 does not trim the AAC encoder priming for isomp4 (tracked as
    // T16), so a short file decodes slightly long. What this test pins is that
    // every frame decodes exactly once and that no block splits a frame.
    let tolerance = spec.sample_rate as usize / 10;
    assert!(
        frames.abs_diff(expected) <= tolerance,
        "0.3s of audio: expected about {expected} frames, decoded {frames}"
    );
}
