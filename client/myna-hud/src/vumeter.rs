//! vumeter — PURE envelope logic (feature 004; contract extension.md X5;
//! research R5/R16/R16a/R17). RMS/peak → a headset-calibrated dBFS intensity
//! with stale-decay. This is the shared envelope math [`crate::ribbon`]
//! (the 2026-07-30 wave-ribbon redesign) delegates to unchanged — the
//! segmented bar-meter-only helpers (`intensity_to_active_segments`/
//! `segment_color`) were removed once the ribbon fully replaced their only
//! caller.
//!
//! No GTK imports; carries energy only, never samples or content
//! (constitution V, X6).

/// Past this age with no fresh level, ease the VU to its floor rather than
/// freezing on the last value (R5/SC-004).
pub const STALE_MS: f64 = 300.0;

/// Never fully dead while active, so the VU reads as "alive, quiet" not "off".
pub const FLOOR: f64 = 0.04;

// Calibrated from a normal-speech Blackwire C5220 capture (2026-07-30):
// noise ≈ -80 dBFS, normal speech RMS ≈ -41 dBFS / peak ≈ -32 dBFS,
// strong speech RMS ≈ -32 dBFS / peak ≈ -23 dBFS. Map that useful acoustic
// range onto the full meter instead of applying a shallow linear gain.
// Public: the simulator inverts this exact window so its slider drives the
// HUD 1:1 (a deliberate transcription that catches calibration drift).
pub const DB_FLOOR: f64 = -67.0;
pub const DB_CEILING: f64 = -14.0;
const PEAK_WEIGHT: f64 = 0.55;

/// Clamp to `[0,1]`; NaN collapses to 0 (X5's NaN-safety).
fn clamp01(x: f64) -> f64 {
    if x.is_nan() {
        return 0.0;
    }
    x.clamp(0.0, 1.0)
}

/// Perceptual lift of a raw `[0,1]` level: gain + power curve, clamped to
/// `[0,1]`. Monotonic non-decreasing, so it never breaks the
/// "louder → higher" contract.
///
pub fn boost_level(level: f64) -> f64 {
    let l = clamp01(level);
    if l <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * l.log10();
    clamp01((db - DB_FLOOR) / (DB_CEILING - DB_FLOOR))
}

/// RMS + peak → VU intensity in `[FLOOR, 1]`, monotonic and clamped, decaying
/// to [`FLOOR`] once the last update is older than [`STALE_MS`] (X5). RMS
/// keeps the display stable; a weighted peak makes consonants and short
/// transients visible without pinning the meter. Both inputs use the same
/// calibrated dBFS scale ([`boost_level`]) so quiet speech visibly drives the
/// VU.
///
///
/// * `rms` — normalized RMS level in `[0,1]`.
/// * `peak` — normalized peak level in `[0,1]`.
/// * `age_ms` — ms since that level arrived (0 = fresh).
pub fn levels_to_intensity(rms: f64, peak: f64, age_ms: f64) -> f64 {
    let combined = boost_level(clamp01(rms).max(clamp01(peak) * PEAK_WEIGHT));
    if age_ms >= STALE_MS {
        return FLOOR;
    }
    // Linear ease toward the floor across the stale window.
    let freshness = if age_ms <= 0.0 {
        1.0
    } else {
        1.0 - clamp01(age_ms / STALE_MS)
    };
    FLOOR + (combined.max(FLOOR) - FLOOR) * freshness
}
