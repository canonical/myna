// ribbonPaint.js — PURE Cairo drawing for the wave ribbon (feature
// 004-gnome-shell-indicator, 2026-07-30 wave-ribbon redesign; visual
// passes matching a reference mockup — a dense, glowing ribbon with soft,
// wispy, "trailing smoke"-like edges rather than a crisp gradient-filled
// band). Takes a Cairo context + geometry + an already-resolved model
// (from ribbon.js) and palette (from accent.js) and paints — nothing else.
// No `St`/`Clutter`/`Gtk` import, so this exact function runs unmodified
// inside both `hud.js`'s `WaveRibbonActor` and the standalone `dev-lab`
// tool.

import Cairo from 'gi://cairo';

import {RibbonTint, StrandRole} from './ribbon.js';

export const AMBER_MAIN = '#F5A623';
export const AMBER_HIGHLIGHT = '#FFE0A6';

// Ceilings the paint functions below actually reach at full loudness/
// activity — referenced by `computeSafeScale` so the overflow guard stays
// honest if the body's billow is ever retuned (2026-07-31, cropping-bug
// fix, narrowed same day — see `computeSafeScale`'s doc comment for why
// only the opaque contributors are in this budget).
export const VOICE_THICKNESS_FRACTION = 0.85; // paintRibbon's own per-role thickness
export const BASE_CENTRELINE_FRACTION = 0.82; // paintRibbon's un-clamped verticalScale factor
export const MAX_BODY_BILLOW = 1 + (0.12 + 0.32 * 1); // bodyThickness()'s ceiling (activity=1)
export const WISP_THICKNESS_FRACTION = 0.5; // paintRibbon's own wispThickness (scales with safeScale, not part of its budget)

/** Per-role body thickness, as a fraction of the canvas height. */
export const ROLE_THICKNESS_FRACTION = Object.freeze({
    [StrandRole.VOICE]: VOICE_THICKNESS_FRACTION,
    [StrandRole.SECONDARY]: 0.4,
    [StrandRole.BASE]: 0.32,
});

/** Per-role extra alpha multiplier (the base strand reads as soft haze). */
export const ROLE_ALPHA_SCALE = Object.freeze({
    [StrandRole.VOICE]: 1,
    [StrandRole.SECONDARY]: 1,
    [StrandRole.BASE]: 0.5,
});

/**
 * The left→right ribbon gradient: shifts tone (shadow → main → highlight →
 * main → shadow) while fading alpha in/out at the ends. `tone` indexes the
 * resolved palette rather than naming a colour, so the amber-tint and
 * accent-coloured paths reuse the identical stop positions.
 */
export const RIBBON_GRADIENT_STOPS = Object.freeze([
    Object.freeze({pos: 0.0, tone: 'shadow', alpha: 0}),
    Object.freeze({pos: 0.08, tone: 'shadow', alpha: 0.45}),
    Object.freeze({pos: 0.32, tone: 'main', alpha: 0.9}),
    Object.freeze({pos: 0.52, tone: 'highlight', alpha: 0.95}),
    Object.freeze({pos: 0.72, tone: 'main', alpha: 0.85}),
    Object.freeze({pos: 0.92, tone: 'shadow', alpha: 0.35}),
    Object.freeze({pos: 1.0, tone: 'shadow', alpha: 0}),
]);

/**
 * The glow passes. Cairo fakes a bloom by stacking this many progressively
 * wider, fainter round strokes ("Cairo has no native blur"); the shader
 * evaluates the same table as that many summed Gaussians, which is what
 * the stack was approximating in the first place.
 */
export const GLOW_PASSES = Object.freeze([
    Object.freeze({scale: 4.6, alphaScale: 0.10}),
    Object.freeze({scale: 2.8, alphaScale: 0.17}),
    Object.freeze({scale: 1.6, alphaScale: 0.24}),
]);

/** The body-edge feathering passes (same idea, applied to each edge curve). */
export const FEATHER_PASSES = Object.freeze([
    Object.freeze({scale: 2.2, alphaScale: 0.12}),
    Object.freeze({scale: 1.3, alphaScale: 0.18}),
]);

/** The two wisp tendrils curling off the voice strand. */
export const WISP_TENDRILS = Object.freeze([
    Object.freeze({seed: 0.7, alpha: 0.5, timeOffsetMs: 0, mix: 0.4, fromShadow: false}),
    Object.freeze({seed: 1.9, alpha: 0.35, timeOffsetMs: 240, mix: 0.5, fromShadow: true}),
]);

/** The wisp's own left→right alpha gradient (fades in and out along x). */
export const WISP_GRADIENT_STOPS = Object.freeze([
    Object.freeze({pos: 0.0, alpha: 0}),
    Object.freeze({pos: 0.25, alpha: 0.5}),
    Object.freeze({pos: 0.55, alpha: 0.32}),
    Object.freeze({pos: 1.0, alpha: 0}),
]);

/** The wisp tendril's curl magnitude, drift wave and stroke width. */
export const WISP = Object.freeze({
    curlMin: 0.12,
    curlActivity: 1.5,
    tailFloor: 0.25,
    freqBase: 4.4,
    freqSeed: 1.3,
    speedBase: 0.0017,
    speedSeed: 0.0004,
    phaseSeed: 2.1,
    alphaMin: 0.12,
    alphaActivity: 0.88,
    lineWidthFraction: 0.16,
});

/** `bodyThickness`'s billow shape, and the drift wave that produces it. */
export const BILLOW = Object.freeze({
    minAmount: 0.12,
    activityAmount: 0.32,
    freq: 2.3,
    speed: 0.0009,
    phase: 0.6,
    taperFloor: 0.32,
});

/** `edgeTaper`'s raised-cosine in/out widths. */
export const EDGE_TAPER = Object.freeze({inWidth: 0.16, outWidth: 0.3});

/** `activityRamp`'s smoothstep bounds. */
export const ACTIVITY_RAMP = Object.freeze({lo: 0.08, hi: 0.3});

/** The amber palette used when a recoverable notice tints the ribbon. */
export const AMBER_PALETTE = Object.freeze({main: AMBER_MAIN, highlight: AMBER_HIGHLIGHT});

// How much intentional overflow to allow beyond the guaranteed-no-clip
// budget (2026-07-31, "give more room to base/voice" follow-up) — `1` keeps
// the original guarantee (the opaque body never clips, even at the exact
// worst-case billow peak); `>1` relaxes it proportionally, trading some of
// that guarantee for a bigger, more dramatic wave that may occasionally
// graze or clip at extreme, sustained loudness. Capped at `Math.min(1, …)`
// in `computeSafeScale` regardless — this can make the shrink LESS
// aggressive, never inflate the wave past its own raw (unshrunk) size.
export const OVERFLOW_BOOST = 1.3;

function clamp01(x) {
    return Math.max(0, Math.min(1, x));
}

/**
 * The overflow guard (2026-07-31, cropping bug fix; narrowed 2026-07-31
 * after a live-hardware follow-up: "amplitude too low"; relaxed further via
 * `OVERFLOW_BOOST` same day per "let it overflow, even if it's clipped" —
 * more room for the base/voice strands was worth some occasional clipping
 * at extreme loudness). At full loudness the centerline swing and the
 * body's billowed thickness — both **opaque** (~85-95% alpha) — can
 * together reach well past the canvas's available half-height, which
 * Cairo silently clips at the `St.DrawingArea`'s surface edge. With
 * `OVERFLOW_BOOST > 1`, that's now an accepted, deliberate trade-off at the
 * extreme end rather than a guarantee.
 *
 * The glow and wisp passes are, as before, not part of this budget at all
 * (they're low-alpha overlays — glow: 10-24%; wisp: lower still — so their
 * own overflow is even less visible than the body's).
 *
 * Rather than hand-picking a new fixed literal for "how much amplitude
 * feels right" (which is exactly how the original values drifted out of
 * sync with each other and caused the bug), this derives the scale factor
 * from the SAME ceilings `bodyThickness()`'s billow actually reaches, then
 * applies `OVERFLOW_BOOST` as the single, explicit "how much clipping risk
 * am I willing to accept" dial — so retuning the billow formula still keeps
 * this in relative sync, and the overflow policy itself stays a one-line,
 * clearly-named decision rather than baked into the budget's other
 * constants. Returns `1` (no shrink at all) once the (boosted) budget
 * already covers the worst case.
 *
 * @returns {number} a multiplier in `(0, 1]` applied uniformly to the
 *     centerline `verticalScale`, each role's body `thickness`, and
 *     `wispThickness` — never applied unevenly, so the ribbon's relative
 *     proportions (glow vs. body vs. wisp) are preserved, only its overall
 *     scale shrinks (or doesn't).
 */
export function computeSafeScale() {
    const worstCaseExtentFraction =
        BASE_CENTRELINE_FRACTION +
        (VOICE_THICKNESS_FRACTION * MAX_BODY_BILLOW) / 2;
    return Math.min(1, (0.5 * OVERFLOW_BOOST) / worstCaseExtentFraction);
}

function colorToRgbFloat(color) {
    if (typeof color !== 'string') {
        return {
            r: color.red / 255,
            g: color.green / 255,
            b: color.blue / 255,
        };
    }
    const m = /^#?([0-9a-f]{6})$/i.exec(color.trim());
    if (!m)
        return {r: 1, g: 1, b: 1};
    const n = parseInt(m[1], 16);
    return {
        r: ((n >> 16) & 0xff) / 255,
        g: ((n >> 8) & 0xff) / 255,
        b: (n & 0xff) / 255,
    };
}

function mixRgb(a, b, t) {
    const tt = clamp01(t);
    return {r: a.r + (b.r - a.r) * tt, g: a.g + (b.g - a.g) * tt, b: a.b + (b.b - a.b) * tt};
}

function lightenRgb({r, g, b}, amount) {
    const a = clamp01(amount);
    return {r: r + (1 - r) * a, g: g + (1 - g) * a, b: b + (1 - b) * a};
}

function darkenRgb({r, g, b}, amount) {
    const a = clamp01(amount);
    return {r: r * (1 - a), g: g * (1 - a), b: b * (1 - a)};
}

/** A smooth taper in [0,1]: 0 at both ends of the strand, 1 across the
 * middle — "new audio activity appears near the microphone ... before
 * gently dissolving." A raised-cosine shape rather than a linear ramp, so
 * there's no visible kink where the taper begins. */
function edgeTaper(t, inWidth = EDGE_TAPER.inWidth, outWidth = EDGE_TAPER.outWidth) {
    let v = 1;
    if (t < inWidth)
        v = Math.min(v, (1 - Math.cos((t / inWidth) * Math.PI)) / 2);
    if (t > 1 - outWidth)
        v = Math.min(v, (1 - Math.cos(((1 - t) / outWidth) * Math.PI)) / 2);
    return clamp01(v);
}

/** Smoothstep ramp in [0,1]: 0 at/below `lo`, 1 at/above `hi`, smooth in
 * between (no kink) — used to fade the glow/feather/wisp effects in as
 * activity rises, rather than a hard on/off that would visibly "pop" as
 * the level crosses a threshold in real-time animation. */
function activityRamp(activity, lo = ACTIVITY_RAMP.lo, hi = ACTIVITY_RAMP.hi) {
    if (activity <= lo)
        return 0;
    if (activity >= hi)
        return 1;
    const t = (activity - lo) / (hi - lo);
    return t * t * (3 - 2 * t);
}

/** A small, deterministic "drift" wave — used to billow the ribbon's width
 * and to curl wisp tendrils away from the main body, purely a rendering-
 * time effect (never fed back into the model's geometry/tests). */
function driftWave(t, elapsedMs, freq, speed, phase) {
    return Math.sin(t * freq * Math.PI * 2 + elapsedMs * speed + phase);
}

/**
 * Build a smooth Catmull-Rom spline through pixel-space points, appended
 * onto whatever path is already open on `cr` (caller must `moveTo` the
 * first point beforehand, or pass `moveToFirst = true`). Far smoother than
 * a per-point polyline for a small (12-20) control-point count.
 */
function catmullRomTo(cr, points, moveToFirst = true) {
    if (points.length < 2)
        return;
    if (moveToFirst)
        cr.moveTo(points[0].x, points[0].y);
    const at = i => points[Math.max(0, Math.min(points.length - 1, i))];
    for (let i = 0; i < points.length - 1; i++) {
        const p0 = at(i - 1), p1 = at(i), p2 = at(i + 1), p3 = at(i + 2);
        const c1 = {x: p1.x + (p2.x - p0.x) / 6, y: p1.y + (p2.y - p0.y) / 6};
        const c2 = {x: p2.x - (p3.x - p1.x) / 6, y: p2.y - (p3.y - p1.y) / 6};
        cr.curveTo(c1.x, c1.y, c2.x, c2.y, p2.x, p2.y);
    }
}

/** A left→right gradient that both shifts hue (shadow → main → highlight →
 * main → shadow) and fades alpha in/out at the ends. */
function buildRibbonGradient(width, shadowRgb, mainRgb, highlightRgb, alpha) {
    const gradient = new Cairo.LinearGradient(0, 0, width, 0);
    const tones = {shadow: shadowRgb, main: mainRgb, highlight: highlightRgb};
    for (const {pos, tone, alpha: a} of RIBBON_GRADIENT_STOPS) {
        const rgb = tones[tone];
        gradient.addColorStopRGBA(pos, rgb.r, rgb.g, rgb.b, clamp01(a * alpha));
    }
    return gradient;
}

/**
 * The ribbon body's tapered, softly billowing thickness at position `t`
 * (0-1 along the strand) — the raised-cosine edge taper combined with a
 * slow "drift" noise so the width isn't perfectly uniform, more like a
 * wisp of smoke than a ruler-straight band. Both the billow amount and the
 * overall thickness scale down toward `activity=0` (no audio coming
 * through) so the ribbon reads calmer/flatter at idle and fuller/more
 * alive the louder the voice gets — "more reactive to the audio."
 */
function bodyThickness(t, elapsedMs, baseThickness, activity = 1) {
    const billowAmount = BILLOW.minAmount + BILLOW.activityAmount * activity;
    const billow = 1 + billowAmount *
        driftWave(t, elapsedMs, BILLOW.freq, BILLOW.speed, BILLOW.phase);
    const activityScale = 0.5 + 0.5 * activity;
    const taper = BILLOW.taperFloor + (1 - BILLOW.taperFloor) * edgeTaper(t);
    return baseThickness * activityScale * taper * billow;
}

/**
 * Paint one strand as a single, smoothly-filled, tapered, gently billowing
 * ribbon body — one continuous path + gradient, so there are no visible
 * facet seams.
 */
function paintRibbonBody(cr, pixelPoints, baseRgb, highlightRgb, shadowRgb, alpha, thickness, width, elapsedMs, activity) {
    const n = pixelPoints.length;
    if (n < 2)
        return;

    const top = pixelPoints.map((p, i) => {
        const t = i / (n - 1);
        return {x: p.x, y: p.y - bodyThickness(t, elapsedMs, thickness, activity) / 2};
    });
    const bottom = pixelPoints.map((p, i) => {
        const t = i / (n - 1);
        return {x: p.x, y: p.y + bodyThickness(t, elapsedMs, thickness, activity) / 2};
    }).reverse();

    cr.save();
    catmullRomTo(cr, top, true);
    catmullRomTo(cr, bottom, false);
    cr.closePath();
    const gradient = buildRibbonGradient(width, shadowRgb, baseRgb, highlightRgb, alpha);
    cr.setSource(gradient);
    cr.fill();
    cr.restore();
}

/** A cheap glow approximation: several progressively wider, fainter passes
 * of the same smoothed centreline behind the crisp fill — Cairo has no
 * native blur, but a handful of soft, wide strokes reads as bloom at HUD
 * scale. */
function paintGlow(cr, pixelPoints, width, shadowRgb, mainRgb, highlightRgb, alpha, strokeWidth) {
    for (const pass of GLOW_PASSES) {
        cr.save();
        const gradient = buildRibbonGradient(width, shadowRgb, mainRgb, highlightRgb, alpha * pass.alphaScale);
        cr.setSource(gradient);
        cr.setLineWidth(strokeWidth * pass.scale);
        cr.setLineCap(1); // ROUND
        cr.setLineJoin(1); // ROUND
        catmullRomTo(cr, pixelPoints, true);
        cr.stroke();
        cr.restore();
    }
}

/**
 * Soften the ribbon body's own top/bottom boundary — a few translucent,
 * widening strokes traced along each edge curve (not just the centreline)
 * so the boundary itself reads as diffuse/feathered rather than a crisp
 * gradient cutoff, the way smoke's edge dissipates into the air rather
 * than stopping at a hard line.
 */
function paintFeatheredEdges(cr, pixelPoints, thickness, elapsedMs, rgb, alpha, width, activity) {
    const n = pixelPoints.length;
    if (n < 2)
        return;
    for (const edgeSign of [-1, 1]) {
        const edge = pixelPoints.map((p, i) => {
            const t = i / (n - 1);
            return {x: p.x, y: p.y + edgeSign * bodyThickness(t, elapsedMs, thickness, activity) / 2};
        });
        for (const pass of FEATHER_PASSES) {
            cr.save();
            const gradient = buildRibbonGradient(width, rgb, rgb, rgb, alpha * pass.alphaScale);
            cr.setSource(gradient);
            cr.setLineWidth(thickness * 0.18 * pass.scale);
            cr.setLineCap(1);
            cr.setLineJoin(1);
            catmullRomTo(cr, edge, true);
            cr.stroke();
            cr.restore();
        }
    }
}

/**
 * A thin, wispy tendril that curls away from the main strand's centreline
 * — rendered as soft glow strokes only (no solid fill), increasingly
 * dissipated toward the tail, evoking a trailing smoke curl peeling off
 * the main ribbon rather than a second parallel band.
 */
function paintWisp(cr, voicePixelPoints, width, elapsedMs, rgb, baseAlpha, thickness, seed, activity = 1) {
    const n = voicePixelPoints.length;
    if (n < 2)
        return;
    // More pronounced curling overall, and scaled by how much audio is
    // actually coming through: barely-there at idle, a bold curl at full
    // voice — "more reactive to the audio."
    const curlMagnitude = thickness * (WISP.curlMin + WISP.curlActivity * activity);
    const wispPoints = voicePixelPoints.map((p, i) => {
        const t = i / (n - 1);
        // Curls away perpendicular-ish to the flow (a pure vertical offset
        // reads convincingly at this aspect ratio/scale) with its own,
        // higher-frequency drift, dissipating further toward the tail (t→1)
        // in addition to the shared edgeTaper.
        const curl = driftWave(t, elapsedMs,
            WISP.freqBase + seed * WISP.freqSeed,
            WISP.speedBase + seed * WISP.speedSeed,
            seed * WISP.phaseSeed) *
            curlMagnitude * (WISP.tailFloor + (1 - WISP.tailFloor) * t);
        return {x: p.x, y: p.y + curl};
    });
    const wispAlpha = baseAlpha * (WISP.alphaMin + WISP.alphaActivity * activity);
    cr.save();
    const gradient = new Cairo.LinearGradient(0, 0, width, 0);
    for (const {pos, alpha} of WISP_GRADIENT_STOPS)
        gradient.addColorStopRGBA(pos, rgb.r, rgb.g, rgb.b, clamp01(wispAlpha * alpha));
    cr.setSource(gradient);
    cr.setLineWidth(thickness * WISP.lineWidthFraction);
    cr.setLineCap(1);
    cr.setLineJoin(1);
    catmullRomTo(cr, wispPoints, true);
    cr.stroke();
    cr.restore();
}

/**
 * Resolve a model + caller palette into the concrete tones and activity
 * scalars a renderer needs. Shared by the Cairo painter below and the GPU
 * path (`ribbonShader.js` uploads the result as uniforms), so the amber
 * tint substitution, the shadow-tone blend and the `activity` /
 * `effectStrength` ramps are computed in exactly one place.
 *
 * @param {object} model - `computeRibbonModel` output.
 * @param {object} palette - caller-resolved colours (CSS or hex strings).
 * @returns {{mainRgb: object, highlightRgb: object, shadowRgb: object,
 *     activity: number, effectStrength: number}}
 */
export function resolveRibbonPalette(model, palette) {
    const {strands = [], tint = null} = model;
    const mainRgb = tint === RibbonTint.AMBER
        ? colorToRgbFloat(AMBER_MAIN) : colorToRgbFloat(palette.main);
    const highlightRgb = tint === RibbonTint.AMBER
        ? colorToRgbFloat(AMBER_HIGHLIGHT) : colorToRgbFloat(palette.highlight);
    // The darker/complement tone reads as a warm, smouldering-red shadow
    // undertone, not a bold second hue — blended most of the way toward
    // the main warm colour and then darkened toward the background.
    const complementRgb = tint === RibbonTint.AMBER
        ? colorToRgbFloat(AMBER_MAIN) : colorToRgbFloat(palette.darkerComplement);
    const shadowRgb = darkenRgb(mixRgb(complementRgb, mainRgb, 0.6), 0.45);

    // How much audio is actually coming through right now, derived directly
    // from the voice strand's own amplitude (already normalized ~0 at idle
    // to ~1 at loud, since that's exactly what drives its geometry) — used
    // to scale the billow/curl/wisp effects so the ribbon reads calmer and
    // flatter at silence, and more alive the louder the voice gets.
    const voiceStrand = strands.find(s => s.role === StrandRole.VOICE);
    const activity = voiceStrand
        ? clamp01(Math.max(0, ...voiceStrand.points.map(p => Math.abs(p.y))))
        : 1;
    return {
        mainRgb, highlightRgb, shadowRgb,
        activity,
        // A smoothed 0-1 ramp of `activity` used to fade the glow/feather/
        // wisp embellishments in/out — see `activityRamp` for why a hard
        // on/off would visibly "pop".
        effectStrength: activityRamp(activity),
    };
}

/**
 * Paint the wave ribbon into a Cairo context. Pure drawing — no state, no
 * timers, no gi toolkit beyond `cairo` itself.
 *
 * @param {cairo.Context} cr
 * @param {number} width
 * @param {number} height
 * @param {object} model - `ribbon.js`'s `computeRibbonModel` output.
 * @param {{main: (string|object), highlight: (string|object),
 *     darkerComplement: (string|object), translucentAlpha: number}} palette -
 *     colors resolved by the caller. The Shell passes native CSS colours;
 *     strings keep this shared painter usable by the standalone demo/tests.
 */
export function paintRibbon(cr, width, height, model, palette) {
    if (width <= 0 || height <= 0)
        return;
    const {
        strands = [], dots = null, convergence = null, brightnessBoost = 0,
        elapsedMs = 0,
    } = model;
    const centreY = height / 2;
    // 2026-07-31: `safeScale` (≤1) keeps the centerline + thickness + glow
    // + wisp combination from ever exceeding the canvas's half-height —
    // see `computeSafeScale`'s doc comment.
    const safeScale = computeSafeScale();
    const verticalScale = (height / 2) * BASE_CENTRELINE_FRACTION * safeScale;

    const {mainRgb, highlightRgb, shadowRgb, activity, effectStrength} =
        resolveRibbonPalette(model, palette);

    let voicePixelPoints = null;

    // Draw back-to-front: base (soft haze) → secondary (shadow depth) →
    // voice (the bright, glowing focal strand) — so the most important
    // strand sits on top.
    const order = [StrandRole.BASE, StrandRole.SECONDARY, StrandRole.VOICE];
    for (const role of order) {
        // Below a small activity threshold, skip the depth layers entirely
        // rather than fading them — layering several near-flat strands'
        // glow/feather passes (each a few discrete stroke widths, not a
        // true blur) produces visible banding once they're all sitting at
        // nearly the same, nearly-static position. One clean voice strand
        // reads as calm and flat; three faint overlapping copies of it
        // read as stripes. Gated on the same smoothed `effectStrength` as
        // the glow/feather/wisps below, so depth layers fade in/out rather
        // than popping in at a hard threshold.
        if (role !== StrandRole.VOICE && effectStrength <= 0)
            continue;
        for (const strand of strands) {
            if (strand.role !== role)
                continue;
            const pixelPoints = strand.points.map(p => ({
                x: p.x * width,
                y: centreY - p.y * verticalScale,
            }));
            let baseRgb = role === StrandRole.SECONDARY ? shadowRgb : mainRgb;
            if (brightnessBoost > 0)
                baseRgb = lightenRgb(baseRgb, brightnessBoost * 0.6);
            const thickness = height * ROLE_THICKNESS_FRACTION[role] * safeScale;
            // The secondary/base depth layers fade out toward silence too
            // (not just thinner — dimmer), so a calm moment reads as one
            // thin, quiet ribbon rather than several faint parallel
            // stripes; the voice strand alone carries the idle "still
            // alive" motion.
            const depthActivity = role === StrandRole.VOICE ? 1 : effectStrength;
            const alpha = (strand.alpha ?? 1) * ROLE_ALPHA_SCALE[role] * depthActivity;

            if (role === StrandRole.VOICE) {
                voicePixelPoints = pixelPoints;
                // The glow is several stacked stroke widths (a cheap fake
                // blur, not a true one) — on a near-flat, near-static
                // curve those discrete widths become visible as banding
                // rather than a soft bloom, so it fades in smoothly (via
                // `activityRamp`, not a hard cutoff — avoids a visible
                // "pop" as the level crosses the threshold) once there's
                // enough actual shape/motion to hide the seams.
                if (effectStrength > 0) {
                    paintGlow(cr, pixelPoints, width, shadowRgb, baseRgb, highlightRgb,
                        alpha * effectStrength, thickness * 0.5);
                }
            }
            paintRibbonBody(cr, pixelPoints, baseRgb, highlightRgb, shadowRgb, alpha, thickness, width, elapsedMs, activity);
            if (effectStrength > 0) {
                paintFeatheredEdges(cr, pixelPoints, thickness, elapsedMs,
                    mixRgb(baseRgb, shadowRgb, 0.4), alpha * effectStrength, width, activity);
            }
        }
    }

    // Wispy trailing tendrils curling off the main (voice) strand — soft
    // glow strokes only, no solid fill, evoking trailing smoke rather than
    // a second parallel ribbon. Same banding reason as the glow above:
    // fade in smoothly with activity rather than a hard cutoff.
    if (voicePixelPoints !== null && effectStrength > 0) {
        const wispThickness = height * WISP_THICKNESS_FRACTION * safeScale;
        for (const tendril of WISP_TENDRILS) {
            const rgb = tendril.fromShadow
                ? mixRgb(shadowRgb, mainRgb, tendril.mix)
                : mixRgb(mainRgb, highlightRgb, tendril.mix);
            paintWisp(cr, voicePixelPoints, width, elapsedMs + tendril.timeOffsetMs,
                rgb, tendril.alpha * effectStrength, wispThickness, tendril.seed, activity);
        }
    }

    if (dots !== null) {
        for (const dot of dots) {
            const rgb = lightenRgb(mainRgb, 0.3);
            cr.save();
            cr.setSourceRGBA(rgb.r, rgb.g, rgb.b, clamp01(dot.alpha));
            cr.arc(dot.x * width, centreY, height * 0.09, 0, Math.PI * 2);
            cr.fill();
            cr.restore();
        }
    }

    if (convergence !== null) {
        const rgb = lightenRgb(mainRgb, 0.5);
        const radius = height * 0.12 * (1 + brightnessBoost);
        cr.save();
        cr.setSourceRGBA(rgb.r, rgb.g, rgb.b, clamp01(convergence.alpha));
        cr.arc(convergence.x * width, centreY - convergence.y * verticalScale, radius, 0, Math.PI * 2);
        cr.fill();
        cr.restore();
    }
}
