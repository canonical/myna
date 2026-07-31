// ribbon.js — PURE wave-ribbon logic (feature 004-gnome-shell-indicator,
// 2026-07-30 wave-ribbon redesign, refined per the "fabric in gentle
// airflow" design pass; research R17/R17a; contract extension.md X24).
//
// Pipeline (matches the design doc):
//
//     Microphone input
//           ↓
//     Calibrated instantaneous envelope   (vumeter.js's levelsToIntensity —
//                                          dBFS calibration + stale-decay,
//                                          updated ~20-30 Hz)
//           ↓
//     Smoothed loudness envelope          (applyEnvelopeSmoothing — a
//                                          ~250-400 ms one-pole low-pass,
//                                          state MAINTAINED BY THE CALLER
//                                          across repaint frames so this
//                                          module stays a pure function of
//                                          its explicit inputs)
//           ↓
//     Controlled wave amplitude + several offset strands (this module)
//           ↓
//     Left-to-right flowing ribbon         (ribbon-paint.js)
//
// Deliberately NOT an oscilloscope: no raw audio samples, no pitch/
// frequency input, no per-sample reproduction — only a single smoothed
// loudness envelope drives everything, with fixed, small per-strand
// offsets for depth (never independent per-strand state). "Audio drives
// the energy of the animation, while the product controls its shape."
//
// No Shell/St/Clutter/Gtk import — deterministic and unit-testable
// headless (test/ribbon.test.js), reusable verbatim by the dev-lab tool.

import {levelsToIntensity} from './vumeter.js';

// ── Tunables ────────────────────────────────────────────────────────────

export const DEFAULT_ENVELOPE_HZ = 24;
export const DEFAULT_STRAND_COUNT = 5;
export const DEFAULT_POINTS_PER_STRAND = 16;

// Two time constants for the *visual* envelope's ballistics (distinct from
// vumeter.js's arrival-time stale-decay) — the same "fast attack, slower
// release" pattern real audio meters use: quick to respond when getting
// louder (reactive, per direct feedback — tightened further after a live
// pass still read as too laggy, 2026-07-30), smoother on the way down (so
// natural pauses/decay still read as relaxing, not flickering). The
// release side is still within the design doc's original 250-400ms
// smoothing intent; the attack side is deliberately much faster than that
// for responsiveness — a near-immediate reaction to getting louder is
// exactly what "more reactive" means, even though it departs from the
// original symmetric range.
export const ATTACK_TAU_MS = 35;
export const SMOOTHING_TAU_MS = 280; // the release/decay time constant
export const RELEASE_TAU_MS = SMOOTHING_TAU_MS;

// Lifecycle-phase durations (ms).
export const UNFOLD_MS = 175; // 150-200ms
export const RELAX_MS = 500; // 400-600ms
export const MORPH_MS = 225; // 200-250ms ("a morph, not an abrupt replacement")
export const COMPLETE_MS = 400; // 300-500ms ("fast enough not to delay the user")

// A slow, gentle pulse period for the recoverable-issue tint (2026-07-30
// design refinement): motion pauses, but the ribbon still gently pulses
// rather than sitting perfectly still.
export const RECOVERABLE_PULSE_MS = 1800;

// Per-strand offsets (radians / ms / unitless amplitude scale). Fixed,
// small, deterministic — every strand reads the SAME smoothed envelope,
// only these constant offsets differ.
const VOICE_PHASE = 0;
const SECONDARY_PHASE = 1.35;
const SECONDARY_DELAY_MS = 260;
const SECONDARY_AMPLITUDE_SCALE = 0.65;
const SECONDARY_ALPHA = 0.45;
const BASE_PHASE = 2.7;
const BASE_ALPHA = 0.28;
// The base strand is "alive" almost independent of voice — a slow sway
// that keeps the ribbon from ever reading as fully static.
const BASE_AMPLITUDE = 0.05;
const BASE_SPEED_SCALE = 0.35;

const FLOW_SPEED = 0.0022;
const SPATIAL_FREQUENCY = 1.6;

// Never fully flat while active — reads as "alive, quiet", not "off"
// (same philosophy as vumeter.js's FLOOR).
const IDLE_AMPLITUDE = 0.025;

// A strong-syllable onset for the (optional, sparse) particle highlights:
// only a genuine rise in the smoothed envelope counts, never a raw sample
// spike, and the caller is expected to throttle how many are concurrently
// alive — a handful of sparse points, never a music-visualizer shower.
export const PARTICLE_ONSET_THRESHOLD = 0.14;
export const PARTICLE_LIFETIME_MS = 420;

/** Clamp to [0,1]. */
function clamp01(x) {
    if (Number.isNaN(x))
        return 0;
    return Math.max(0, Math.min(1, x));
}

/**
 * The calibrated, instantaneous loudness envelope — a thin, named
 * re-export of vumeter.js's dBFS mapping + arrival-time stale-decay
 * (R16a), reused unchanged. This is NOT yet the value the wave shape
 * should be driven by; see `applyEnvelopeSmoothing`.
 *
 * @param {number} rms - normalized RMS level in [0,1].
 * @param {number} peak - normalized peak level in [0,1].
 * @param {number} [ageMs] - ms since that level arrived (0 = fresh).
 * @returns {number} instantaneous envelope intensity in [FLOOR, 1].
 */
export function computeEnvelope(rms, peak, ageMs = 0) {
    return levelsToIntensity(rms, peak, ageMs);
}

/**
 * One step of a one-pole low-pass filter (exponential smoothing) toward
 * `target`, given `dtMs` elapsed since the previous step. Pure and
 * deterministic: same (previous, target, dtMs, tauMs) always produces the
 * same result. The CALLER owns the running smoothed value as state across
 * repaint frames (mirroring how phase/phaseStartedAt are already
 * caller-owned) — this keeps the module itself side-effect-free.
 *
 * Uses **attack/release ballistics** (the same pattern real audio meters
 * use) when `tauMs` is not explicitly given: a fast `ATTACK_TAU_MS` while
 * the target is rising (so the ribbon reacts promptly to getting louder),
 * and the slower `RELEASE_TAU_MS`/`SMOOTHING_TAU_MS` while it's falling
 * (so decay/pauses still ease rather than flicker). Pass an explicit
 * `tauMs` to force a single, symmetric time constant instead (used by a
 * few tests that don't care about attack/release, only convergence).
 *
 * @param {number} previous - the previously smoothed value.
 * @param {number} target - the latest instantaneous envelope value.
 * @param {number} dtMs - elapsed ms since `previous` was computed.
 * @param {number} [tauMs] - an explicit, symmetric time constant; when
 *     omitted, attack/release ballistics are used instead.
 * @returns {number} the new smoothed value.
 */
export function applyEnvelopeSmoothing(previous, target, dtMs, tauMs = undefined) {
    const clampedTarget = clamp01(target);
    const tau = tauMs !== undefined ? tauMs : (clampedTarget > previous ? ATTACK_TAU_MS : RELEASE_TAU_MS);
    if (tau <= 0 || dtMs <= 0)
        return clampedTarget;
    const alpha = 1 - Math.exp(-dtMs / tau);
    return clamp01(previous + (clampedTarget - previous) * alpha);
}

/**
 * Whether a rise in the *smoothed* envelope is large enough to count as a
 * strong-syllable onset worth a sparse particle highlight. Pure — the
 * caller supplies the delta (this frame's smoothed value minus last
 * frame's) so this module never needs to remember history itself.
 *
 * @param {number} envelopeDelta - smoothed-envelope rise since last frame.
 * @returns {boolean}
 */
export function isStrongSyllableOnset(envelopeDelta) {
    return envelopeDelta >= PARTICLE_ONSET_THRESHOLD;
}

function phaseProgress(elapsedMs, durationMs) {
    if (durationMs <= 0)
        return 1;
    return clamp01(elapsedMs / durationMs);
}

/** Unfold progress: the ribbon's brief reveal when a session starts. */
export function unfoldProgress(elapsedMs) {
    return phaseProgress(elapsedMs, UNFOLD_MS);
}

/** Relax progress: easing toward a thin idle line during a pause. */
export function relaxProgress(elapsedMs) {
    return phaseProgress(elapsedMs, RELAX_MS);
}

/** Morph progress: crossfade from the flowing wave into travelling dots. */
export function morphProgress(elapsedMs) {
    return phaseProgress(elapsedMs, MORPH_MS);
}

/** Completion progress: a brief, non-blocking convergence + brightness pulse. */
export function completeProgress(elapsedMs) {
    return phaseProgress(elapsedMs, COMPLETE_MS);
}

function generateWavePoints(amplitude, elapsedMs, phaseOffset, delayMs, pointCount, speedScale = 1) {
    const count = Math.max(2, Math.floor(pointCount));
    const points = [];
    for (let i = 0; i < count; i++) {
        const t = i / (count - 1);
        const angle = t * SPATIAL_FREQUENCY * Math.PI * 2 +
            phaseOffset + (elapsedMs - delayMs) * FLOW_SPEED * speedScale;
        points.push({x: t, y: Math.sin(angle) * amplitude});
    }
    return points;
}

/** Per-point "crest factor" in [0,1] — how close each point is to the
 * strand's own peak, used to blend the paint colour toward the highlight
 * tone at the loudest parts of the wave ("brighter orange or warm cream
 * can highlight the active crest"). */
function crestFactors(points, amplitude) {
    if (amplitude <= 0)
        return points.map(() => 0);
    return points.map(p => clamp01(Math.abs(p.y) / amplitude));
}

/**
 * Compute the full ribbon model from an already-smoothed envelope value.
 * Four conceptual layers (design doc's "layered construction"):
 *   - `base` strand: slow, low-amplitude sway, nearly independent of the
 *     voice — keeps the ribbon "alive" even in silence.
 *   - `voice` strand: the main, most-reactive strand, driven directly by
 *     the smoothed envelope, with per-point crest brightness.
 *   - `secondary` strand: delayed and less opaque, for depth.
 *   - optional sparse particle highlights (caller-managed list; see
 *     `isStrongSyllableOnset` above) are NOT generated here — this
 *     function only returns the strand geometry; particle lifetime
 *     bookkeeping is the caller's (mirrors phase/smoothing state).
 *
 * During `morph` the strands crossfade into 3 travelling dots; during
 * `complete` they converge toward a single centred point with a brief
 * brightness pulse (FR-010d). A `severityTint` of `'recoverable'` pauses
 * audio-reactivity and applies a slow, gentle pulse instead (motion
 * "pauses" but never looks frozen-dead).
 *
 * @param {object} input
 * @param {number} input.envelope - the SMOOTHED loudness envelope [0,1]
 *     (from `applyEnvelopeSmoothing`, not the raw instantaneous value).
 * @param {number} input.elapsedMs - elapsed time for the flow animation.
 * @param {('unfold'|'flow'|'relax'|'morph'|'complete')} [input.phase]
 * @param {number} [input.phaseElapsedMs]
 * @param {boolean} [input.reducedMotion] - static single-strand model (FR-022a).
 * @param {(('recoverable'|'critical')|null)} [input.severityTint]
 * @param {number} [input.strandCount] - defaults to DEFAULT_STRAND_COUNT (3-5 supported).
 * @param {number} [input.pointCount] - defaults to DEFAULT_POINTS_PER_STRAND.
 * @returns {{
 *   strands: {role: string, points: {x:number,y:number}[], crest: number[], alpha: number}[],
 *   dots: ({x:number, alpha:number}[]|null),
 *   convergence: ({x:number, y:number, alpha:number}|null),
 *   brightnessBoost: number,
 *   tint: (('amber')|null),
 * }}
 */
export function computeRibbonModel({
    envelope,
    elapsedMs,
    phase = 'flow',
    phaseElapsedMs = 0,
    reducedMotion = false,
    severityTint = null,
    strandCount = DEFAULT_STRAND_COUNT,
    pointCount = DEFAULT_POINTS_PER_STRAND,
} = {}) {
    const points = Math.max(2, Math.floor(pointCount));
    const strands = Math.max(1, Math.floor(strandCount));

    if (reducedMotion) {
        const flat = [];
        for (let i = 0; i < points; i++)
            flat.push({x: i / (points - 1), y: 0});
        return {
            strands: [{role: 'voice', points: flat, crest: flat.map(() => 0), alpha: 1.0}],
            dots: null,
            convergence: null,
            brightnessBoost: 0,
            tint: severityTint === 'recoverable' ? 'amber' : null,
            elapsedMs,
        };
    }

    // --- Recoverable severity: freeze audio-reactivity, gentle pulse -----
    if (severityTint === 'recoverable') {
        const pulsePhase = (elapsedMs % RECOVERABLE_PULSE_MS) / RECOVERABLE_PULSE_MS;
        const pulse = (Math.sin(pulsePhase * Math.PI * 2) + 1) / 2; // 0..1..0
        const amplitude = IDLE_AMPLITUDE * (0.6 + 0.4 * pulse);
        const voicePoints = generateWavePoints(amplitude, elapsedMs, VOICE_PHASE, 0, points, BASE_SPEED_SCALE);
        return {
            strands: [{
                role: 'voice',
                points: voicePoints,
                crest: crestFactors(voicePoints, amplitude),
                alpha: 0.85,
            }],
            dots: null,
            convergence: null,
            brightnessBoost: 0,
            tint: 'amber',
            elapsedMs,
        };
    }

    let env = clamp01(envelope);
    let brightnessBoost = 0;
    let dots = null;
    let convergence = null;

    switch (phase) {
    case 'unfold':
        env *= unfoldProgress(phaseElapsedMs);
        break;
    case 'relax': {
        const p = relaxProgress(phaseElapsedMs);
        env = env * (1 - p) + Math.min(env, IDLE_AMPLITUDE) * p;
        break;
    }
    case 'morph': {
        const p = morphProgress(phaseElapsedMs);
        env *= 1 - p;
        // Crossfade in 3 travelling dots as the wave fades out.
        dots = [0, 1, 2].map(i => {
            const t = ((elapsedMs * FLOW_SPEED * 40 + i * 0.33) % 1 + 1) % 1;
            return {x: t, alpha: p};
        });
        break;
    }
    case 'complete': {
        const p = completeProgress(phaseElapsedMs);
        env = 0;
        brightnessBoost = Math.sin(clamp01(p) * Math.PI); // rises then falls, 0→1→0
        // The convergence point's own alpha MUST follow the same fade —
        // otherwise it lingers at full opacity for as long as the phase
        // itself remains 'complete', which can be indefinitely (a bug
        // found via live testing, 2026-07-30: the dot stayed visible
        // until the next session started, since nothing else resets the
        // phase away from 'complete' in the meantime).
        convergence = {x: 0.5, y: 0, alpha: brightnessBoost};
        break;
    }
    case 'flow':
    default:
        // Steady-state: amplitude tracks the smoothed envelope directly.
        break;
    }

    const voiceAmplitude = Math.max(IDLE_AMPLITUDE, env);
    const voicePoints = generateWavePoints(voiceAmplitude, elapsedMs, VOICE_PHASE, 0, points);
    const layers = [{
        role: 'voice',
        points: voicePoints,
        crest: crestFactors(voicePoints, voiceAmplitude),
        alpha: 0.95,
    }];

    if (strands >= 2) {
        const secondaryAmplitude = voiceAmplitude * SECONDARY_AMPLITUDE_SCALE;
        const secondaryPoints = generateWavePoints(
            secondaryAmplitude, elapsedMs, SECONDARY_PHASE, SECONDARY_DELAY_MS, points);
        layers.push({
            role: 'secondary',
            points: secondaryPoints,
            crest: secondaryPoints.map(() => 0),
            alpha: SECONDARY_ALPHA,
        });
    }
    if (strands >= 3) {
        const basePoints = generateWavePoints(
            BASE_AMPLITUDE, elapsedMs, BASE_PHASE, 0, points, BASE_SPEED_SCALE);
        layers.push({
            role: 'base',
            points: basePoints,
            crest: basePoints.map(() => 0),
            alpha: BASE_ALPHA,
        });
    }
    // Strand counts beyond 3 (up to the design's "three to five") add
    // extra secondary-like depth strands, cycling the same fixed offsets
    // at slightly different scales — still all driven by the one shared
    // envelope, never independent state.
    for (let extra = 3; extra < strands; extra++) {
        const scale = SECONDARY_AMPLITUDE_SCALE * (1 - (extra - 2) * 0.15);
        const phaseOffset = SECONDARY_PHASE + extra * 0.9;
        const extraPoints = generateWavePoints(
            voiceAmplitude * Math.max(0.2, scale), elapsedMs, phaseOffset,
            SECONDARY_DELAY_MS * extra, points);
        layers.push({
            role: 'secondary',
            points: extraPoints,
            crest: extraPoints.map(() => 0),
            alpha: SECONDARY_ALPHA * Math.max(0.4, 1 - (extra - 2) * 0.2),
        });
    }

    // `elapsedMs` is echoed through so ribbon-paint.js can add its own
    // time-based rendering-only effects (e.g. flowing "smoke" edge
    // turbulence) without needing new geometry fields on the strands
    // themselves — additive, doesn't change the strand contract X24 tests
    // already cover.
    return {strands: layers, dots, convergence, brightnessBoost, tint: null, elapsedMs};
}
