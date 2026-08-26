//! ribbon — PURE wave-ribbon logic (feature 004-gnome-shell-indicator,
//! 2026-07-30 wave-ribbon redesign, refined per the "fabric in gentle
//! airflow" design pass; research R17/R17a; contract extension.md X24).
//!
//! Pipeline (matches the design doc):
//!
//! ```text
//! Microphone input
//!       ↓
//! Calibrated instantaneous envelope   (vumeter's levels_to_intensity —
//!                                      dBFS calibration + stale-decay,
//!                                      updated ~20-30 Hz)
//!       ↓
//! Smoothed loudness envelope          (apply_envelope_smoothing — an
//!                                      attack/release-ballistics one-pole
//!                                      low-pass, state MAINTAINED BY THE
//!                                      CALLER across repaint frames so this
//!                                      module stays a pure function of its
//!                                      explicit inputs)
//!       ↓
//! Controlled wave amplitude + several offset strands (this module)
//!       ↓
//! Left-to-right flowing ribbon         (the GLArea/GPU renderer, shader.rs)
//! ```
//!
//! Deliberately NOT an oscilloscope: no raw audio samples, no pitch/
//! frequency input, no per-sample reproduction — only a single smoothed
//! loudness envelope drives everything, with fixed, small per-strand offsets
//! for depth (never independent per-strand state). "Audio drives the energy
//! of the animation, while the product controls its shape."
//!
//! Ported 1:1 from `extensions/myna-shell/ribbon.js` (deleted with the old
//! bundle; this is now the single source of truth). No GTK imports —
//! deterministic and unit-testable headless ([`tests/ribbon.rs`]).

use crate::states::Severity;
use crate::vumeter::levels_to_intensity;

// ── Lifecycle phases & strand roles ──────────────────────────────────────

/// The ribbon's lifecycle phases:
/// - [`RibbonPhase::Unfold`]: the brief reveal when a session starts.
/// - [`RibbonPhase::Flow`]: the steady-state flowing wave (audio-reactive).
/// - [`RibbonPhase::Morph`]: crossfade from the flowing wave into travelling
///   dots.
/// - [`RibbonPhase::Complete`]: a brief convergence + brightness pulse
///   before the pill clears.
///
/// There is deliberately no pause/relax phase. FR-010a's "relax smoothly
/// toward a thin idle line during pauses" is delivered by the envelope's
/// [`RELEASE_TAU_MS`] ballistics below, continuously and in proportion to
/// the actual audio, rather than by a phase: a phase needs a pause
/// *detector*, and there is no threshold that works. Trip it at ~400ms and
/// it fires on ordinary inter-word gaps (the VU reaches its floor after
/// 300ms of silence), so the ribbon would collapse and snap back
/// mid-sentence; wait ~1.5s to avoid that and the release curve has already
/// done the job. A RELAX phase also capped amplitude at the idle floor
/// regardless of input, so resuming speech mid-ramp was actively suppressed
/// — something the release curve cannot do. Removed 2026-08-24, having never
/// been reachable: `ribbon_phase_for_state_key` never returned it and the
/// renderer never selected it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RibbonPhase {
    Unfold,
    Flow,
    Morph,
    Complete,
}

/// The roles assigned to individual ribbon strands:
/// - [`StrandRole::Voice`]: the primary, most-reactive strand tracking the
///   voice loudness.
/// - [`StrandRole::Secondary`]: delayed and softer strands adding depth
///   behind the voice.
/// - [`StrandRole::Base`]: slow, low-amplitude ambient sway that stays alive
///   in silence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrandRole {
    Voice,
    Secondary,
    Base,
}

/// The visual tints applied to the ribbon: [`RibbonTint::Amber`] —
/// recoverable issue (paused audio-reactivity, gentle pulse).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RibbonTint {
    Amber,
}

// ── Tunables ────────────────────────────────────────────────────────────

pub const DEFAULT_ENVELOPE_HZ: f64 = 24.0;
pub const DEFAULT_STRAND_COUNT: usize = 5;
pub const DEFAULT_POINTS_PER_STRAND: usize = 16;

/// The attack (rising) time constant for the *visual* envelope's ballistics
/// (distinct from the vumeter's arrival-time stale-decay) — tightened twice
/// after live passes read as too laggy: a near-immediate reaction to getting
/// louder is exactly what "more reactive" means.
pub const ATTACK_TAU_MS: f64 = 35.0;
/// The release/decay time constant — the slower, smoother side, still within
/// the design doc's original 250-400ms smoothing intent. This is what
/// satisfies FR-010a's pause behaviour now that the RELAX phase is gone: a
/// pause eases the wave from a speaking amplitude toward roughly an eighth
/// of it over ~1.5s, smoothly and in proportion to the audio, with no
/// threshold to misfire on.
pub const SMOOTHING_TAU_MS: f64 = 280.0;
pub const RELEASE_TAU_MS: f64 = SMOOTHING_TAU_MS;

// Lifecycle-phase durations (ms).
pub const UNFOLD_MS: f64 = 175.0; // 150-200ms
pub const MORPH_MS: f64 = 225.0; // 200-250ms ("a morph, not an abrupt replacement")
pub const COMPLETE_MS: f64 = 400.0; // 300-500ms ("fast enough not to delay the user")

/// A slow, gentle pulse period for the recoverable-issue tint: motion
/// pauses, but the ribbon still gently pulses rather than sitting perfectly
/// still.
pub const RECOVERABLE_PULSE_MS: f64 = 1800.0;

// Per-strand offsets (radians / ms / unitless amplitude scale). Fixed,
// small, deterministic — every strand reads the SAME smoothed envelope,
// only these constant offsets differ. Public because the GPU path
// (shader.rs) regenerates this exact wave in GLSL rather than consuming the
// sampled points, and bakes these into the shader as `#define`s — so the two
// evaluate the same sine instead of two hand-copied literals that can drift
// apart.
pub const VOICE_PHASE: f64 = 0.0;
pub const SECONDARY_PHASE: f64 = 1.35;
pub const SECONDARY_DELAY_MS: f64 = 260.0;
pub const SECONDARY_AMPLITUDE_SCALE: f64 = 0.65;
pub const SECONDARY_ALPHA: f64 = 0.45;
pub const BASE_PHASE: f64 = 2.7;
pub const BASE_ALPHA: f64 = 0.28;
/// The base strand is "alive" almost independent of voice — a slow sway that
/// keeps the ribbon from ever reading as fully static.
pub const BASE_AMPLITUDE: f64 = 0.05;
pub const BASE_SPEED_SCALE: f64 = 0.35;

/// How fast the wave flows left-to-right (radians per ms) — shared with the
/// GPU path's shader `#define`s (see the per-strand offsets note above).
pub const FLOW_SPEED: f64 = 0.0032;
/// Wave cycles across the strand's width — shared with the GPU path.
pub const SPATIAL_FREQUENCY: f64 = 1.6;

/// Never fully flat while active — reads as "alive, quiet", not "off" (same
/// philosophy as the vumeter's [`crate::vumeter::FLOOR`]).
pub const IDLE_AMPLITUDE: f64 = 0.025;

/// The amplitude response curve's strength (the "log scale" follow-up,
/// 2026-07-31) — distinct from the vumeter's dBFS calibration, which decides
/// what counts as quiet/normal/loud speech in the first place. This reshapes
/// the already-calibrated envelope specifically for the wave's visual
/// amplitude: a mild logarithmic lift so modest-but-real speech reads as
/// clearly present on-screen, while staying anchored at the same ceiling
/// (`env=1 → amplitude=1`). See [`shape_amplitude`].
pub const AMPLITUDE_CURVE_K: f64 = 5.0;

/// A strong-syllable onset for the (optional, sparse) particle highlights:
/// only a genuine rise in the smoothed envelope counts, never a raw sample
/// spike, and the caller is expected to throttle how many are concurrently
/// alive — a handful of sparse points, never a music-visualizer shower.
pub const PARTICLE_ONSET_THRESHOLD: f64 = 0.14;
pub const PARTICLE_LIFETIME_MS: f64 = 420.0;

/// Clamp to `[0,1]`; NaN collapses to 0.
fn clamp01(x: f64) -> f64 {
    if x.is_nan() {
        return 0.0;
    }
    x.clamp(0.0, 1.0)
}

/// The amplitude response curve: a mild logarithmic lift so quiet-but-real
/// speech (a low but nonzero envelope) reads as clearly present, while loud
/// speech still tops out at the same ceiling as before (`1`) — a "higher at
/// low energy, capped at highest energy" shape, matching a conventional
/// audio loudness/log-encoding curve. Boundary-preserving (`0→0`, `1→1`)
/// and strictly monotonic, so it never changes the ceiling the renderer's
/// safe-scale guard protects against — only what happens below it.
///
/// Port of `ribbon.js`'s `shapeAmplitude`.
pub fn shape_amplitude(env: f64) -> f64 {
    let e = clamp01(env);
    if e <= 0.0 {
        return 0.0;
    }
    clamp01((1.0 + AMPLITUDE_CURVE_K * e).ln() / (1.0 + AMPLITUDE_CURVE_K).ln())
}

/// The calibrated, instantaneous loudness envelope — a thin, named
/// re-export of the vumeter's dBFS mapping + arrival-time stale-decay
/// (R16a), reused unchanged. This is NOT yet the value the wave shape
/// should be driven by; see [`apply_envelope_smoothing`].
///
/// Port of `ribbon.js`'s `computeEnvelope`.
pub fn compute_envelope(rms: f64, peak: f64, age_ms: f64) -> f64 {
    levels_to_intensity(rms, peak, age_ms)
}

/// One step of a one-pole low-pass filter (exponential smoothing) toward
/// `target`, given `dt_ms` elapsed since the previous step, with
/// **attack/release ballistics** (the same pattern real audio meters use):
/// a fast [`ATTACK_TAU_MS`] while the target is rising, the slower
/// [`RELEASE_TAU_MS`] while it's falling. Pure and deterministic; the CALLER
/// owns the running smoothed value as state across repaint frames (mirroring
/// how the phase/phase-start timestamps are caller-owned) — this keeps the
/// module side-effect-free.
///
/// Port of `ribbon.js`'s `applyEnvelopeSmoothing` (default-tau form).
pub fn apply_envelope_smoothing(previous: f64, target: f64, dt_ms: f64) -> f64 {
    let clamped_target = clamp01(target);
    let tau = if clamped_target > previous {
        ATTACK_TAU_MS
    } else {
        RELEASE_TAU_MS
    };
    smoothing_step(previous, clamped_target, dt_ms, tau)
}

/// The explicit-`tau` form of [`apply_envelope_smoothing`] — forces a
/// single, symmetric time constant instead of attack/release auto-selection
/// (used by a few tests that care only about convergence, not ballistics).
///
/// Port of `ribbon.js`'s `applyEnvelopeSmoothing(previous, target, dtMs,
/// tauMs)`.
pub fn apply_envelope_smoothing_with_tau(
    previous: f64,
    target: f64,
    dt_ms: f64,
    tau_ms: f64,
) -> f64 {
    smoothing_step(previous, clamp01(target), dt_ms, tau_ms)
}

fn smoothing_step(previous: f64, clamped_target: f64, dt_ms: f64, tau_ms: f64) -> f64 {
    if tau_ms <= 0.0 || dt_ms <= 0.0 {
        return clamped_target;
    }
    let alpha = 1.0 - (-dt_ms / tau_ms).exp();
    clamp01(previous + (clamped_target - previous) * alpha)
}

/// Whether a rise in the *smoothed* envelope is large enough to count as a
/// strong-syllable onset worth a sparse particle highlight. Pure — the
/// caller supplies the delta (this frame's smoothed value minus last
/// frame's) so this module never needs to remember history itself.
///
/// Port of `ribbon.js`'s `isStrongSyllableOnset`.
pub fn is_strong_syllable_onset(envelope_delta: f64) -> bool {
    envelope_delta >= PARTICLE_ONSET_THRESHOLD
}

fn phase_progress(elapsed_ms: f64, duration_ms: f64) -> f64 {
    if duration_ms <= 0.0 {
        return 1.0;
    }
    clamp01(elapsed_ms / duration_ms)
}

/// Unfold progress: the ribbon's brief reveal when a session starts.
pub fn unfold_progress(elapsed_ms: f64) -> f64 {
    phase_progress(elapsed_ms, UNFOLD_MS)
}

/// Morph progress: crossfade from the flowing wave into travelling dots.
pub fn morph_progress(elapsed_ms: f64) -> f64 {
    phase_progress(elapsed_ms, MORPH_MS)
}

/// Completion progress: a brief, non-blocking convergence + brightness
/// pulse.
pub fn complete_progress(elapsed_ms: f64) -> f64 {
    phase_progress(elapsed_ms, COMPLETE_MS)
}

// ── The model ─────────────────────────────────────────────────────────────

/// One sampled point of a strand's centreline, in normalized `[0,1]`
/// coordinates (`x` across the ribbon's width, `y` amplitude).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// One strand: its sampled points plus the *parameters* that generated
/// them. The parameters are echoed out so a renderer that evaluates the
/// wave itself — the GPU path regenerates this exact sine per-pixel in GLSL
/// — reproduces it from the same numbers that produced `points`, rather
/// than from a hand-copied second set of constants that could drift.
/// Additive: consumers reading only `role`/`points`/`crest`/`alpha` (the X24
/// contract) are unaffected.
#[derive(Clone, Debug, PartialEq)]
pub struct Strand {
    pub role: StrandRole,
    pub points: Vec<Point>,
    /// Per-point "crest factor" in `[0,1]` — how close each point is to the
    /// strand's own peak, used to blend the paint colour toward the
    /// highlight tone at the loudest parts of the wave.
    pub crest: Vec<f64>,
    pub alpha: f64,
    pub amplitude: f64,
    pub phase_offset: f64,
    pub delay_ms: f64,
    pub speed_scale: f64,
}

/// A travelling dot revealed during the `morph` crossfade.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dot {
    pub x: f64,
    pub alpha: f64,
}

/// The convergence point + brightness pulse of the `complete` phase. Its
/// `alpha` follows [`RibbonModel::brightness_boost`] exactly (same fade
/// curve) — a 2026-07-30 live-testing regression: it used to be hardcoded to
/// 1 and lingered at full opacity indefinitely.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Convergence {
    pub x: f64,
    pub y: f64,
    pub alpha: f64,
}

/// The full ribbon model (X24's contract): layered strands, the morph dots,
/// the complete-phase convergence, the brightness pulse, and the tint.
#[derive(Clone, Debug, PartialEq)]
pub struct RibbonModel {
    pub strands: Vec<Strand>,
    pub dots: Option<Vec<Dot>>,
    pub convergence: Option<Convergence>,
    pub brightness_boost: f64,
    pub tint: Option<RibbonTint>,
    /// Echoed through so renderers can add their own time-based,
    /// rendering-only effects without needing new geometry fields on the
    /// strands themselves.
    pub elapsed_ms: f64,
}

/// The input of [`compute_ribbon_model`] — the named-argument object of the
/// GJS original, with the same defaults.
#[derive(Clone, Debug)]
pub struct RibbonInput {
    /// The SMOOTHED loudness envelope `[0,1]` (from
    /// [`apply_envelope_smoothing`], not the raw instantaneous value).
    pub envelope: f64,
    /// Elapsed time for the flow animation.
    pub elapsed_ms: f64,
    pub phase: RibbonPhase,
    pub phase_elapsed_ms: f64,
    /// Static single-strand model (FR-022a).
    pub reduced_motion: bool,
    pub severity_tint: Option<Severity>,
    /// 3-5 supported (design doc's "three to five").
    pub strand_count: usize,
    pub point_count: usize,
}

impl Default for RibbonInput {
    fn default() -> Self {
        Self {
            envelope: 0.0,
            elapsed_ms: 0.0,
            phase: RibbonPhase::Flow,
            phase_elapsed_ms: 0.0,
            reduced_motion: false,
            severity_tint: None,
            strand_count: DEFAULT_STRAND_COUNT,
            point_count: DEFAULT_POINTS_PER_STRAND,
        }
    }
}

fn generate_wave_points(
    amplitude: f64,
    elapsed_ms: f64,
    phase_offset: f64,
    delay_ms: f64,
    point_count: usize,
    speed_scale: f64,
) -> Vec<Point> {
    let count = point_count.max(2);
    let mut points = Vec::with_capacity(count);
    for i in 0..count {
        let t = i as f64 / (count - 1) as f64;
        let angle = t * SPATIAL_FREQUENCY * std::f64::consts::PI * 2.0
            + phase_offset
            + (elapsed_ms - delay_ms) * FLOW_SPEED * speed_scale;
        points.push(Point {
            x: t,
            y: angle.sin() * amplitude,
        });
    }
    points
}

fn crest_factors(points: &[Point], amplitude: f64) -> Vec<f64> {
    if amplitude <= 0.0 {
        return vec![0.0; points.len()];
    }
    points
        .iter()
        .map(|p| clamp01(p.y.abs() / amplitude))
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn make_strand(
    role: StrandRole,
    amplitude: f64,
    elapsed_ms: f64,
    phase_offset: f64,
    delay_ms: f64,
    point_count: usize,
    alpha: f64,
    speed_scale: f64,
    with_crest: bool,
) -> Strand {
    let points = generate_wave_points(
        amplitude,
        elapsed_ms,
        phase_offset,
        delay_ms,
        point_count,
        speed_scale,
    );
    Strand {
        role,
        crest: if with_crest {
            crest_factors(&points, amplitude)
        } else {
            vec![0.0; points.len()]
        },
        points,
        alpha,
        amplitude,
        phase_offset,
        delay_ms,
        speed_scale,
    }
}

/// Compute the full ribbon model from an already-smoothed envelope value.
///
/// Four conceptual layers (the design doc's "layered construction"):
/// - `base` strand: slow, low-amplitude sway, nearly independent of the
///   voice — keeps the ribbon "alive" even in silence.
/// - `voice` strand: the main, most-reactive strand, driven directly by the
///   smoothed envelope, with per-point crest brightness.
/// - `secondary` strand: delayed and less opaque, for depth.
/// - optional sparse particle highlights (caller-managed list; see
///   [`is_strong_syllable_onset`]) are NOT generated here — this function
///   only returns the strand geometry; particle lifetime bookkeeping is the
///   caller's (mirrors phase/smoothing state).
///
/// During `morph` the strands crossfade into 3 travelling dots; during
/// `complete` they converge toward a single centred point with a brief
/// brightness pulse (FR-010d). A `severity_tint` of `Recoverable` pauses
/// audio-reactivity and applies a slow, gentle pulse instead (motion
/// "pauses" but never looks frozen-dead).
///
/// Port of `ribbon.js`'s `computeRibbonModel`.
pub fn compute_ribbon_model(input: RibbonInput) -> RibbonModel {
    let points = input.point_count.max(2);
    let strands = input.strand_count.max(1);

    if input.reduced_motion {
        let flat: Vec<Point> = (0..points)
            .map(|i| Point {
                x: i as f64 / (points - 1) as f64,
                y: 0.0,
            })
            .collect();
        return RibbonModel {
            strands: vec![Strand {
                role: StrandRole::Voice,
                crest: vec![0.0; flat.len()],
                points: flat,
                alpha: 1.0,
                amplitude: 0.0,
                phase_offset: 0.0,
                delay_ms: 0.0,
                speed_scale: 0.0,
            }],
            dots: None,
            convergence: None,
            brightness_boost: 0.0,
            tint: if input.severity_tint == Some(Severity::Recoverable) {
                Some(RibbonTint::Amber)
            } else {
                None
            },
            elapsed_ms: input.elapsed_ms,
        };
    }

    // --- Recoverable severity: freeze audio-reactivity, gentle pulse -----
    if input.severity_tint == Some(Severity::Recoverable) {
        let pulse_phase = (input.elapsed_ms % RECOVERABLE_PULSE_MS) / RECOVERABLE_PULSE_MS;
        let pulse = (pulse_phase * std::f64::consts::PI * 2.0).sin() / 2.0 + 0.5; // 0..1..0
        let amplitude = IDLE_AMPLITUDE * (0.6 + 0.4 * pulse);
        return RibbonModel {
            strands: vec![make_strand(
                StrandRole::Voice,
                amplitude,
                input.elapsed_ms,
                VOICE_PHASE,
                0.0,
                points,
                0.85,
                BASE_SPEED_SCALE,
                true,
            )],
            dots: None,
            convergence: None,
            brightness_boost: 0.0,
            tint: Some(RibbonTint::Amber),
            elapsed_ms: input.elapsed_ms,
        };
    }

    let mut env = clamp01(input.envelope);
    let mut brightness_boost = 0.0;
    let mut dots: Option<Vec<Dot>> = None;
    let mut convergence: Option<Convergence> = None;

    match input.phase {
        RibbonPhase::Unfold => {
            env *= unfold_progress(input.phase_elapsed_ms);
        }
        RibbonPhase::Morph => {
            let p = morph_progress(input.phase_elapsed_ms);
            env *= 1.0 - p;
            // Crossfade in 3 travelling dots as the wave fades out.
            dots = Some(
                [0.0, 1.0, 2.0]
                    .iter()
                    .map(|&i| {
                        let raw = input.elapsed_ms * FLOW_SPEED * 40.0 + i * 0.33;
                        let t = ((raw % 1.0) + 1.0) % 1.0;
                        Dot { x: t, alpha: p }
                    })
                    .collect(),
            );
        }
        RibbonPhase::Complete => {
            let p = complete_progress(input.phase_elapsed_ms);
            env = 0.0;
            brightness_boost = (clamp01(p) * std::f64::consts::PI).sin(); // rises then falls, 0→1→0
                                                                          // The convergence point's own alpha MUST follow the same fade —
                                                                          // otherwise it lingers at full opacity for as long as the phase
                                                                          // itself remains 'complete', which can be indefinitely (a bug
                                                                          // found via live testing, 2026-07-30: the dot stayed visible
                                                                          // until the next session started, since nothing else resets the
                                                                          // phase away from 'complete' in the meantime).
            convergence = Some(Convergence {
                x: 0.5,
                y: 0.0,
                alpha: brightness_boost,
            });
        }
        RibbonPhase::Flow => {
            // Steady-state: amplitude tracks the smoothed envelope (through
            // shape_amplitude's response curve, below).
        }
    }

    let voice_amplitude = IDLE_AMPLITUDE.max(shape_amplitude(env));
    let mut layers = vec![make_strand(
        StrandRole::Voice,
        voice_amplitude,
        input.elapsed_ms,
        VOICE_PHASE,
        0.0,
        points,
        0.95,
        1.0,
        true,
    )];

    if strands >= 2 {
        layers.push(make_strand(
            StrandRole::Secondary,
            voice_amplitude * SECONDARY_AMPLITUDE_SCALE,
            input.elapsed_ms,
            SECONDARY_PHASE,
            SECONDARY_DELAY_MS,
            points,
            SECONDARY_ALPHA,
            1.0,
            false,
        ));
    }
    if strands >= 3 {
        layers.push(make_strand(
            StrandRole::Base,
            BASE_AMPLITUDE,
            input.elapsed_ms,
            BASE_PHASE,
            0.0,
            points,
            BASE_ALPHA,
            BASE_SPEED_SCALE,
            false,
        ));
    }
    // Strand counts beyond 3 (up to the design's "three to five") add extra
    // secondary-like depth strands, cycling the same fixed offsets at
    // slightly different scales — still all driven by the one shared
    // envelope, never independent state.
    for extra in 3..strands {
        let scale = SECONDARY_AMPLITUDE_SCALE * (1.0 - (extra as f64 - 2.0) * 0.15);
        layers.push(make_strand(
            StrandRole::Secondary,
            voice_amplitude * 0.2_f64.max(scale),
            input.elapsed_ms,
            SECONDARY_PHASE + extra as f64 * 0.9,
            SECONDARY_DELAY_MS * extra as f64,
            points,
            SECONDARY_ALPHA * 0.4_f64.max(1.0 - (extra as f64 - 2.0) * 0.2),
            1.0,
            false,
        ));
    }

    // `elapsed_ms` is echoed through so renderers can add their own
    // time-based rendering-only effects without needing new geometry fields
    // on the strands themselves — additive, doesn't change the strand
    // contract X24 tests already cover.
    RibbonModel {
        strands: layers,
        dots,
        convergence,
        brightness_boost,
        tint: None,
        elapsed_ms: input.elapsed_ms,
    }
}
