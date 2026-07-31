// vumeter.js — PURE envelope logic (feature 004; contract extension.md X5;
// research R5/R16/R16a/R17). RMS/peak → a headset-calibrated dBFS intensity
// with stale-decay. This is the shared envelope math `ribbon.js` (the
// 2026-07-30 wave-ribbon redesign) delegates to unchanged — the segmented
// bar-meter-only helpers (`intensityToActiveSegments`/`segmentColor`) that
// used to live here were removed once `ribbon.js` fully replaced their only
// caller (`hud.js`'s `BarMeterActor`, itself replaced by `WaveRibbonActor`).
// No Shell or gi imports. Carries energy only, never samples or content
// (constitution V, X6).

// Past this age with no fresh level, ease the VU to its floor rather than
// freezing on the last value (R5/SC-004).
export const STALE_MS = 300;
// Never fully dead while active, so the VU reads as "alive, quiet" not "off".
export const FLOOR = 0.04;

// Calibrated from a normal-speech Blackwire C5220 capture (2026-07-30):
// noise ≈ -80 dBFS, normal speech RMS ≈ -41 dBFS / peak ≈ -32 dBFS,
// strong speech RMS ≈ -32 dBFS / peak ≈ -23 dBFS. Map that useful acoustic
// range onto the full meter instead of applying a shallow linear gain.
const DB_FLOOR = -67;
const DB_CEILING = -14;
const PEAK_WEIGHT = 0.55;

/** Clamp to [0,1]. */
function clamp01(x) {
    if (Number.isNaN(x))
        return 0;
    return Math.max(0, Math.min(1, x));
}

/**
 * Perceptual lift of a raw [0,1] level: gain + power curve, clamped to [0,1].
 * Monotonic non-decreasing, so it never breaks the "louder → higher" contract.
 *
 * @param {number} level - normalized audio level in [0,1].
 * @returns {number} boosted level in [0,1].
 */
export function boostLevel(level) {
    const l = clamp01(level);
    if (l <= 0)
        return 0;
    const db = 20 * Math.log10(l);
    return clamp01((db - DB_FLOOR) / (DB_CEILING - DB_FLOOR));
}

/**
 * RMS + peak → VU intensity in [FLOOR,1], monotonic and clamped, decaying to
 * FLOOR once the last update is older than STALE_MS (X5). RMS keeps the
 * display stable; a weighted peak makes consonants and short transients
 * visible without pinning the meter. Both inputs use the same calibrated
 * dBFS scale (boostLevel) so quiet speech visibly drives the VU.
 *
 * @param {number} rms - normalized RMS level in [0,1].
 * @param {number} peak - normalized peak level in [0,1].
 * @param {number} [ageMs] - ms since that level arrived (0 = fresh).
 * @returns {number} intensity in [FLOOR, 1].
 */
export function levelsToIntensity(rms, peak, ageMs = 0) {
    const combined = boostLevel(Math.max(clamp01(rms), clamp01(peak) * PEAK_WEIGHT));
    if (ageMs >= STALE_MS)
        return FLOOR;
    // Linear ease toward the floor across the stale window.
    const freshness = ageMs <= 0 ? 1 : 1 - clamp01(ageMs / STALE_MS);
    return FLOOR + (Math.max(combined, FLOOR) - FLOOR) * freshness;
}
