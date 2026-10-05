// tests/ribbon.rs — hermetic contract test for the pure wave-ribbon logic
// (feature 004, 2026-07-30 wave-ribbon redesign + the "fabric in gentle
// airflow" refinement; contract extension.md RC24). No Shell, no D-Bus.
//
// The headless Cairo `paintRibbon` smoke check is not here — the Cairo
// painter is deliberately not ported (GPU-only rendering, R23); the
// equivalent lands as the env-gated EGL render check over the shader.

use myna_hud::ribbon::{
    complete_progress, compute_ribbon_model, morph_progress, shape_amplitude, unfold_progress,
    RibbonInput, RibbonPhase, RibbonTint, StrandRole, AMPLITUDE_CURVE_K, COMPLETE_MS,
    DEFAULT_POINTS_PER_STRAND, DEFAULT_STRAND_COUNT, MORPH_MS, UNFOLD_MS,
};
use myna_hud::states::Severity;

fn input(envelope: f64, elapsed_ms: f64) -> RibbonInput {
    RibbonInput {
        envelope,
        elapsed_ms,
        ..Default::default()
    }
}

fn voice_strand(model: &myna_hud::ribbon::RibbonModel) -> &myna_hud::ribbon::Strand {
    model
        .strands
        .iter()
        .find(|s| s.role == StrandRole::Voice)
        .expect("voice strand")
}

fn max_amplitude(strand: &myna_hud::ribbon::Strand) -> f64 {
    strand.points.iter().map(|p| p.y.abs()).fold(0.0, f64::max)
}

// --- amplitude response curve ("log scale" follow-up) ---------------------

#[test]
fn shape_amplitude_curve() {
    assert_eq!(shape_amplitude(0.0), 0.0, "boundary-preserving at 0");
    assert_eq!(
        shape_amplitude(1.0),
        1.0,
        "same ceiling as before — never reopens the crop fix"
    );
    assert!(shape_amplitude(0.1) > 0.1, "low energy boosted above raw");
    assert!(shape_amplitude(0.5) > 0.5, "mid energy boosted above raw");

    let samples = [0.0, 0.05, 0.1, 0.25, 0.5, 0.75, 0.9, 1.0];
    assert!(
        samples
            .windows(2)
            .all(|w| shape_amplitude(w[1]) > shape_amplitude(w[0])),
        "strictly monotonic increasing"
    );

    assert_eq!(shape_amplitude(-3.0), 0.0, "negative clamps to 0");
    assert_eq!(shape_amplitude(5.0), 1.0, "above-1 clamps to 1");
    assert_eq!(shape_amplitude(f64::NAN), 0.0, "NaN is safe");
    assert!(
        AMPLITUDE_CURVE_K > 0.0 && AMPLITUDE_CURVE_K.is_finite(),
        "AMPLITUDE_CURVE_K is a positive, finite tunable"
    );
}

// `computeRibbonModel` uses the shaped amplitude, not the raw envelope —
// confirmed via the rendered voice strand's own peak.
#[test]
fn model_uses_shaped_amplitude() {
    let envelope = 0.2;
    let model = compute_ribbon_model(input(envelope, 0.0));
    let observed = max_amplitude(voice_strand(&model));
    let expected = shape_amplitude(envelope);
    assert!(
        (observed - expected).abs() < 0.01,
        "voice amplitude {observed:.4} reflects the shaped envelope {expected:.4}, not the raw {envelope}"
    );
    assert!(
        observed > envelope,
        "the shaped amplitude is a real boost over the raw envelope"
    );
}

// --- RC24: layered strands (base/voice/secondary), deterministic -----------

#[test]
fn x24_layered_strands_deterministic() {
    let a = compute_ribbon_model(input(0.6, 1234.0));
    let b = compute_ribbon_model(input(0.6, 1234.0));
    assert_eq!(
        a.strands.len(),
        DEFAULT_STRAND_COUNT,
        "default strand count"
    );
    assert!(a.strands.iter().any(|s| s.role == StrandRole::Voice));
    assert!(a.strands.iter().any(|s| s.role == StrandRole::Secondary));
    assert!(a.strands.iter().any(|s| s.role == StrandRole::Base));
    assert_eq!(
        voice_strand(&a).points.len(),
        DEFAULT_POINTS_PER_STRAND,
        "point count"
    );
    let points = voice_strand(&a).points.len();
    assert!(
        (12..=20).contains(&points),
        "point count in the 12-20 design range"
    );
    assert_eq!(
        a.strands, b.strands,
        "deterministic: identical inputs → identical strands"
    );

    let c = compute_ribbon_model(input(0.6, 9999.0));
    assert_ne!(
        a.strands, c.strands,
        "different elapsed time → different points (it flows)"
    );
    let base = a
        .strands
        .iter()
        .find(|s| s.role == StrandRole::Base)
        .expect("base strand");
    assert_ne!(
        voice_strand(&a).points,
        base.points,
        "strands differ from each other (per-strand phase/amplitude offsets)"
    );

    // 3-5 strand range from the design doc.
    let five = compute_ribbon_model(RibbonInput {
        envelope: 0.6,
        elapsed_ms: 1234.0,
        strand_count: 5,
        ..Default::default()
    });
    assert_eq!(
        five.strands.len(),
        5,
        "strandCount=5 honoured (three to five)"
    );
}

// --- Base strand stays "alive" nearly independent of the voice -----------

#[test]
fn base_strand_stays_alive() {
    let silent = compute_ribbon_model(input(0.0, 500.0));
    let base = silent
        .strands
        .iter()
        .find(|s| s.role == StrandRole::Base)
        .expect("base strand");
    assert!(
        max_amplitude(base) > 0.0,
        "base keeps amplitude even at zero envelope (never reads as off)"
    );
}

// --- Crest highlighting: the voice strand reports per-point crest --------

#[test]
fn crest_factors() {
    let loud = compute_ribbon_model(input(0.9, 100.0));
    let voice = voice_strand(&loud);
    assert_eq!(voice.crest.len(), voice.points.len(), "crest per point");
    assert!(voice.crest.iter().all(|c| (0.0..=1.0).contains(c)));
    assert!(
        voice.crest.iter().any(|c| *c > 0.9),
        "at least one point near the crest"
    );
}

// --- RC24: the lifecycle-phase timing functions are pure & independent ----

#[test]
fn phase_timing_functions() {
    assert_eq!(unfold_progress(0.0), 0.0);
    assert_eq!(unfold_progress(UNFOLD_MS), 1.0, "rises from 0 toward 1");
    assert!(
        (150.0..=200.0).contains(&UNFOLD_MS),
        "unfold duration range"
    );
    assert!((200.0..=250.0).contains(&MORPH_MS), "morph duration range");
    assert!(
        (300.0..=500.0).contains(&COMPLETE_MS),
        "complete duration range"
    );
    assert_eq!(morph_progress(100.0), morph_progress(100.0), "pure");
    assert_eq!(complete_progress(0.0), 0.0);
    assert_eq!(complete_progress(10000.0), 1.0);
}

// --- Phase-driven behavior (FR-010a) --------------------------------------

#[test]
fn phase_driven_behavior() {
    let unfolding = compute_ribbon_model(RibbonInput {
        envelope: 0.8,
        elapsed_ms: 0.0,
        phase: RibbonPhase::Unfold,
        phase_elapsed_ms: 0.0,
        ..Default::default()
    });
    let unfolded = compute_ribbon_model(RibbonInput {
        envelope: 0.8,
        elapsed_ms: 0.0,
        phase: RibbonPhase::Unfold,
        phase_elapsed_ms: UNFOLD_MS,
        ..Default::default()
    });
    assert!(
        max_amplitude(voice_strand(&unfolding)) <= max_amplitude(voice_strand(&unfolded)),
        "unfold starts near-flat and grows to full amplitude"
    );
}

// --- Morph → travelling dots ----------------------------------------------

#[test]
fn morph_travelling_dots() {
    let morphing = compute_ribbon_model(RibbonInput {
        envelope: 0.9,
        elapsed_ms: 500.0,
        phase: RibbonPhase::Morph,
        phase_elapsed_ms: MORPH_MS,
        ..Default::default()
    });
    let dots = morphing
        .dots
        .as_ref()
        .expect("morph produces travelling dots");
    assert_eq!(dots.len(), 3);
    assert!(
        dots.iter()
            .all(|d| (0.0..=1.0).contains(&d.x) && (0.0..=1.0).contains(&d.alpha)),
        "dots have valid x/alpha"
    );
    let morph_start = compute_ribbon_model(RibbonInput {
        envelope: 0.9,
        elapsed_ms: 500.0,
        phase: RibbonPhase::Morph,
        phase_elapsed_ms: 0.0,
        ..Default::default()
    });
    assert!(
        max_amplitude(voice_strand(&morph_start)) >= max_amplitude(voice_strand(&morphing)),
        "the wave fades out as morph progresses"
    );
}

// --- Complete → convergence + brightness pulse (FR-010d) -----------------

#[test]
fn complete_convergence_pulse() {
    let completing = compute_ribbon_model(RibbonInput {
        envelope: 0.9,
        elapsed_ms: 0.0,
        phase: RibbonPhase::Complete,
        phase_elapsed_ms: 100.0,
        ..Default::default()
    });
    assert!(
        completing.convergence.is_some(),
        "completion produces a convergence point"
    );
    assert!(
        completing.brightness_boost > 0.0,
        "completion is a brightness boost, not a blocking state"
    );
    let complete_end = compute_ribbon_model(RibbonInput {
        envelope: 0.9,
        elapsed_ms: 0.0,
        phase: RibbonPhase::Complete,
        phase_elapsed_ms: COMPLETE_MS,
        ..Default::default()
    });
    assert!(
        complete_end.brightness_boost < 0.1,
        "brightness pulse rises then falls, never stays pinned at max"
    );
    // Regression (2026-07-30, found via live testing): the convergence dot's
    // own alpha MUST follow the brightness pulse's fade — it used to be
    // hardcoded to 1 and lingered at full opacity indefinitely.
    assert!(
        complete_end.convergence.as_ref().unwrap().alpha < 0.1,
        "the convergence dot fades out with the brightness pulse"
    );
    assert!(
        completing.convergence.as_ref().unwrap().alpha > 0.5,
        "the convergence dot is visible mid-pulse, not just a flash at t=0"
    );
    assert_eq!(
        completing.convergence.as_ref().unwrap().alpha,
        completing.brightness_boost,
        "convergence alpha tracks brightnessBoost exactly (same fade curve)"
    );
}

// --- FR-022a: reduced motion returns a static, non-flowing model ----------

#[test]
fn reduced_motion_static() {
    let reduced1 = compute_ribbon_model(RibbonInput {
        envelope: 0.5,
        elapsed_ms: 100.0,
        reduced_motion: true,
        ..Default::default()
    });
    let reduced2 = compute_ribbon_model(RibbonInput {
        envelope: 0.5,
        elapsed_ms: 999999.0,
        reduced_motion: true,
        ..Default::default()
    });
    assert_eq!(reduced1.strands.len(), 1, "single strand");
    assert_eq!(
        reduced1.strands, reduced2.strands,
        "time-independent (no flow)"
    );
}

// --- R17a: recoverable severity → amber, paused, pulsing ------------------

#[test]
fn recoverable_amber_pulse() {
    let recoverable = compute_ribbon_model(RibbonInput {
        envelope: 0.9,
        elapsed_ms: 300.0,
        severity_tint: Some(Severity::Recoverable),
        ..Default::default()
    });
    assert_eq!(recoverable.tint, Some(RibbonTint::Amber), "tints amber");
    assert!(
        max_amplitude(voice_strand(&recoverable)) < 0.15,
        "ignores the loud envelope (motion pauses)"
    );
    let t1 = compute_ribbon_model(RibbonInput {
        envelope: 0.9,
        elapsed_ms: 0.0,
        severity_tint: Some(Severity::Recoverable),
        ..Default::default()
    });
    let t2 = compute_ribbon_model(RibbonInput {
        envelope: 0.9,
        elapsed_ms: 900.0,
        severity_tint: Some(Severity::Recoverable),
        ..Default::default()
    });
    assert_ne!(
        max_amplitude(voice_strand(&t1)),
        max_amplitude(voice_strand(&t2)),
        "still gently pulses (not perfectly frozen)"
    );
    assert_eq!(
        compute_ribbon_model(input(0.5, 0.0)).tint,
        None,
        "normal (non-severity) model has no tint"
    );
}

// --- X6 (delegated): no content in outputs, numeric points only ----------

#[test]
fn x6_outputs_are_numeric_points_only() {
    let model = compute_ribbon_model(input(0.4, 50.0));
    assert!(
        model
            .strands
            .iter()
            .flat_map(|s| s.points.iter())
            .all(|p| p.x.is_finite() && p.y.is_finite()),
        "outputs are plain numeric points, no content"
    );
}
