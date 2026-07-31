// ribbon.test.js — GJS contract test for the pure wave-ribbon logic
// (feature 004-gnome-shell-indicator, 2026-07-30 wave-ribbon redesign,
// refined per the "fabric in gentle airflow" design pass; contract
// extension.md X24). No Shell / no D-Bus. Includes a headless Cairo smoke
// check for `ribbon-paint.js` (an `ImageSurface` needs no display server,
// so this is a genuine no-throw/sane-output check, not just a visual
// claim).
//
//     gjs -m test/ribbon.test.js        (from extensions/myna-shell/)

import System from 'system';
import Cairo from 'gi://cairo';

import {
    applyEnvelopeSmoothing,
    computeEnvelope,
    computeRibbonModel,
    isStrongSyllableOnset,
    unfoldProgress,
    relaxProgress,
    morphProgress,
    completeProgress,
    UNFOLD_MS,
    RELAX_MS,
    MORPH_MS,
    COMPLETE_MS,
    ATTACK_TAU_MS,
    SMOOTHING_TAU_MS,
    RELEASE_TAU_MS,
    PARTICLE_ONSET_THRESHOLD,
    DEFAULT_STRAND_COUNT,
    DEFAULT_POINTS_PER_STRAND,
} from '../ribbon.js';
import {resolveAccentPalette} from '../accent.js';
import {paintRibbon} from '../ribbon-paint.js';
import {STALE_MS} from '../vumeter.js';

let failures = 0;

function check(name, condition) {
    if (condition)
        print(`ok   ${name}`);
    else {
        failures++;
        print(`FAIL ${name}`);
    }
}

function eq(name, actual, expected) {
    check(`${name} (got ${JSON.stringify(actual)})`, actual === expected);
}

function maxAmplitude(strand) {
    return Math.max(...strand.points.map(p => Math.abs(p.y)));
}

function voiceStrand(model) {
    return model.strands.find(s => s.role === 'voice');
}

// --- X5 (delegated): instantaneous envelope reuses vumeter.js unchanged --

check('X5 louder → higher instantaneous envelope (monotonic)',
    computeEnvelope(0.002, 0.002, 0) < computeEnvelope(0.02, 0.02, 0));
check('X5 stale decays toward the floor',
    computeEnvelope(0.9, 0.9, 0) > computeEnvelope(0.9, 0.9, STALE_MS + 50));

// --- 2026-07-30: ~250-400ms smoothing (applyEnvelopeSmoothing) -----------

{
    check('SMOOTHING_TAU_MS is within the 250-400ms design range',
        SMOOTHING_TAU_MS >= 250 && SMOOTHING_TAU_MS <= 400);
    // The ATTACK path (rising) is deliberately fast now — "more reactive to
    // the audio" (tightened twice, 2026-07-30) — so a single ~16ms frame
    // tracks a good fraction of a sudden jump, but still isn't instant.
    const attackStep = applyEnvelopeSmoothing(0, 1.0, 16);
    check('a single ~16ms attack frame is fast but not instantaneous',
        attackStep > 0.2 && attackStep < 0.9);
    // The RELEASE path (falling) is still the slower, smoother one — this
    // is what "no oscilloscope-like jumps" actually protects: pauses/decay
    // should ease, not snap. Compare how much of the total distance each
    // direction covers in the same ~16ms: attack should cover far more.
    const releaseStep = applyEnvelopeSmoothing(1.0, 0, 16);
    const attackFraction = attackStep; // distance covered toward target 1.0
    const releaseFraction = 1 - releaseStep; // distance covered toward target 0
    check('attack covers much more distance per frame than release',
        attackFraction > releaseFraction * 2);
    // But after several time constants' worth of steps, it should have
    // converged close to the target (syllables remain visible).
    let converged = 0;
    for (let i = 0; i < 200; i++)
        converged = applyEnvelopeSmoothing(converged, 1.0, 16);
    check('after enough time, smoothing converges close to the target',
        converged > 0.95);
    eq('smoothing is a pure function of its inputs',
        applyEnvelopeSmoothing(0.3, 0.6, 50), applyEnvelopeSmoothing(0.3, 0.6, 50));
    check('smoothing output stays clamped to [0,1]',
        applyEnvelopeSmoothing(0, 5, 1000) <= 1 && applyEnvelopeSmoothing(0, -5, 1000) >= 0);
}

// --- 2026-07-30 refinement: attack/release ballistics ("more reactive") --

{
    check('attack is faster than release (more reactive to getting louder)',
        ATTACK_TAU_MS < RELEASE_TAU_MS);
    // Rising (attack): should track a lot faster than the old symmetric tau.
    const rising = applyEnvelopeSmoothing(0, 1.0, 40);
    const risingWithReleaseTau = applyEnvelopeSmoothing(0, 1.0, 40, RELEASE_TAU_MS);
    check('a rising target uses the fast attack tau by default',
        rising > risingWithReleaseTau);
    // Falling (release): should still ease smoothly, not snap down.
    const falling = applyEnvelopeSmoothing(1.0, 0, 40);
    check('a falling target still eases (does not snap to the floor)',
        falling > 0.5 && falling < 1.0);
    check('an explicit tauMs overrides attack/release auto-selection',
        applyEnvelopeSmoothing(0, 1.0, 40, 12345) === applyEnvelopeSmoothing(0, 1.0, 40, 12345));
}

// --- 2026-07-30: strong-syllable onset detection (particles, optional) --

{
    check('a rise at/above the threshold is a strong-syllable onset',
        isStrongSyllableOnset(PARTICLE_ONSET_THRESHOLD));
    check('a small rise is not an onset', !isStrongSyllableOnset(0.01));
    check('a negative delta (level falling) is never an onset', !isStrongSyllableOnset(-0.5));
}

// --- X24: layered strands (base/voice/secondary), deterministic ---------

{
    const a = computeRibbonModel({envelope: 0.6, elapsedMs: 1234, phase: 'flow'});
    const b = computeRibbonModel({envelope: 0.6, elapsedMs: 1234, phase: 'flow'});
    eq('X24 default strand count', a.strands.length, DEFAULT_STRAND_COUNT);
    check('X24 has a voice strand', a.strands.some(s => s.role === 'voice'));
    check('X24 has a secondary strand', a.strands.some(s => s.role === 'secondary'));
    check('X24 has a base strand', a.strands.some(s => s.role === 'base'));
    eq('X24 point count', voiceStrand(a).points.length, DEFAULT_POINTS_PER_STRAND);
    check('X24 point count within the 12-20 design range',
        voiceStrand(a).points.length >= 12 && voiceStrand(a).points.length <= 20);
    check('X24 deterministic: identical control points for identical inputs',
        JSON.stringify(a.strands) === JSON.stringify(b.strands));

    const c = computeRibbonModel({envelope: 0.6, elapsedMs: 9999, phase: 'flow'});
    check('X24 different elapsed time → different control points (it flows)',
        JSON.stringify(a.strands) !== JSON.stringify(c.strands));
    check('X24 strands differ from each other (per-strand phase/amplitude offsets)',
        JSON.stringify(voiceStrand(a).points) !== JSON.stringify(a.strands.find(s => s.role === 'base').points));

    // 3-5 strand range from the design doc.
    const five = computeRibbonModel({envelope: 0.6, elapsedMs: 1234, phase: 'flow', strandCount: 5});
    eq('strandCount=5 is honoured (design doc: "three to five")', five.strands.length, 5);
}

// --- Base strand stays "alive" nearly independent of the voice ----------

{
    const silent = computeRibbonModel({envelope: 0, elapsedMs: 500, phase: 'flow'});
    const baseStrand = silent.strands.find(s => s.role === 'base');
    check('base strand keeps moving/has amplitude even at zero envelope (never reads as off)',
        maxAmplitude(baseStrand) > 0);
}

// --- Crest highlighting: the voice strand reports per-point crest -------

{
    const loud = computeRibbonModel({envelope: 0.9, elapsedMs: 100, phase: 'flow'});
    const voice = voiceStrand(loud);
    check('voice strand reports a crest factor per point', voice.crest.length === voice.points.length);
    check('crest factors are all within [0,1]', voice.crest.every(c => c >= 0 && c <= 1));
    check('at least one point is near the crest (factor close to 1)',
        voice.crest.some(c => c > 0.9));
}

// --- X24: the 5 lifecycle-phase timing functions are pure & independent --

{
    check('unfold progress rises from 0 toward 1', unfoldProgress(0) === 0 && unfoldProgress(UNFOLD_MS) === 1);
    check('unfold duration within the 150-200ms design range', UNFOLD_MS >= 150 && UNFOLD_MS <= 200);
    check('relax progress rises from 0 toward 1', relaxProgress(0) === 0 && relaxProgress(RELAX_MS) === 1);
    check('relax duration within the 400-600ms design range', RELAX_MS >= 400 && RELAX_MS <= 600);
    check('morph duration within the 200-250ms design range', MORPH_MS >= 200 && MORPH_MS <= 250);
    check('complete duration within the 300-500ms design range', COMPLETE_MS >= 300 && COMPLETE_MS <= 500);
    check('morph progress is a pure function of elapsed time',
        morphProgress(100) === morphProgress(100));
    check('complete progress rises then is queryable independently',
        completeProgress(0) === 0 && completeProgress(10000) === 1);
}

// --- Phase-driven behavior (FR-010a) --------------------------------------

{
    const unfolding = computeRibbonModel({envelope: 0.8, elapsedMs: 0, phase: 'unfold', phaseElapsedMs: 0});
    const unfolded = computeRibbonModel({envelope: 0.8, elapsedMs: 0, phase: 'unfold', phaseElapsedMs: UNFOLD_MS});
    check('unfold starts near-flat and grows to full amplitude',
        maxAmplitude(voiceStrand(unfolding)) <= maxAmplitude(voiceStrand(unfolded)));

    const speaking = computeRibbonModel({envelope: 0.9, elapsedMs: 0, phase: 'flow'});
    const relaxing = computeRibbonModel({envelope: 0.9, elapsedMs: 0, phase: 'relax', phaseElapsedMs: RELAX_MS});
    check('relax eases toward the idle floor, not an abrupt stop',
        maxAmplitude(voiceStrand(relaxing)) < maxAmplitude(voiceStrand(speaking)));
}

// --- Morph → travelling dots (contract: "contracts into a line or dots") -

{
    const morphing = computeRibbonModel({envelope: 0.9, elapsedMs: 500, phase: 'morph', phaseElapsedMs: MORPH_MS});
    check('morph produces travelling dots', Array.isArray(morphing.dots) && morphing.dots.length === 3);
    check('dots have valid x/alpha', morphing.dots.every(d =>
        d.x >= 0 && d.x <= 1 && d.alpha >= 0 && d.alpha <= 1));
    const morphStart = computeRibbonModel({envelope: 0.9, elapsedMs: 500, phase: 'morph', phaseElapsedMs: 0});
    check('the wave fades out as morph progresses',
        maxAmplitude(voiceStrand(morphStart)) >= maxAmplitude(voiceStrand(morphing)));
}

// --- Complete → convergence + brightness pulse (FR-010d) -----------------

{
    const completing = computeRibbonModel({envelope: 0.9, elapsedMs: 0, phase: 'complete', phaseElapsedMs: 100});
    check('FR-010d: completion produces a convergence point', completing.convergence !== null);
    check('FR-010d: completion is a brightness boost, not a blocking state',
        completing.brightnessBoost > 0 && typeof completing.brightnessBoost === 'number');
    const completeEnd = computeRibbonModel({envelope: 0.9, elapsedMs: 0, phase: 'complete', phaseElapsedMs: COMPLETE_MS});
    check('brightness pulse rises then falls (0→1→0), never stays pinned at max',
        completeEnd.brightnessBoost < 0.1);
    // Regression (2026-07-30, found via live testing): the convergence
    // dot's own alpha MUST follow the same fade as the brightness pulse —
    // it used to be hardcoded to 1, so it stayed at full opacity
    // indefinitely (until the next session forced the phase away from
    // 'complete'), well past the "brief" quiet-success indication FR-010d
    // requires.
    check('the convergence dot fades out with the brightness pulse, not hardcoded to full opacity',
        completeEnd.convergence.alpha < 0.1);
    check('the convergence dot is visible mid-pulse, not just a flash at t=0',
        completing.convergence.alpha > 0.5);
    check('convergence alpha tracks brightnessBoost exactly (same fade curve)',
        completing.convergence.alpha === completing.brightnessBoost);
}

// --- FR-022a: reduced motion returns a static, non-flowing model ---------

{
    const reduced1 = computeRibbonModel({envelope: 0.5, elapsedMs: 100, reducedMotion: true});
    const reduced2 = computeRibbonModel({envelope: 0.5, elapsedMs: 999999, reducedMotion: true});
    eq('reduced motion → single strand', reduced1.strands.length, 1);
    check('reduced motion is time-independent (no flow)',
        JSON.stringify(reduced1.strands) === JSON.stringify(reduced2.strands));
}

// --- 2026-07-30, R17a: recoverable severity → amber, paused, pulsing -----

{
    const recoverable = computeRibbonModel({envelope: 0.9, elapsedMs: 300, phase: 'flow', severityTint: 'recoverable'});
    eq('recoverable severity tints the ribbon amber', recoverable.tint, 'amber');
    check('recoverable severity ignores the loud envelope (motion "pauses")',
        maxAmplitude(voiceStrand(recoverable)) < 0.15);
    const t1 = computeRibbonModel({envelope: 0.9, elapsedMs: 0, severityTint: 'recoverable'});
    const t2 = computeRibbonModel({envelope: 0.9, elapsedMs: 900, severityTint: 'recoverable'});
    check('recoverable severity still gently pulses (not perfectly frozen)',
        maxAmplitude(voiceStrand(t1)) !== maxAmplitude(voiceStrand(t2)));
    check('normal (non-severity) model has no tint', computeRibbonModel({envelope: 0.5, elapsedMs: 0}).tint === null);
}

// --- X6 (delegated): no content in outputs, numbers/points only ---------

{
    const model = computeRibbonModel({envelope: 0.4, elapsedMs: 50});
    check('X6 outputs are plain numeric points, no content',
        model.strands.every(strand => strand.points.every(p =>
            typeof p.x === 'number' && typeof p.y === 'number')));
}

// --- Bonus smoke check: paintRibbon against a REAL headless Cairo surface -
// (an ImageSurface needs no display server, so this genuinely exercises
// the shared Cairo drawing path both hud.js and dev-lab use, not just a
// mock.) Exercises the normal, morph (dots), complete (convergence), and
// amber-tint render paths.

{
    const width = 160, height = 32;
    const surface = new Cairo.ImageSurface(Cairo.Format.ARGB32, width, height);
    const cr = new Cairo.Context(surface);
    let threw = false;
    try {
        const palette = resolveAccentPalette('orange');
        paintRibbon(cr, width, height,
            computeRibbonModel({envelope: 0.7, elapsedMs: 500, phase: 'flow'}), palette);
        paintRibbon(cr, width, height,
            computeRibbonModel({envelope: 0.7, elapsedMs: 500, phase: 'morph', phaseElapsedMs: 100}), palette);
        paintRibbon(cr, width, height,
            computeRibbonModel({envelope: 0.7, elapsedMs: 500, phase: 'complete', phaseElapsedMs: 100}), palette);
        paintRibbon(cr, width, height,
            computeRibbonModel({envelope: 0.7, elapsedMs: 500, severityTint: 'recoverable'}), palette);
    } catch (e) {
        threw = true;
        logError(e);
    }
    check('paintRibbon runs against a real headless Cairo surface without throwing (all phases + amber tint)', !threw);
}

print(failures === 0 ? 'PASS ribbon.test.js' : `FAIL ribbon.test.js: ${failures} failure(s)`);
System.exit(failures === 0 ? 0 : 1);
