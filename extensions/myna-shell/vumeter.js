// vumeter.js — PURE VU logic (feature 004; contract extension.md X5; research
// R5/R7). Level → a normalized intensity with stale-decay, and a bar-height
// profile for a ribbon VU. No Shell, no gi imports — unit-tested headless.
// Carries energy only, never samples or content (constitution V, X6).

// Past this age with no fresh level, ease the VU to its floor rather than
// freezing on the last value (R5/SC-004).
export const STALE_MS = 300;
// Never fully dead while active, so the VU reads as "alive, quiet" not "off".
export const FLOOR = 0.06;
// Speech is quiet in full-scale terms (RMS ~0.03–0.15). A raw linear level
// leaves the VU barely twitching, so we lift it perceptually with a soft
// saturation curve `1 - exp(-GAIN·l)`: it lifts quiet speech steeply, eases
// toward 1 without ever pinning, and is strictly monotonic (contract holds).
const GAIN = 6.0;

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
    return 1 - Math.exp(-GAIN * clamp01(level));
}

/**
 * Level → glow/VU intensity in [FLOOR,1], monotonic and clamped, decaying to
 * FLOOR once the last update is older than STALE_MS (X5). The raw level is
 * perceptually boosted (boostLevel) so quiet speech visibly drives the VU.
 *
 * @param {number} level - normalized audio level in [0,1].
 * @param {number} [ageMs] - ms since that level arrived (0 = fresh).
 * @returns {number} intensity in [FLOOR, 1].
 */
export function levelToIntensity(level, ageMs = 0) {
    const l = boostLevel(level);
    if (ageMs >= STALE_MS)
        return FLOOR;
    // Linear ease toward the floor across the stale window.
    const freshness = ageMs <= 0 ? 1 : 1 - clamp01(ageMs / STALE_MS);
    return FLOOR + (Math.max(l, FLOOR) - FLOOR) * freshness;
}

/**
 * A symmetric bar-height profile for a ribbon VU: `barCount` values in
 * [FLOOR,1], tallest in the centre and tapering to the edges, scaled by the
 * current intensity. Deterministic (no randomness) so it's testable; a view
 * may add its own liveliness on top.
 *
 * @param {number} level - normalized audio level in [0,1].
 * @param {number} [ageMs] - ms since that level (for stale-decay).
 * @param {number} [barCount] - number of bars (>=1).
 * @returns {number[]} heights in [FLOOR,1], length `barCount`.
 */
export function levelToBars(level, ageMs = 0, barCount = 24) {
    const n = Math.max(1, Math.floor(barCount));
    const intensity = levelToIntensity(level, ageMs);
    const mid = (n - 1) / 2;
    const bars = new Array(n);
    for (let i = 0; i < n; i++) {
        // 1 at centre → ~0.35 at the edges: a smooth spindle shape.
        const dist = mid === 0 ? 0 : Math.abs(i - mid) / mid;
        const shape = 1 - 0.65 * dist * dist;
        bars[i] = Math.max(FLOOR, intensity * shape);
    }
    return bars;
}
