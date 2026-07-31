// accent.test.js — GJS contract test for the pure accent-color/reduced-
// motion resolution (feature 004-gnome-shell-indicator, 2026-07-30
// wave-ribbon redesign; contract extension.md X25/X26). No Shell import;
// exercises the pure resolvers directly (the live Gio.Settings-backed
// `SystemPreferences` class is lifecycle plumbing analogous to dbus.js,
// not re-tested here — same harness-tier split as the rest of the bundle).
//
//     gjs -m test/accent.test.js        (from extensions/myna-shell/)

import System from 'system';

import {
    resolveAccentPalette,
    resolveReducedMotion,
    derivePalette,
    ACCENT_HEX,
    UBUNTU_ORANGE,
    UBUNTU_AUBERGINE,
} from '../accent.js';

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

// --- X25: null (untouched default OR schema/key-absent) → Ubuntu orange --

{
    const fallback = resolveAccentPalette(null);
    eq('X25 null user-value → Ubuntu-orange main', fallback.main, UBUNTU_ORANGE);
    eq('X25 the fallback itself counts as "orange" → aubergine complement',
        fallback.darkerComplement, UBUNTU_AUBERGINE);

    const fallbackUndefined = resolveAccentPalette(undefined);
    eq('X25 undefined behaves the same as null (schema-absent safety)',
        fallbackUndefined.main, UBUNTU_ORANGE);

    const unknown = resolveAccentPalette('not-a-real-accent-name');
    eq('X25 an unrecognized name never throws — falls back to Ubuntu orange',
        unknown.main, UBUNTU_ORANGE);
}

// --- X25: a genuine user choice (including 'blue', same nick as default) --

{
    const blue = resolveAccentPalette('blue');
    eq('X25 explicit blue resolves to the libadwaita blue, NOT the fallback',
        blue.main, ACCENT_HEX.blue);
    check('X25 explicit blue is distinguishable from the untouched-default fallback',
        blue.main !== resolveAccentPalette(null).main);

    for (const name of Object.keys(ACCENT_HEX)) {
        const palette = resolveAccentPalette(name);
        eq(`X25 ${name} resolves to its own hex table entry`, palette.main, ACCENT_HEX[name]);
    }
}

// --- The reinstated design-brief rule: aubergine iff orange, else a true -
// --- colour-wheel complement for every other accent.                    -

{
    const orange = resolveAccentPalette('orange');
    eq('orange\'s darker-complement is the fixed Ubuntu aubergine',
        orange.darkerComplement, UBUNTU_AUBERGINE);

    for (const name of Object.keys(ACCENT_HEX)) {
        if (name === 'orange')
            continue;
        const palette = resolveAccentPalette(name);
        check(`${name}'s darker-complement is NOT the aubergine override`,
            palette.darkerComplement !== UBUNTU_AUBERGINE);
    }
}

// --- derivePalette: highlight is lighter than main, translucent alpha ok -

{
    const palette = derivePalette(ACCENT_HEX.blue, false);
    check('highlight tone differs from main (lighter)', palette.highlight !== palette.main);
    check('translucentAlpha is a valid alpha in [0,1]',
        palette.translucentAlpha >= 0 && palette.translucentAlpha <= 1);
}

// --- X26: reduced-motion query never throws, defaults to full motion ----

{
    eq('X26 enable-animations=false → reduced motion', resolveReducedMotion(false), true);
    eq('X26 enable-animations=true → full motion', resolveReducedMotion(true), false);
    eq('X26 schema/key absent (null) → defaults to full motion', resolveReducedMotion(null), false);
    eq('X26 undefined also defaults to full motion', resolveReducedMotion(undefined), false);
}

print(failures === 0 ? 'PASS accent.test.js' : `FAIL accent.test.js: ${failures} failure(s)`);
System.exit(failures === 0 ? 0 : 1);
