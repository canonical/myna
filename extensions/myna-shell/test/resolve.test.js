// resolve.test.js — GJS contract test for the renderer-binary resolution
// (feature 004-gnome-shell-indicator, 2026-08-26 architecture revision;
// contract extension.md XH2, FR-027). No Shell / no filesystem: the
// environment and the executability predicate are injected.
//
//     gjs -m test/resolve.test.js        (from extensions/myna-shell/)

import System from 'system';

import {CANDIDATE_PATHS, OVERRIDE_ENV, resolveHudBinary} from '../resolve.js';

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

function fakes({env = {}, executable = []} = {}) {
    return {
        getenv: name => env[name] ?? null,
        isExecutable: path => executable.includes(path),
    };
}

// --- XH2: the documented order, first hit wins ---------------------------

{
    const both = resolveHudBinary(fakes({executable: CANDIDATE_PATHS}));
    eq('XH2 the snap command wins over the system path', both.path, '/snap/bin/myna-hud');
    eq('XH2 reported as a candidate', both.source, 'candidate');

    const systemOnly = resolveHudBinary(fakes({executable: ['/usr/bin/myna-hud']}));
    eq('XH2 falls through to the system path', systemOnly.path, '/usr/bin/myna-hud');

    const override = resolveHudBinary(fakes({
        env: {[OVERRIDE_ENV]: '/home/dev/target/debug/myna-hud'},
        executable: ['/home/dev/target/debug/myna-hud', ...CANDIDATE_PATHS],
    }));
    eq('XH2 the developer override outranks everything',
        override.path, '/home/dev/target/debug/myna-hud');
    eq('XH2 reported as an override', override.source, 'override');
}

// --- XH2: failure states are bounded, never exceptions -------------------

{
    const nothing = resolveHudBinary(fakes({}));
    eq('XH2 nothing installed → no path', nothing.path, null);
    eq('XH2 nothing installed → reported missing', nothing.source, 'missing');

    // A set-but-broken override is a configuration error: it must NOT
    // silently fall back to a packaged binary the developer did not ask
    // for, or a stale snap would shadow the build under test.
    const brokenOverride = resolveHudBinary(fakes({
        env: {[OVERRIDE_ENV]: '/nope/myna-hud'},
        executable: CANDIDATE_PATHS,
    }));
    eq('XH2 a broken override does not silently fall back', brokenOverride.path, null);
    eq('XH2 a broken override is reported missing', brokenOverride.source, 'missing');

    const emptyOverride = resolveHudBinary(fakes({
        env: {[OVERRIDE_ENV]: ''},
        executable: ['/usr/bin/myna-hud'],
    }));
    eq('XH2 an empty override is ignored (unset semantics)',
        emptyOverride.path, '/usr/bin/myna-hud');
}

print(failures === 0 ? 'PASS resolve.test.js' : `FAIL resolve.test.js: ${failures} failure(s)`);
System.exit(failures === 0 ? 0 : 1);
