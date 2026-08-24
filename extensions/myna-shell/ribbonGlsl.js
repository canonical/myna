// ribbonGlsl.js — GENERATES the wave ribbon's GLSL fragment shader from the
// same constants the Cairo painter uses (feature 004-gnome-shell-indicator,
// 2026-08-21 GPU rasterization pass).
//
// # Why a generator rather than a .glsl file
//
// `ribbonPaint.js` (Cairo, scanline fills/strokes) and this shader (a
// per-pixel distance field) are two genuinely different rasterization
// algorithms — one cannot be derived from the other, and neither is a
// translation of the other. What they MUST share is their tuning: the
// gradient stops, the glow/feather pass tables, the billow and taper
// shapes, the per-role thicknesses. That is precisely where two
// hand-maintained copies would silently drift apart, which is the same
// class of bug `computeSafeScale` was written to close (see its doc
// comment: values "drifted out of sync with each other and caused the
// bug").
//
// So this module imports those tables from `ribbon.js` / `ribbonPaint.js`
// and bakes them into the shader source as `#define`s and unrolled
// expressions. There is no build step: it is an ordinary ES module that
// returns a string, so the constants are read from the one place they are
// defined, at load time. `test/ribbonGlsl.test.js` asserts the emitted
// `#define`s still match their JS originals, so a retune of either side
// cannot quietly desynchronize them.
//
// # What is NOT in the shader
//
// The model itself. `computeRibbonModel` (phase state machine, envelope
// smoothing, the amplitude response curve) stays pure JS, stays headlessly
// testable, and stays the single authority for *what* to draw. The shader
// only rasterizes, and it regenerates each strand's sine analytically from
// the per-strand parameters that model now reports (`amplitude`,
// `phaseOffset`, `delayMs`, `speedScale`) rather than from constants of its
// own. Because the centreline is single-valued (`y = f(x)`), no polyline
// SDF is needed — the vertical distance IS the distance, which also matches
// what `paintRibbonBody` does (it offsets its top/bottom edges purely
// vertically too).

import {
    DEFAULT_STRAND_COUNT,
    FLOW_SPEED,
    SPATIAL_FREQUENCY,
    StrandRole,
} from './ribbon.js';
import {
    ACTIVITY_RAMP,
    BASE_CENTRELINE_FRACTION,
    BILLOW,
    computeSafeScale,
    EDGE_TAPER,
    FEATHER_PASSES,
    GLOW_PASSES,
    RIBBON_GRADIENT_STOPS,
    ROLE_ALPHA_SCALE,
    ROLE_THICKNESS_FRACTION,
    WISP,
    WISP_GRADIENT_STOPS,
    WISP_TENDRILS,
    WISP_THICKNESS_FRACTION,
} from './ribbonPaint.js';

/** Upper bound on the number of strands the shader can draw (GLSL needs a
 * compile-time count; the model never returns more than this). */
export const MAX_STRANDS = DEFAULT_STRAND_COUNT;

/** How many travelling dots the `morph` phase produces. Packed into a
 * single vec3, so this cannot exceed 4 without repacking. */
export const MAX_DOTS = 3;

/** Numeric role tags, since GLSL has no strings. */
export const ROLE_TAG = Object.freeze({
    [StrandRole.VOICE]: 0,
    [StrandRole.SECONDARY]: 1,
    [StrandRole.BASE]: 2,
});

/**
 * Painter's order, back to front: base (soft haze) → secondary (shadow
 * depth) → voice (the bright focal strand). `paintRibbon` walks this same
 * order; the GPU path sorts its uniform upload by it so the shader can
 * simply composite strand 0, 1, 2… in index order instead of running a
 * role-matching pass per layer.
 */
export const PAINT_ORDER = Object.freeze([
    StrandRole.BASE, StrandRole.SECONDARY, StrandRole.VOICE,
]);

/**
 * Every uniform the generated shader declares, with its component count.
 *
 * All of them are scalars or vec2/3/4 — never arrays. `ClutterShaderEffect`
 * marshals uniforms through `ClutterShaderFloat`, which asserts
 * `size <= 4` (clutter-shader-types.c), so `float u[5]` is simply not
 * expressible through this API: it fails at runtime with
 * `clutter_value_set_shader_float: assertion 'size <= 4' failed` and the
 * uniform silently stays zero. Per-strand values are therefore packed into
 * one vec4 + one vec3 each, and the shader's strand loop is unrolled.
 *
 * `ribbonShader.js` uploads exactly this set; ribbonGlsl.test.js asserts
 * the two agree, so a uniform cannot be added to the shader and forgotten
 * in the uploader.
 */
export const RIBBON_UNIFORMS = Object.freeze([
    Object.freeze({name: 'uSize', components: 2}),
    Object.freeze({name: 'uElapsedMs', components: 1}),
    Object.freeze({name: 'uActivity', components: 1}),
    Object.freeze({name: 'uEffectStrength', components: 1}),
    Object.freeze({name: 'uBrightnessBoost', components: 1}),
    // Per strand, in painter's order: geometry is
    // (amplitude, phaseOffset, delayMs, speedScale) — exactly the arguments
    // ribbon.js's generateWavePoints used — and style is
    // (alpha, roleTag, active).
    ...Array.from({length: MAX_STRANDS}, (_, i) => [
        Object.freeze({name: `uStrandGeom${i}`, components: 4}),
        Object.freeze({name: `uStrandStyle${i}`, components: 3}),
    ]).flat(),
    Object.freeze({name: 'uVoice', components: 4}),
    Object.freeze({name: 'uMain', components: 3}),
    Object.freeze({name: 'uHighlight', components: 3}),
    Object.freeze({name: 'uShadow', components: 3}),
    Object.freeze({name: 'uDotX', components: MAX_DOTS}),
    Object.freeze({name: 'uDotAlpha', components: 1}),
    Object.freeze({name: 'uConvergence', components: 3}),
]);

/** Emit a JS number as a GLSL float literal (GLSL has no implicit int→float
 * conversion in expressions like `1 / 2`, so integers need a decimal). */
function f(value) {
    if (!Number.isFinite(value))
        throw new Error(`ribbonGlsl: refusing to emit non-finite ${value}`);
    return Number.isInteger(value) ? `${value}.0` : String(value);
}

/** The `#define` block: every shared constant, named after its JS origin
 * so a grep for the JS name finds the shader's copy too. */
export function glslConstantDefines() {
    const defines = {
        MYNA_PI: Math.PI,
        MYNA_SPATIAL_FREQUENCY: SPATIAL_FREQUENCY,
        MYNA_FLOW_SPEED: FLOW_SPEED,
        MYNA_BASE_CENTRELINE_FRACTION: BASE_CENTRELINE_FRACTION,
        MYNA_SAFE_SCALE: computeSafeScale(),
        MYNA_TAPER_IN: EDGE_TAPER.inWidth,
        MYNA_TAPER_OUT: EDGE_TAPER.outWidth,
        MYNA_BILLOW_MIN: BILLOW.minAmount,
        MYNA_BILLOW_ACTIVITY: BILLOW.activityAmount,
        MYNA_BILLOW_FREQ: BILLOW.freq,
        MYNA_BILLOW_SPEED: BILLOW.speed,
        MYNA_BILLOW_PHASE: BILLOW.phase,
        MYNA_TAPER_FLOOR: BILLOW.taperFloor,
        MYNA_ACTIVITY_LO: ACTIVITY_RAMP.lo,
        MYNA_ACTIVITY_HI: ACTIVITY_RAMP.hi,
        MYNA_THICKNESS_VOICE: ROLE_THICKNESS_FRACTION[StrandRole.VOICE],
        MYNA_THICKNESS_SECONDARY: ROLE_THICKNESS_FRACTION[StrandRole.SECONDARY],
        MYNA_THICKNESS_BASE: ROLE_THICKNESS_FRACTION[StrandRole.BASE],
        MYNA_ALPHA_VOICE: ROLE_ALPHA_SCALE[StrandRole.VOICE],
        MYNA_ALPHA_SECONDARY: ROLE_ALPHA_SCALE[StrandRole.SECONDARY],
        MYNA_ALPHA_BASE: ROLE_ALPHA_SCALE[StrandRole.BASE],
        MYNA_WISP_THICKNESS_FRACTION: WISP_THICKNESS_FRACTION,
        MYNA_WISP_CURL_MIN: WISP.curlMin,
        MYNA_WISP_CURL_ACTIVITY: WISP.curlActivity,
        MYNA_WISP_TAIL_FLOOR: WISP.tailFloor,
        MYNA_WISP_FREQ_BASE: WISP.freqBase,
        MYNA_WISP_FREQ_SEED: WISP.freqSeed,
        MYNA_WISP_SPEED_BASE: WISP.speedBase,
        MYNA_WISP_SPEED_SEED: WISP.speedSeed,
        MYNA_WISP_PHASE_SEED: WISP.phaseSeed,
        MYNA_WISP_ALPHA_MIN: WISP.alphaMin,
        MYNA_WISP_ALPHA_ACTIVITY: WISP.alphaActivity,
        MYNA_WISP_LINE_WIDTH: WISP.lineWidthFraction,
    };
    return Object.entries(defines)
        .map(([name, value]) => `#define ${name} ${f(value)}`)
        .join('\n');
}

/** Unroll a stop table into a piecewise `mix()` chain. `toneOf` names the
 * GLSL expression carrying each stop's colour. */
function emitGradient(fnName, stops, toneOf) {
    const lines = [`vec4 ${fnName}(float t, vec3 shadowTone, vec3 mainTone, vec3 highlightTone) {`];
    lines.push(`    vec3 rgb = ${toneOf(stops[0])};`);
    lines.push(`    float a = ${f(stops[0].alpha)};`);
    for (let i = 0; i < stops.length - 1; i++) {
        const from = stops[i];
        const to = stops[i + 1];
        const span = to.pos - from.pos;
        // A zero-width span would divide by zero; the table is authored
        // strictly increasing, so this is a guard against a bad retune.
        if (span <= 0)
            throw new Error(`ribbonGlsl: ${fnName} stops must strictly increase`);
        lines.push(`    if (t >= ${f(from.pos)} && t <= ${f(to.pos)}) {`);
        lines.push(`        float u = (t - ${f(from.pos)}) / ${f(span)};`);
        lines.push(`        rgb = mix(${toneOf(from)}, ${toneOf(to)}, u);`);
        lines.push(`        a = mix(${f(from.alpha)}, ${f(to.alpha)}, u);`);
        lines.push('    }');
    }
    lines.push('    return vec4(rgb, a);');
    lines.push('}');
    return lines.join('\n');
}

/** The glow/feather stacks become summed Gaussians — which is what a stack
 * of progressively wider, fainter round strokes was approximating all
 * along, only without the discrete banding the Cairo comments call out. */
function emitGaussianStack(fnName, passes) {
    const lines = [`float ${fnName}(float d, float strokeWidth) {`, '    float total = 0.0;', '    float sigma;'];
    for (const {scale, alphaScale} of passes) {
        lines.push(`    sigma = max(strokeWidth * ${f(scale)} * 0.5, 0.5);`);
        lines.push(`    total += ${f(alphaScale)} * exp(-(d * d) / (2.0 * sigma * sigma));`);
    }
    lines.push('    return total;');
    lines.push('}');
    return lines.join('\n');
}

/** The wisp tendrils, unrolled from `WISP_TENDRILS` (a loop would need the
 * per-tendril tone mix as an array too, for no gain at two entries). */
function emitWisps() {
    const lines = [];
    for (const {seed, alpha, timeOffsetMs, mix: mixT, fromShadow} of WISP_TENDRILS) {
        const tone = fromShadow
            ? `mix(uShadow, uMain, ${f(mixT)})`
            : `mix(uMain, uHighlight, ${f(mixT)})`;
        lines.push(`    acc = over(acc, wispLayer(t, py, centreY, verticalScale, wispThickness, ${f(seed)}, uElapsedMs + ${f(timeOffsetMs)}, ${tone}, ${f(alpha)} * uEffectStrength));`);
    }
    return lines.join('\n');
}

/**
 * Build the fragment shader.
 *
 * @returns {{declarations: string, code: string}} ready for
 *     `Cogl.Snippet.new(Cogl.SnippetHook.FRAGMENT, declarations, null)`
 *     plus `snippet.set_replace(code)`.
 */
export function buildRibbonShader() {
    const declarations = `
${glslConstantDefines()}

uniform vec2 uSize;
uniform float uElapsedMs;
uniform float uActivity;
uniform float uEffectStrength;
uniform float uBrightnessBoost;
${Array.from({length: MAX_STRANDS}, (_, i) =>
    `uniform vec4 uStrandGeom${i};\nuniform vec3 uStrandStyle${i};`).join('\n')}
uniform vec4 uVoice;
uniform vec3 uMain;
uniform vec3 uHighlight;
uniform vec3 uShadow;
uniform vec3 uDotX;
uniform float uDotAlpha;
uniform vec3 uConvergence;

float clamp01(float x) {
    return clamp(x, 0.0, 1.0);
}

vec3 lightenRgb(vec3 c, float amount) {
    return c + (1.0 - c) * clamp01(amount);
}

// Premultiplied source-over. Cogl blends premultiplied, and accumulating
// in the same space keeps this a single add/lerp per layer.
vec4 over(vec4 dst, vec4 src) {
    return src + dst * (1.0 - src.a);
}

vec4 premul(vec3 rgb, float a) {
    float c = clamp01(a);
    return vec4(rgb * c, c);
}

// Mirrors ribbonPaint.js's edgeTaper: a raised cosine, so there is no
// visible kink where the taper begins.
float edgeTaper(float t) {
    float v = 1.0;
    if (t < MYNA_TAPER_IN)
        v = min(v, (1.0 - cos((t / MYNA_TAPER_IN) * MYNA_PI)) / 2.0);
    if (t > 1.0 - MYNA_TAPER_OUT)
        v = min(v, (1.0 - cos(((1.0 - t) / MYNA_TAPER_OUT) * MYNA_PI)) / 2.0);
    return clamp01(v);
}

float driftWave(float t, float ms, float freq, float speed, float phase) {
    return sin(t * freq * MYNA_PI * 2.0 + ms * speed + phase);
}

float bodyThickness(float t, float ms, float baseThickness, float activity) {
    float billowAmount = MYNA_BILLOW_MIN + MYNA_BILLOW_ACTIVITY * activity;
    float billow = 1.0 + billowAmount *
        driftWave(t, ms, MYNA_BILLOW_FREQ, MYNA_BILLOW_SPEED, MYNA_BILLOW_PHASE);
    float activityScale = 0.5 + 0.5 * activity;
    float taper = MYNA_TAPER_FLOOR + (1.0 - MYNA_TAPER_FLOOR) * edgeTaper(t);
    return baseThickness * activityScale * taper * billow;
}

// The strand centreline, regenerated analytically from the same parameters
// that produced the model's sampled points (ribbon.js generateWavePoints).
float strandY(float t, float amplitude, float phaseOffset, float delayMs, float speedScale) {
    float angle = t * MYNA_SPATIAL_FREQUENCY * MYNA_PI * 2.0 +
        phaseOffset + (uElapsedMs - delayMs) * MYNA_FLOW_SPEED * speedScale;
    return sin(angle) * amplitude;
}

${emitGradient('ribbonGradient', RIBBON_GRADIENT_STOPS, stop => {
    if (stop.tone === 'shadow')
        return 'shadowTone';
    if (stop.tone === 'highlight')
        return 'highlightTone';
    return 'mainTone';
})}

${emitGradient('wispGradient', WISP_GRADIENT_STOPS, () => 'mainTone')}

${emitGaussianStack('glowStack', GLOW_PASSES)}

${emitGaussianStack('featherStack', FEATHER_PASSES)}

float roleThickness(float role) {
    if (role < 0.5)
        return MYNA_THICKNESS_VOICE;
    if (role < 1.5)
        return MYNA_THICKNESS_SECONDARY;
    return MYNA_THICKNESS_BASE;
}

float roleAlphaScale(float role) {
    if (role < 0.5)
        return MYNA_ALPHA_VOICE;
    if (role < 1.5)
        return MYNA_ALPHA_SECONDARY;
    return MYNA_ALPHA_BASE;
}

// A soft-edged disc, antialiased over one pixel.
vec4 disc(vec2 p, vec2 centre, float radius, vec3 rgb, float alpha) {
    float d = length(p - centre);
    float cov = 1.0 - smoothstep(radius - 1.0, radius + 1.0, d);
    return premul(rgb, alpha * cov);
}

// One strand, composited over the accumulator. Strands arrive already sorted
// back-to-front by the uploader (PAINT_ORDER: base → secondary → voice), so
// this is simply called in index order rather than running a role-matching
// pass per layer.
//
//   geom  = (amplitude, phaseOffset, delayMs, speedScale)
//   style = (alpha, roleTag, active)
vec4 drawStrand(vec4 geom, vec3 style, float t, float py,
                float centreY, float verticalScale, vec4 acc) {
    float role = style.y;
    if (style.z < 0.5)
        return acc;
    // Below a small activity threshold the depth layers are skipped rather
    // than faded — several near-flat strands stacked at nearly the same
    // position read as stripes, not depth (see paintRibbon's comment).
    if (role >= 0.5 && uEffectStrength <= 0.0)
        return acc;

    float centre = centreY - strandY(t, geom.x, geom.y, geom.z, geom.w) * verticalScale;
    float d = abs(py - centre);
    float thickness = uSize.y * roleThickness(role) * MYNA_SAFE_SCALE;
    float halfBody = bodyThickness(t, uElapsedMs, thickness, uActivity) * 0.5;

    // The secondary strand is drawn in the shadow tone; the gradient's
    // "main" stops therefore carry that tone.
    vec3 baseRgb = (role > 0.5 && role < 1.5) ? uShadow : uMain;
    baseRgb = lightenRgb(baseRgb, uBrightnessBoost * 0.6);

    float depthActivity = (role < 0.5) ? 1.0 : uEffectStrength;
    float strandAlpha = style.x * roleAlphaScale(role) * depthActivity;

    vec4 grad = ribbonGradient(t, uShadow, baseRgb, uHighlight);

    // Glow sits behind the body, and only under the voice strand.
    if (role < 0.5 && uEffectStrength > 0.0) {
        float glow = glowStack(d, thickness * 0.5);
        acc = over(acc, premul(grad.rgb, grad.a * strandAlpha * uEffectStrength * glow));
    }

    // The body edge is feathered by widening its own falloff — which is
    // what Cairo's extra edge strokes were emulating.
    float feather = max(1.0, featherStack(0.0, thickness * 0.18) * thickness * uEffectStrength);
    float cov = 1.0 - smoothstep(halfBody - feather, halfBody + feather, d);
    return over(acc, premul(grad.rgb, grad.a * strandAlpha * cov));
}

vec4 wispLayer(float t, float py, float centreY, float verticalScale,
               float thickness, float seed, float ms, vec3 tone, float baseAlpha) {
    float centre = centreY - strandY(t, uVoice.x, uVoice.y, uVoice.z, uVoice.w) * verticalScale;
    float curlMagnitude = thickness * (MYNA_WISP_CURL_MIN + MYNA_WISP_CURL_ACTIVITY * uActivity);
    float curl = driftWave(t, ms,
        MYNA_WISP_FREQ_BASE + seed * MYNA_WISP_FREQ_SEED,
        MYNA_WISP_SPEED_BASE + seed * MYNA_WISP_SPEED_SEED,
        seed * MYNA_WISP_PHASE_SEED) *
        curlMagnitude * (MYNA_WISP_TAIL_FLOOR + (1.0 - MYNA_WISP_TAIL_FLOOR) * t);
    float d = abs(py - (centre + curl));
    float sigma = max(thickness * MYNA_WISP_LINE_WIDTH, 0.5);
    float fall = exp(-(d * d) / (2.0 * sigma * sigma));
    float alpha = baseAlpha * (MYNA_WISP_ALPHA_MIN + MYNA_WISP_ALPHA_ACTIVITY * uActivity);
    vec4 grad = wispGradient(t, tone, tone, tone);
    return premul(tone, alpha * grad.a * fall);
}
`;

    const code = `
vec2 uv = cogl_tex_coord_in[0].xy;
float t = clamp01(uv.x);
vec2 p = vec2(uv.x * uSize.x, uv.y * uSize.y);
float py = p.y;
float centreY = uSize.y * 0.5;
float verticalScale = (uSize.y * 0.5) * MYNA_BASE_CENTRELINE_FRACTION * MYNA_SAFE_SCALE;

vec4 acc = vec4(0.0);

// Strands are uploaded already sorted back-to-front (PAINT_ORDER), so the
// bright focal voice strand lands on top. Unrolled because the per-strand
// parameters are separate uniforms, not an array — see RIBBON_UNIFORMS.
${Array.from({length: MAX_STRANDS}, (_, i) =>
    `acc = drawStrand(uStrandGeom${i}, uStrandStyle${i}, t, py, centreY, verticalScale, acc);`).join('\n')}

// Wispy trailing tendrils curling off the voice strand — soft falloff only,
// no solid body, evoking trailing smoke rather than a second ribbon.
if (uEffectStrength > 0.0) {
    float wispThickness = uSize.y * MYNA_WISP_THICKNESS_FRACTION * MYNA_SAFE_SCALE;
${emitWisps()}
}

// morph: three travelling dots crossfading in as the wave fades out.
if (uDotAlpha > 0.0) {
    vec3 rgb = lightenRgb(uMain, 0.3);
    float radius = uSize.y * 0.09;
    acc = over(acc, disc(p, vec2(uDotX.x * uSize.x, centreY), radius, rgb, uDotAlpha));
    acc = over(acc, disc(p, vec2(uDotX.y * uSize.x, centreY), radius, rgb, uDotAlpha));
    acc = over(acc, disc(p, vec2(uDotX.z * uSize.x, centreY), radius, rgb, uDotAlpha));
}

// complete: the convergence point, fading on the same curve as its pulse.
if (uConvergence.z > 0.0) {
    float radius = uSize.y * 0.12 * (1.0 + uBrightnessBoost);
    acc = over(acc, disc(p, vec2(uConvergence.x * uSize.x, centreY - uConvergence.y * verticalScale),
                         radius, lightenRgb(uMain, 0.5), uConvergence.z));
}

cogl_color_out = acc;
`;

    return {declarations, code};
}
