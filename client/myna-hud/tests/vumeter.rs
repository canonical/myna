// tests/vumeter.rs — hermetic contract test for the pure envelope logic
// (feature 004, contract extension.md RC5, SC-004), reused unchanged by
// ribbon.rs (2026-07-30 wave-ribbon redesign). No Shell, no D-Bus.

use myna_hud::vumeter::{
    intensity_to_active_segments, levels_to_intensity, segment_color, SegmentColor, STALE_MS,
};

#[test]
fn x5_louder_means_higher_in_the_speech_range() {
    assert!(
        levels_to_intensity(0.002, 0.002, 0.0) < levels_to_intensity(0.02, 0.02, 0.0),
        "monotonic in the speech range"
    );
}

#[test]
fn x5_clamps_above_one() {
    assert!(levels_to_intensity(5.0, 5.0, 0.0) <= 1.0);
}

#[test]
fn x5_clamps_below_zero() {
    assert!(levels_to_intensity(-3.0, -3.0, 0.0) >= 0.0);
}

#[test]
fn x5_nan_is_safe() {
    assert!(levels_to_intensity(f64::NAN, f64::NAN, 0.0) >= 0.0);
}

// Hardware calibration (Blackwire C5220, 2026-07-30): ordinary speech was
// RMS≈0.009 / peak≈0.025 (median), while room/device noise was
// RMS≈0.00003 / peak≈0.0001. Normal speech must occupy a clearly visible
// middle section of the meter instead of hugging its floor.
#[test]
fn blackwire_calibration_zones() {
    let noise = levels_to_intensity(0.00003, 0.0001, 0.0);
    let normal = levels_to_intensity(0.009, 0.025, 0.0);
    let strong = levels_to_intensity(0.024, 0.067, 0.0);
    let overload = levels_to_intensity(0.05, 0.16, 0.0);
    assert!(noise < 0.12, "noise stays near meter floor: {noise}");
    assert!(
        (0.45..=0.7).contains(&normal),
        "normal speech clearly moves meter: {normal}"
    );
    assert!(
        strong >= 0.65,
        "strong speech reaches yellow zone: {strong}"
    );
    assert!(
        overload >= 0.85,
        "near-overload reaches red zone: {overload}"
    );
    assert!(
        noise < normal && normal < strong && strong < overload,
        "combined level remains monotonic"
    );
}

// Fresh loud vs stale loud: stale decays to (near) the floor.
#[test]
fn x5_decays_across_the_stale_window() {
    let fresh = levels_to_intensity(0.9, 0.9, 0.0);
    let half_stale = levels_to_intensity(0.9, 0.9, STALE_MS / 2.0);
    let stale = levels_to_intensity(0.9, 0.9, STALE_MS + 50.0);
    assert!(fresh > half_stale && half_stale > stale);
    assert!(
        stale <= levels_to_intensity(0.0, 0.0, 0.0) + 1e-9,
        "stale reaches the floor"
    );
}

// Conventional loudness zones (still true of the underlying envelope, even
// though the wave ribbon no longer renders discrete colour zones).
#[test]
fn quiet_input_stays_near_the_floor() {
    assert!(levels_to_intensity(0.00003, 0.0001, 0.0) < 0.12);
}

#[test]
fn normal_speech_clearly_moves_the_envelope() {
    let normal = levels_to_intensity(0.009, 0.025, 0.0);
    assert!((0.45..=0.7).contains(&normal), "normal = {normal}");
}

// X6: no content in outputs (numbers only) — type-level in Rust; the test
// pins that the output is a plain finite f64 with no side channel.
#[test]
fn x6_outputs_are_numbers_only() {
    let out = levels_to_intensity(0.5, 0.5, 0.0);
    assert!(out.is_finite());
}

// ── Segmented bar meter (classic vumeter HUD style) ─────────────────────────

#[test]
fn segment_count_scales_with_intensity() {
    // Zero intensity lights nothing; full intensity lights every segment.
    assert_eq!(intensity_to_active_segments(0.0, 24), 0);
    assert_eq!(intensity_to_active_segments(1.0, 24), 24);
    // Mid range: roughly intensity × count (ceil, so it never reads as "off").
    assert_eq!(intensity_to_active_segments(0.5, 24), 12);
    // Clamped: out-of-range intensities can never over/under-light.
    assert_eq!(intensity_to_active_segments(5.0, 24), 24);
    assert_eq!(intensity_to_active_segments(-3.0, 24), 0);
    assert_eq!(intensity_to_active_segments(f64::NAN, 24), 0);
}

#[test]
fn zero_segment_count_is_treated_as_one() {
    assert_eq!(intensity_to_active_segments(1.0, 0), 1);
}

#[test]
fn zones_follow_the_conventional_vu_colours() {
    assert!(matches!(segment_color(0.3), SegmentColor::Green));
    assert!(matches!(segment_color(0.6), SegmentColor::Green));
    assert!(matches!(segment_color(0.68), SegmentColor::Yellow));
    assert!(matches!(segment_color(0.75), SegmentColor::Yellow));
    assert!(matches!(segment_color(0.86), SegmentColor::Red));
    assert!(matches!(segment_color(1.0), SegmentColor::Red));
}

/// The calibration test's "strong speech" reaches the yellow zone on a 24-seg
/// meter — the red zone is reserved for near-overload.
#[test]
fn calibrated_levels_land_in_the_expected_zones() {
    let normal = levels_to_intensity(0.009, 0.025, 0.0);
    let strong = levels_to_intensity(0.024, 0.067, 0.0);
    let overload = levels_to_intensity(0.05, 0.16, 0.0);
    assert!(
        matches!(segment_color(normal), SegmentColor::Green),
        "normal speech should sit in the green zone, got {normal}"
    );
    assert!(
        matches!(segment_color(strong), SegmentColor::Yellow),
        "strong speech should reach yellow, got {strong}"
    );
    assert!(
        matches!(segment_color(overload), SegmentColor::Red),
        "near-overload should reach red, got {overload}"
    );
}
