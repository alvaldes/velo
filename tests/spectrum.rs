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
