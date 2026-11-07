//! The spectrum (ui-redesign T5, S2): the tap the device callback feeds and
//! the analysis the interface draws.
//!
//! The callback itself only runs with a sound device, so its one new duty —
//! handing each frame's mono mix to a lock-free ring — is a pure function the
//! callback calls, tested here with a real `rtrb` ring.

use rtrb::RingBuffer;
use velo::audio::output::tap_mono;

#[test]
fn the_tap_hands_on_one_mono_sample_per_frame() {
    let (mut producer, mut consumer) = RingBuffer::<f32>::new(8);
    let stereo = [1.0, 0.0, 0.5, 0.5, -1.0, -1.0];

    tap_mono(&stereo, 2, &mut producer);

    let mut heard = Vec::new();
    while let Ok(sample) = consumer.pop() {
        heard.push(sample);
    }
    assert_eq!(heard, [0.5, 0.5, -1.0]);
}

#[test]
fn a_full_tap_drops_what_does_not_fit_instead_of_waiting() {
    let (mut producer, mut consumer) = RingBuffer::<f32>::new(2);

    // Three frames into a ring with room for two: the call returns, and the
    // first two frames are what the reader finds. The callback cannot wait
    // for a reader that is drawing a frame.
    tap_mono(&[0.1, 0.2, 0.3], 1, &mut producer);

    assert_eq!(consumer.pop(), Ok(0.1));
    assert_eq!(consumer.pop(), Ok(0.2));
    assert!(consumer.pop().is_err());
}

#[test]
fn a_partial_frame_at_the_end_of_a_buffer_is_not_tapped() {
    let (mut producer, mut consumer) = RingBuffer::<f32>::new(8);

    // An underrun can leave an odd number of samples copied; half a frame
    // is not a frame.
    tap_mono(&[0.2, 0.4, 0.9], 2, &mut producer);

    assert!((consumer.pop().unwrap() - 0.3).abs() < 1e-6);
    assert!(consumer.pop().is_err());
}

// ---- the analysis (T5b) ----------------------------------------------------

use velo::audio::spectrum::{Spectrum, WINDOW, band_range};

const RATE: u32 = 48_000;

#[expect(clippy::cast_precision_loss, reason = "test signal indices are small")]
fn sine(frequency: f32, amplitude: f32, samples: usize) -> Vec<f32> {
    (0..samples)
        .map(|n| {
            amplitude * (2.0 * std::f32::consts::PI * frequency * n as f32 / RATE as f32).sin()
        })
        .collect()
}

fn loudest(levels: &[f32]) -> usize {
    levels
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map(|(index, _)| index)
        .expect("some bands")
}

fn band_of(frequency: f32, bands: usize) -> usize {
    (0..bands)
        .find(|&band| {
            let (low, high) = band_range(band, bands, RATE);
            (low..high).contains(&frequency)
        })
        .expect("a band holds the frequency")
}

#[test]
fn a_tone_lights_the_band_that_holds_its_frequency_and_no_far_one() {
    let mut spectrum = Spectrum::new();
    spectrum.push(&sine(1_000.0, 0.5, WINDOW * 2));
    spectrum.update(32, RATE);
    let levels = spectrum.levels();

    assert_eq!(levels.len(), 32);
    assert_eq!(loudest(levels), band_of(1_000.0, 32));
    assert!(levels[band_of(1_000.0, 32)] > 0.8, "got {levels:?}");
    assert!(
        levels[band_of(100.0, 32)] < 0.05 && levels[band_of(8_000.0, 32)] < 0.05,
        "a 1 kHz tone does not light 100 Hz or 8 kHz, got {levels:?}"
    );
    assert!(levels.iter().all(|level| (0.0..=1.0).contains(level)));
}

#[test]
fn silence_draws_no_bars() {
    let mut spectrum = Spectrum::new();
    spectrum.push(&vec![0.0; WINDOW]);
    spectrum.update(24, RATE);

    assert!(spectrum.levels().iter().all(|&level| level == 0.0));
}

#[test]
fn bars_rise_at_once_and_fall_smoothly_when_the_music_stops() {
    let mut spectrum = Spectrum::new();
    spectrum.push(&sine(1_000.0, 0.5, WINDOW));
    spectrum.update(32, RATE);
    let band = band_of(1_000.0, 32);
    let peak = spectrum.levels()[band];
    assert!(peak > 0.8, "one update is enough to rise, got {peak}");

    // Nothing new arrives — a pause, or the end of the queue.
    spectrum.update(32, RATE);
    let after_one = spectrum.levels()[band];
    assert!(
        after_one > 0.0 && after_one < peak,
        "one frame later the bar is falling, not gone: {after_one}"
    );

    for _ in 0..60 {
        spectrum.update(32, RATE);
    }
    assert!(
        spectrum.levels().iter().all(|&level| level == 0.0),
        "two seconds of nothing leave no bar standing"
    );
}

#[test]
fn the_bands_climb_from_the_lowest_to_the_highest_frequency_without_gaps() {
    let bands = 40;
    let (first_low, _) = band_range(0, bands, RATE);
    let (_, last_high) = band_range(bands - 1, bands, RATE);
    assert!(first_low >= 20.0 && last_high <= 24_000.0);
    for band in 1..bands {
        assert_eq!(
            band_range(band, bands, RATE).0,
            band_range(band - 1, bands, RATE).1
        );
    }
}
