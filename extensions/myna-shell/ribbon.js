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
//     Left-to-right flowing ribbon         (ribbonPaint.js)
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
import {Severity} from './states.js';

// ── Lifecycle phases & strand roles ──────────────────────────────────────

// The ribbon's lifecycle phases, as a frozen enum-like object (the string
// values are the wire contract computeRibbonModel/setPhase compare on):
//   - UNFOLD:   the brief reveal when a session starts.
//   - FLOW:     the steady-state flowing wave (audio-reactive).
//   - RELAX:    easing toward a thin idle line during a pause.
//   - MORPH:    crossfade from the flowing wave into travelling dots.
//   - COMPLETE: a brief convergence + brightness pulse before the pill clears.
export const RibbonPhase = Object.freeze({
    UNFOLD: 'unfold',
    FLOW: 'flow',
    RELAX: 'relax',
    MORPH: 'morph',
    COMPLETE: 'complete',
});

// The roles assigned to individual ribbon strands:
//   - VOICE:     the primary, most-reactive strand tracking the voice loudness.
//   - SECONDARY: delayed and softer strands adding depth behind the voice.
//   - BASE:      slow, low-amplitude ambient sway that stays alive in silence.
export const StrandRole = Object.freeze({
    VOICE: 'voice',
    SECONDARY: 'secondary',
    BASE: 'base',
});

// The visual tints applied to the ribbon:
//   - AMBER: recoverable issue (paused audio-reactivity, gentle pulse).
export const RibbonTint = Object.freeze({
    AMBER: 'amber',
});

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

// Exported because the GPU path (ribbonGlsl.js) regenerates this exact
// wave in GLSL rather than consuming the sampled points, and bakes these
// into the shader as `#define`s — so the two evaluate the same sine
// instead of two hand-copied literals that can drift apart.
export const FLOW_SPEED = 0.0032;
export const SPATIAL_FREQUENCY = 1.6;

// Never fully flat while active — reads as "alive, quiet", not "off"
// (same philosophy as vumeter.js's FLOOR).
export const IDLE_AMPLITUDE = 0.025;

// The amplitude RESPONSE CURVE (2026-07-31, "log scale" follow-up) — distinct
// from vumeter.js's dBFS calibration, which decides what counts as
// quiet/normal/loud speech in the first place. This reshapes the already-
// calibrated envelope specifically for the wave's visual amplitude: a mild
// logarithmic lift so modest-but-real speech reads as clearly present
// on-screen, while staying anchored at the same ceiling (env=1 → amplitude=1)
// `computeSafeScale` (ribbonPaint.js) already guarantees never clips —
// this curve reshapes what happens *below* that ceiling, never the ceiling
// itself, so it cannot reopen the cropping bug. See `shapeAmplitude`.
export const AMPLITUDE_CURVE_K = 5;

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
 * The amplitude response curve: a mild logarithmic lift so quiet-but-real
 * speech (a low but nonzero envelope) reads as clearly present, while
 * loud speech still tops out at the same ceiling as before (`1`) — a
 * "higher at low energy, capped at highest energy" shape, matching a
 * conventional audio loudness/log-encoding curve. Boundary-preserving
 * (`0→0`, `1→1`) and strictly monotonic, so it never changes the ceiling
 * `computeSafeScale` (ribbonPaint.js) guards against — only what happens
 * below it.
 *
 * @param {number} env - the calibrated, smoothed envelope in [0,1].
 * @returns {number} the shaped amplitude in [0,1].
 */
export function shapeAmplitude(env) {
    const e = clamp01(env);
    if (e <= 0)
        return 0;
    return clamp01(Math.log(1 + AMPLITUDE_CURVE_K * e) / Math.log(1 + AMPLITUDE_CURVE_K));
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
    const tau = tauMs ?? (clampedTarget > previous ? ATTACK_TAU_MS : RELEASE_TAU_MS);
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
 * Build one strand: its sampled points plus the *parameters* that
 * generated them. The parameters are echoed out so a renderer that
 * evaluates the wave itself — ribbonGlsl.js's GPU path regenerates this
 * exact sine per-pixel in GLSL — reproduces it from the same numbers that
 * produced `points`, rather than from a hand-copied second set of
 * constants that could drift. Additive: consumers reading only
 * `role`/`points`/`crest`/`alpha` (the X24 contract) are unaffected.
 *
 * @param {object} spec
 * @param {string} spec.role - a StrandRole value.
 * @param {number} spec.amplitude - peak |y| of the sine, in [0,1] units.
 * @param {number} spec.elapsedMs
 * @param {number} spec.phaseOffset - fixed per-strand radian offset.
 * @param {number} spec.delayMs - fixed per-strand time delay.
 * @param {number} spec.pointCount
 * @param {number} spec.alpha
 * @param {number} [spec.speedScale]
 * @param {boolean} [spec.withCrest] - compute real crest factors (the
 *     voice strand) rather than a zero-filled array (depth strands).
 * @returns {object} the strand.
 */
function makeStrand({
    role, amplitude, elapsedMs, phaseOffset, delayMs, pointCount, alpha,
    speedScale = 1, withCrest = false,
}) {
    const points = generateWavePoints(
        amplitude, elapsedMs, phaseOffset, delayMs, pointCount, speedScale);
    return {
        role,
        points,
        crest: withCrest ? crestFactors(points, amplitude) : points.map(() => 0),
        alpha,
        amplitude,
        phaseOffset,
        delayMs,
        speedScale,
    };
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
 * @param {string} [input.phase] - a RibbonPhase value (defaults to RibbonPhase.FLOW).
 * @param {number} [input.phaseElapsedMs]
 * @param {boolean} [input.reducedMotion] - static single-strand model (FR-022a).
 * @param {(string|null)} [input.severityTint] - Severity.RECOVERABLE / null.
 * @param {number} [input.strandCount] - defaults to DEFAULT_STRAND_COUNT (3-5 supported).
 * @param {number} [input.pointCount] - defaults to DEFAULT_POINTS_PER_STRAND.
 * @returns {{
 *   strands: {role: string, points: {x:number,y:number}[], crest: number[], alpha: number}[],
 *   dots: ({x:number, alpha:number}[]|null),
 *   convergence: ({x:number, y:number, alpha:number}|null),
 *   brightnessBoost: number,
 *   tint: (string|null),
 * }}
 */
export function computeRibbonModel({
    envelope,
    elapsedMs,
    phase = RibbonPhase.FLOW,
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
            strands: [{
                role: StrandRole.VOICE,
                points: flat,
                crest: flat.map(() => 0),
                alpha: 1.0,
                amplitude: 0,
                phaseOffset: 0,
                delayMs: 0,
                speedScale: 0,
            }],
            dots: null,
            convergence: null,
            brightnessBoost: 0,
            tint: severityTint === Severity.RECOVERABLE ? RibbonTint.AMBER : null,
            elapsedMs,
        };
    }

    // --- Recoverable severity: freeze audio-reactivity, gentle pulse -----
    if (severityTint === Severity.RECOVERABLE) {
        const pulsePhase = (elapsedMs % RECOVERABLE_PULSE_MS) / RECOVERABLE_PULSE_MS;
        const pulse = (Math.sin(pulsePhase * Math.PI * 2) + 1) / 2; // 0..1..0
        const amplitude = IDLE_AMPLITUDE * (0.6 + 0.4 * pulse);
        return {
            strands: [makeStrand({
                role: StrandRole.VOICE,
                amplitude,
                elapsedMs,
                phaseOffset: VOICE_PHASE,
                delayMs: 0,
                pointCount: points,
                alpha: 0.85,
                speedScale: BASE_SPEED_SCALE,
                withCrest: true,
            })],
            dots: null,
            convergence: null,
            brightnessBoost: 0,
            tint: RibbonTint.AMBER,
            elapsedMs,
        };
    }

    let env = clamp01(envelope);
    let brightnessBoost = 0;
    let dots = null;
    let convergence = null;

    switch (phase) {
    case RibbonPhase.UNFOLD:
        env *= unfoldProgress(phaseElapsedMs);
        break;
    case RibbonPhase.RELAX: {
        const p = relaxProgress(phaseElapsedMs);
        env = env * (1 - p) + Math.min(env, IDLE_AMPLITUDE) * p;
        break;
    }
    case RibbonPhase.MORPH: {
        const p = morphProgress(phaseElapsedMs);
        env *= 1 - p;
        // Crossfade in 3 travelling dots as the wave fades out.
        dots = [0, 1, 2].map(i => {
            const t = ((elapsedMs * FLOW_SPEED * 40 + i * 0.33) % 1 + 1) % 1;
            return {x: t, alpha: p};
        });
        break;
    }
    case RibbonPhase.COMPLETE: {
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
    case RibbonPhase.FLOW:
    default:
        // Steady-state: amplitude tracks the smoothed envelope (through
        // shapeAmplitude's response curve, below).
        break;
    }

    const voiceAmplitude = Math.max(IDLE_AMPLITUDE, shapeAmplitude(env));
    const layers = [makeStrand({
        role: StrandRole.VOICE,
        amplitude: voiceAmplitude,
        elapsedMs,
        phaseOffset: VOICE_PHASE,
        delayMs: 0,
        pointCount: points,
        alpha: 0.95,
        withCrest: true,
    })];

    if (strands >= 2) {
        layers.push(makeStrand({
            role: StrandRole.SECONDARY,
            amplitude: voiceAmplitude * SECONDARY_AMPLITUDE_SCALE,
            elapsedMs,
            phaseOffset: SECONDARY_PHASE,
            delayMs: SECONDARY_DELAY_MS,
            pointCount: points,
            alpha: SECONDARY_ALPHA,
        }));
    }
    if (strands >= 3) {
        layers.push(makeStrand({
            role: StrandRole.BASE,
            amplitude: BASE_AMPLITUDE,
            elapsedMs,
            phaseOffset: BASE_PHASE,
            delayMs: 0,
            pointCount: points,
            alpha: BASE_ALPHA,
            speedScale: BASE_SPEED_SCALE,
        }));
    }
    // Strand counts beyond 3 (up to the design's "three to five") add
    // extra secondary-like depth strands, cycling the same fixed offsets
    // at slightly different scales — still all driven by the one shared
    // envelope, never independent state.
    for (let extra = 3; extra < strands; extra++) {
        const scale = SECONDARY_AMPLITUDE_SCALE * (1 - (extra - 2) * 0.15);
        layers.push(makeStrand({
            role: StrandRole.SECONDARY,
            amplitude: voiceAmplitude * Math.max(0.2, scale),
            elapsedMs,
            phaseOffset: SECONDARY_PHASE + extra * 0.9,
            delayMs: SECONDARY_DELAY_MS * extra,
            pointCount: points,
            alpha: SECONDARY_ALPHA * Math.max(0.4, 1 - (extra - 2) * 0.2),
        }));
    }

    // `elapsedMs` is echoed through so ribbonPaint.js can add its own
    // time-based rendering-only effects (e.g. flowing "smoke" edge
    // turbulence) without needing new geometry fields on the strands
    // themselves — additive, doesn't change the strand contract X24 tests
    // already cover.
    return {strands: layers, dots, convergence, brightnessBoost, tint: null, elapsedMs};
}
