// shellCompat.test.js — GJS contract test for the pure half of the Shell
// compatibility seam (2026-08-27 Shell 46 backport).
//
//     gjs -m test/shellCompat.test.js   (from extensions/myna-shell/)
//
// exits 0 when every guarantee holds, 1 otherwise. No Shell needed — which
// is the whole point of shellCompatLogic.js existing separately. The half
// that does need one is test/compat-probe.sh.

import System from 'system';

import {motionSource, orientationProps} from '../shellCompatLogic.js';

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

// --- Modern Shells: read `reduced-motion` straight through -----------------

const modern = motionSource({reducedMotion: true});
eq('modern Shell connects notify::reduced-motion',
    modern.signal, 'notify::reduced-motion');
eq('modern Shell reports reduced motion when set',
    modern.read({reducedMotion: true}), true);
eq('modern Shell reports normal motion when unset',
    modern.read({reducedMotion: false}), false);

// Shell 51 kept the name and changed the type: `reduced-motion` is now the
// St.ReducedMotion enum, so the property reads as a number. 0 is falsy and
// would have limped along; REDUCE=1 is truthy but not `true`, and the value
// travels as far as ribbon.js.
eq('modern Shell reads St.ReducedMotion.REDUCE as reduced motion',
    modern.read({reducedMotion: 1}), true);
eq('modern Shell reads St.ReducedMotion.NO_PREFERENCE as normal motion',
    modern.read({reducedMotion: 0}), false);

// --- Pre-`reduced-motion` Shells (46): `enable-animations`, INVERTED -------
// The inversion is the bug this file exists to catch: getting it backwards
// runs the wave ribbon's full animation for the users who asked for none,
// and pins everyone else to a static line — neither of which throws, so
// nothing else would notice.

const legacy = motionSource({reducedMotion: false});
eq('pre-reduced-motion Shell connects notify::enable-animations',
    legacy.signal, 'notify::enable-animations');
eq('animations disabled means reduced motion ON',
    legacy.read({enableAnimations: false}), true);
eq('animations enabled means reduced motion OFF',
    legacy.read({enableAnimations: true}), false);

// The reader must not consult the property this Shell does not have: an
// St.Settings without `reduced-motion` returns undefined for it, and a
// reader that fell through to `undefined` would read as "not reduced" for
// everyone, silently.
eq('the legacy reader ignores a missing reduced-motion property',
    legacy.read({enableAnimations: false, reducedMotion: undefined}), true);

// --- Both branches return a real boolean, never a truthy value ------------
// `_syncFrameTimeline` compares with `!`, but `computeRibbonModel` passes
// the value on as `reducedMotion`, where a non-boolean would reach ribbon.js.
for (const [label, source, settings] of [
    ['modern', modern, {reducedMotion: true}],
    ['modern (enum)', modern, {reducedMotion: 1}],
    ['legacy', legacy, {enableAnimations: true}],
]) {
    check(`${label} reader returns a boolean`,
        typeof source.read(settings) === 'boolean');
}

// --- St.BoxLayout direction -----------------------------------------------
// Caught live by test/entrance-visual.sh on Shell 46: passing `orientation`
// to a BoxLayout that has none throws out of the constructor ("No property
// orientation on StBoxLayout") and the pill never exists at all.

const ORIENTATION = {VERTICAL: 'v-enum', HORIZONTAL: 'h-enum'};

function deepEq(name, actual, expected) {
    check(`${name} (got ${JSON.stringify(actual)})`,
        JSON.stringify(actual) === JSON.stringify(expected));
}

deepEq('modern Shell, vertical box',
    orientationProps(true, true, ORIENTATION), {orientation: 'v-enum'});
deepEq('modern Shell, horizontal box',
    orientationProps(true, false, ORIENTATION), {orientation: 'h-enum'});
deepEq('pre-47 Shell, vertical box',
    orientationProps(false, true, ORIENTATION), {vertical: true});
deepEq('pre-47 Shell, horizontal box',
    orientationProps(false, false, ORIENTATION), {vertical: false});

// Each spelling must be the ONLY key present: St rejects an unknown
// construction property outright rather than ignoring it, so carrying both
// would throw on every Shell instead of none.
for (const [label, hasOrientation, forbidden] of [
    ['modern', true, 'vertical'],
    ['pre-47', false, 'orientation'],
]) {
    const props = orientationProps(hasOrientation, true, ORIENTATION);
    check(`${label} box omits '${forbidden}'`, !(forbidden in props));
    eq(`${label} box sets exactly one property`, Object.keys(props).length, 1);
}

print(failures === 0 ? 'PASS shellCompat.test.js' : `FAIL shellCompat.test.js (${failures})`);
System.exit(failures === 0 ? 0 : 1);
