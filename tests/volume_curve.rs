//! The volume knob's perceptual curve (issue #39): the amplitude the samples
//! carry is `(percent / 100)³`, not `percent / 100`. Hearing is roughly
//! logarithmic in amplitude, so the linear knob squeezed everything audible
//! into its last few points (100→90 was about one decibel, 10→0 was twenty).
//!
//! The curve itself is pinned against the table the decision records; the
//! ramp is pinned through the same curve targets the callback uses; and
//! bit-perfect output stays untouched by the curve — its volume is pinned to
//! 100%, where the amplitude is exactly 1.0 and the samples pass through.

use velo::audio::output::{GainRamp, volume_amplitude};

#[test]
fn the_curve_matches_the_table_the_decision_recorded() {
    // From issue #39: shown percent → amplitude. Small tolerance: the table
    // is rounded to three decimals, the function is not.
    let table = [
        (100, 1.000),
        (75, 0.422),
        (50, 0.125),
        (25, 0.016),
        (10, 0.001),
        (0, 0.000),
    ];
    for (percent, amplitude) in table {
        let got = volume_amplitude(percent);
        assert!(
            (got - amplitude).abs() < 1e-3,
            "{percent}% must sound as {amplitude}, got {got}"
        );
    }
}

#[test]
fn the_curve_is_monotonic_across_the_whole_knob() {
    let mut previous = volume_amplitude(0);
    for percent in 0..=100 {
        let amplitude = volume_amplitude(percent);
        assert!(
            amplitude >= previous,
            "the curve must never fall: {percent}% below {}%",
            percent - 1
        );
        previous = amplitude;
    }
}

#[test]
fn zero_is_exactly_silence_and_100_is_exactly_unity() {
    assert_eq!(volume_amplitude(0), 0.0, "0% must be exact silence");
    assert_eq!(volume_amplitude(100), 1.0, "100% must be exact unity");
}

#[test]
fn a_volume_change_ramps_across_the_buffer_instead_of_jumping() {
    // The callback's shape: a ramp sitting at one curve target is pointed at
    // another and must fade across the buffer, arriving at the new target at
    // the last sample — never stepping straight to it.
    let start = volume_amplitude(80);
    let target = volume_amplitude(60);
    let mut buffer = vec![1.0_f32; 480];
    let mut ramp = GainRamp::new(start);
    ramp.ramp_to(&mut buffer, target);

    let low = start.min(target);
    let high = start.max(target);
    assert!(
        buffer[0] > low && buffer[0] < high,
        "the first sample moves, but does not land on the target: {}",
        buffer[0]
    );
    for sample in &buffer[..479] {
        assert!(
            *sample > low && *sample < high,
            "every sample before the last sits between the two targets: {sample}"
        );
    }
    assert!(
        (buffer[479] - target).abs() < 1e-4,
        "the ramp arrives at the target at the last sample: {}",
        buffer[479]
    );
}

#[test]
fn a_buffer_at_the_arrived_target_is_unchanged() {
    // The ramp's own old rule, unchanged by the curve: once arrived, writing
    // more samples at the same target drifts nothing and fades nothing.
    let mut ramp = GainRamp::new(0.0);
    let mut buffer = vec![1.0_f32; 480];
    ramp.ramp_to(&mut buffer, 0.5);
    assert!(
        (buffer[479] - 0.5).abs() < 1e-4,
        "the first fade arrives at its target: {}",
        buffer[479]
    );
    ramp.ramp_to(&mut buffer, 0.5);
    // The next callback fills a fresh buffer; at the arrived target every
    // sample of it carries exactly the target gain — no second fade, no drift.
    let mut buffer = vec![1.0_f32; 480];
    ramp.ramp_to(&mut buffer, 0.5);
    assert!(
        buffer.iter().all(|sample| (*sample - 0.5).abs() < 1e-6),
        "a second buffer at the same target is a no-op"
    );
}
