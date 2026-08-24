// ribbonGlsl.test.js — conformance test for the GENERATED wave-ribbon
// shader (feature 004-gnome-shell-indicator, 2026-08-21 GPU rasterization
// pass). No Shell, no GL context: this checks the one property that
// actually matters and that a visual check cannot give you — that the
// shader's baked-in constants still equal the JS constants the Cairo
// painter uses.
//
// The two renderers are deliberately different algorithms (scanline
// fills/strokes vs. a per-pixel distance field), so their code cannot be
// shared and their pixels will never be bit-identical. Their TUNING must be
// shared, and that is exactly what silently drifts — see `computeSafeScale`
// in ribbonPaint.js, written precisely because hand-picked literals "drifted
// out of sync with each other and caused the bug".
//
//     gjs -m test/ribbonGlsl.test.js        (from extensions/myna-shell/)

import System from 'system';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';

import {
    computeRibbonModel,
    DEFAULT_STRAND_COUNT,
    FLOW_SPEED,
    RibbonPhase,
    SPATIAL_FREQUENCY,
    StrandRole,
} from '../ribbon.js';
import {
    buildRibbonShader,
    glslConstantDefines,
    MAX_DOTS,
    MAX_STRANDS,
    PAINT_ORDER,
    RIBBON_UNIFORMS,
    ROLE_TAG,
} from '../ribbonGlsl.js';
import {
    ACTIVITY_RAMP,
    BASE_CENTRELINE_FRACTION,
    BILLOW,
    computeSafeScale,
    EDGE_TAPER,
    RIBBON_GRADIENT_STOPS,
    ROLE_ALPHA_SCALE,
    ROLE_THICKNESS_FRACTION,
    WISP,
    WISP_THICKNESS_FRACTION,
} from '../ribbonPaint.js';

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

const {declarations, code} = buildRibbonShader();
const source = `${declarations}\n${code}`;

// --- The shader source is well-formed enough to hand to Cogl ------------

check('the generator produces a declarations block', declarations.length > 0);
check('the generator produces a replace block', code.length > 0);
check('the shader writes cogl_color_out', code.includes('cogl_color_out ='));
check('the shader reads the actor UV via cogl_tex_coord_in',
    code.includes('cogl_tex_coord_in[0]'));
check('no unresolved JS value leaked into the source',
    !/undefined|NaN|\[object/.test(source));
// GLSL has no implicit int→float promotion, so a bare integer literal in a
// float expression is a compile error on stricter drivers.
check('every emitted #define is a float literal (has a decimal point)',
    glslConstantDefines().split('\n').every(line => /\s-?\d+\.\d+([eE]-?\d+)?$/.test(line)));

// --- Constants: the shader's copy must equal the JS original ------------

const defines = new Map(
    glslConstantDefines().split('\n').map(line => {
        const [, name, value] = /^#define (\S+) (\S+)$/.exec(line);
        return [name, Number(value)];
    }));

function sameConstant(defineName, jsValue, label) {
    check(`${label}: shader ${defineName} === JS ${jsValue}`,
        defines.get(defineName) === jsValue);
}

sameConstant('MYNA_SPATIAL_FREQUENCY', SPATIAL_FREQUENCY, 'wave');
sameConstant('MYNA_FLOW_SPEED', FLOW_SPEED, 'wave');
sameConstant('MYNA_BASE_CENTRELINE_FRACTION', BASE_CENTRELINE_FRACTION, 'geometry');
sameConstant('MYNA_SAFE_SCALE', computeSafeScale(), 'overflow guard');
sameConstant('MYNA_TAPER_IN', EDGE_TAPER.inWidth, 'edge taper');
sameConstant('MYNA_TAPER_OUT', EDGE_TAPER.outWidth, 'edge taper');
sameConstant('MYNA_BILLOW_MIN', BILLOW.minAmount, 'billow');
sameConstant('MYNA_BILLOW_ACTIVITY', BILLOW.activityAmount, 'billow');
sameConstant('MYNA_BILLOW_FREQ', BILLOW.freq, 'billow');
sameConstant('MYNA_BILLOW_SPEED', BILLOW.speed, 'billow');
sameConstant('MYNA_BILLOW_PHASE', BILLOW.phase, 'billow');
sameConstant('MYNA_TAPER_FLOOR', BILLOW.taperFloor, 'billow');
sameConstant('MYNA_ACTIVITY_LO', ACTIVITY_RAMP.lo, 'activity ramp');
sameConstant('MYNA_ACTIVITY_HI', ACTIVITY_RAMP.hi, 'activity ramp');
sameConstant('MYNA_WISP_THICKNESS_FRACTION', WISP_THICKNESS_FRACTION, 'wisp');
sameConstant('MYNA_WISP_CURL_MIN', WISP.curlMin, 'wisp');
sameConstant('MYNA_WISP_CURL_ACTIVITY', WISP.curlActivity, 'wisp');
sameConstant('MYNA_WISP_LINE_WIDTH', WISP.lineWidthFraction, 'wisp');

// The per-role tables are the likeliest thing to be retuned in only one
// renderer, since they read as "just a number" at each call site.
for (const role of Object.values(StrandRole)) {
    const suffix = role.toUpperCase();
    sameConstant(`MYNA_THICKNESS_${suffix}`, ROLE_THICKNESS_FRACTION[role], 'role thickness');
    sameConstant(`MYNA_ALPHA_${suffix}`, ROLE_ALPHA_SCALE[role], 'role alpha');
}

// --- Uniforms: the declared set and the uploaded set must agree ---------

const declaredUniforms = [...declarations.matchAll(/^uniform\s+(\w+)\s+(\w+);/gm)]
    .map(([, type, name]) => ({type, name}));

// ClutterShaderFloat asserts `size <= 4`, so a GLSL array uniform can never
// be uploaded — it fails at runtime and the uniform silently stays zero.
// Everything must be packed into a scalar or a vec2/3/4.
eq('no uniform is declared as an array',
    [...declarations.matchAll(/^uniform\s+\w+\s+\w+\[/gm)].length, 0);

eq('every declared uniform is in RIBBON_UNIFORMS',
    declaredUniforms.filter(u => !RIBBON_UNIFORMS.some(r => r.name === u.name)).length, 0);
eq('every RIBBON_UNIFORMS entry is declared in the shader',
    RIBBON_UNIFORMS.filter(r => !declaredUniforms.some(u => u.name === r.name)).length, 0);

const GLSL_COMPONENTS = {float: 1, vec2: 2, vec3: 3, vec4: 4};
for (const u of declaredUniforms) {
    const spec = RIBBON_UNIFORMS.find(r => r.name === u.name);
    if (spec === undefined)
        continue;
    eq(`${u.name}: component count matches its GLSL type`,
        spec.components, GLSL_COMPONENTS[u.type]);
    check(`${u.name}: fits ClutterShaderFloat's four-component limit`,
        spec.components >= 1 && spec.components <= 4);
}

check('there is one geometry/style uniform pair per available strand slot',
    Array.from({length: MAX_STRANDS}, (_, i) => i).every(i =>
        declaredUniforms.some(u => u.name === `uStrandGeom${i}` && u.type === 'vec4') &&
        declaredUniforms.some(u => u.name === `uStrandStyle${i}` && u.type === 'vec3')));

check('the strand slots are sized for the model\'s maximum strand count',
    MAX_STRANDS === DEFAULT_STRAND_COUNT);

check('every strand slot is composited by the shader body',
    Array.from({length: MAX_STRANDS}, (_, i) => i).every(i =>
        code.includes(`drawStrand(uStrandGeom${i}, uStrandStyle${i}`)));

// The shader composites strand 0, 1, 2… in index order, so the uploader's
// sort is what puts the bright voice strand on top.
eq('the paint order covers every StrandRole', PAINT_ORDER.length,
    Object.values(StrandRole).length);
eq('the paint order ends with the voice strand',
    PAINT_ORDER[PAINT_ORDER.length - 1], StrandRole.VOICE);

// --- Role tags: distinct, and covering every StrandRole ----------------

const roleValues = Object.values(StrandRole);
eq('every StrandRole has a numeric shader tag',
    roleValues.filter(r => typeof ROLE_TAG[r] !== 'number').length, 0);
eq('role tags are distinct', new Set(roleValues.map(r => ROLE_TAG[r])).size, roleValues.length);
eq('voice is tag 0, so it draws last (in front)', ROLE_TAG[StrandRole.VOICE], 0);
check('the shader branches on every role tag',
    roleValues.every(r => declarations.includes(`MYNA_THICKNESS_${r.toUpperCase()}`)));

// --- The gradient chain covers the whole 0-1 span ----------------------

const stopPositions = [...declarations.matchAll(/if \(t >= (\S+) && t <= (\S+)\)/g)]
    .map(([, from, to]) => [Number(from), Number(to)]);
check('the emitted gradient starts at 0', stopPositions.some(([from]) => from === 0));
check('the emitted gradient ends at 1', stopPositions.some(([, to]) => to === 1));
check('the emitted gradient has a segment per authored stop pair',
    stopPositions.length >= RIBBON_GRADIENT_STOPS.length - 1);

// --- The model still supplies every parameter the shader needs ---------
//
// The shader regenerates each strand's sine itself, so it depends on the
// model reporting the parameters that produced its sampled points. If
// computeRibbonModel ever stops emitting these, the GPU path would silently
// render flat strands rather than fail.

const REQUIRED_STRAND_FIELDS = ['amplitude', 'phaseOffset', 'delayMs', 'speedScale'];
for (const phase of Object.values(RibbonPhase)) {
    const model = computeRibbonModel({envelope: 0.5, elapsedMs: 120, phase, phaseElapsedMs: 40});
    for (const field of REQUIRED_STRAND_FIELDS) {
        check(`${phase}: every strand reports a numeric ${field}`,
            model.strands.every(s => Number.isFinite(s[field])));
    }
}

{
    const reduced = computeRibbonModel({envelope: 0.5, elapsedMs: 120, reducedMotion: true});
    check('reduced motion still reports the shader parameters',
        reduced.strands.every(s => REQUIRED_STRAND_FIELDS.every(f => Number.isFinite(s[f]))));
    eq('reduced motion is a flat strand (zero amplitude → flat sine on the GPU too)',
        reduced.strands[0].amplitude, 0);
}

{
    // The morph phase is the only producer of dots, and the shader has a
    // fixed-size array for them.
    const morph = computeRibbonModel({
        envelope: 0.5, elapsedMs: 120, phase: RibbonPhase.MORPH, phaseElapsedMs: 100,
    });
    check('the morph phase never emits more dots than the shader can hold',
        morph.dots !== null && morph.dots.length <= MAX_DOTS);
}

// --- Regenerating the wave on the GPU matches the model's own points ----
//
// The shader evaluates `strandY` per pixel instead of consuming the model's
// sampled points. This mirrors that GLSL expression in JS and checks it
// reproduces the very points computeRibbonModel returned — the actual
// guarantee that the two renderers draw the same wave.

function strandYMirror(t, strand, elapsedMs) {
    const angle = t * SPATIAL_FREQUENCY * Math.PI * 2 +
        strand.phaseOffset + (elapsedMs - strand.delayMs) * FLOW_SPEED * strand.speedScale;
    return Math.sin(angle) * strand.amplitude;
}

{
    const elapsedMs = 417;
    const model = computeRibbonModel({envelope: 0.62, elapsedMs});
    let worst = 0;
    for (const strand of model.strands) {
        const n = strand.points.length;
        for (let i = 0; i < n; i++) {
            const t = i / (n - 1);
            worst = Math.max(worst,
                Math.abs(strandYMirror(t, strand, elapsedMs) - strand.points[i].y));
        }
    }
    check(`the shader's strandY reproduces the model's points (worst Δ ${worst.toExponential(2)})`,
        worst < 1e-12);
}

// --- Does it actually COMPILE? ------------------------------------------
//
// Everything above checks the source we generate is *consistent*; this
// checks a driver would accept it. glslangValidator is optional (the check
// skips when it is absent) but catches the whole class of errors a
// generator makes — a missing decimal point, an undeclared identifier, an
// int/float mismatch — which otherwise surface only as a silently blank
// ribbon inside a live Shell session.

function glslangPath() {
    for (const dir of (GLib.getenv('PATH') ?? '').split(':')) {
        const path = `${dir}/glslangValidator`;
        if (GLib.file_test(path, GLib.FileTest.IS_EXECUTABLE))
            return path;
    }
    return null;
}

/** Wrap our snippet in the surrounding declarations Cogl itself provides,
 * so the fragment is a complete, compilable shader. */
function standaloneShader(version) {
    const preamble = version === 100
        ? '#version 100\nprecision highp float;'
        : '#version 120';
    return `${preamble}
vec4 cogl_color_out;
vec4 cogl_color_in;
vec4 cogl_tex_coord_in[4];
${declarations}
void main() {
${code}
gl_FragColor = cogl_color_out;
}
`;
}

{
    const glslang = glslangPath();
    if (glslang === null) {
        print('     (skip) glslangValidator not installed — shader compile check skipped');
    } else {
        for (const version of [120, 100]) {
            const [path, stream] = Gio.File.new_tmp('myna-ribbon-XXXXXX.frag');
            stream.get_output_stream().write_all(standaloneShader(version), null);
            stream.close(null);
            const [, stdout, , status] = GLib.spawn_sync(
                null, [glslang, '-S', 'frag', path.get_path()], null,
                GLib.SpawnFlags.DEFAULT, null);
            const output = new TextDecoder().decode(stdout);
            check(`the generated shader compiles as GLSL ${version === 100 ? 'ES 1.00' : '1.20'}`,
                status === 0 && !output.includes('ERROR'));
            if (status !== 0 || output.includes('ERROR'))
                print(output);
            path.delete(null);
        }
    }
}

print(failures === 0 ? 'PASS ribbonGlsl.test.js' : `FAIL ribbonGlsl.test.js (${failures})`);
System.exit(failures === 0 ? 0 : 1);
