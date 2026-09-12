// resolve.test.js — GJS contract test for the renderer-launch resolution
// (feature 004-gnome-shell-indicator, 2026-08-26 architecture revision;
// contract extension.md XH2, FR-027). No Shell / no filesystem: the
// environment and the executability predicate are injected.
//
//     gjs -m test/resolve.test.js        (from extensions/myna-shell/)

import System from 'system';

import {OVERRIDE_ENV, SNAP_APP, SNAP_LAUNCHER, resolveHudLaunch} from '../resolve.js';

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

function deepEq(name, actual, expected) {
    check(
        `${name} (got ${JSON.stringify(actual)})`,
        JSON.stringify(actual) === JSON.stringify(expected));
}

function fakes({env = {}, executable = []} = {}) {
    return {
        getenv: name => env[name] ?? null,
        isExecutable: path => executable.includes(path),
    };
}

// --- XH2: the packaged renderer is launched via `snap run myna.hud` ------

{
    const snap = resolveHudLaunch(fakes({executable: [SNAP_LAUNCHER]}));
    deepEq('XH2 the packaged renderer runs through snap',
        snap.argv, [SNAP_LAUNCHER, 'run', SNAP_APP]);
    eq('XH2 reported as the snap source', snap.source, 'snap');
    eq('XH2 the snap app is the dotted app name', SNAP_APP, 'myna.hud');

    // `snap` found by absolute path rather than on PATH also works.
    const snapAbs = resolveHudLaunch(fakes({executable: ['/usr/bin/snap']}));
    deepEq('XH2 snap resolved by absolute path too',
        snapAbs.argv, [SNAP_LAUNCHER, 'run', SNAP_APP]);
}

// --- XH2: the developer override outranks the packaged command -----------

{
    const override = resolveHudLaunch(fakes({
        env: {[OVERRIDE_ENV]: '/home/dev/target/debug/myna-hud'},
        executable: ['/home/dev/target/debug/myna-hud', SNAP_LAUNCHER],
    }));
    deepEq('XH2 the developer override outranks the snap',
        override.argv, ['/home/dev/target/debug/myna-hud']);
    eq('XH2 reported as an override', override.source, 'override');
}

// --- XH2: failure states are bounded, never exceptions -------------------

{
    const nothing = resolveHudLaunch(fakes({}));
    eq('XH2 nothing installed → no argv', nothing.argv, null);
    eq('XH2 nothing installed → reported missing', nothing.source, 'missing');

    // A set-but-broken override is a configuration error: it must NOT
    // silently fall back to the packaged snap the developer did not ask
    // for, or a stale snap would shadow the build under test.
    const brokenOverride = resolveHudLaunch(fakes({
        env: {[OVERRIDE_ENV]: '/nope/myna-hud'},
        executable: [SNAP_LAUNCHER],
    }));
    eq('XH2 a broken override does not silently fall back', brokenOverride.argv, null);
    eq('XH2 a broken override is reported missing', brokenOverride.source, 'missing');

    const emptyOverride = resolveHudLaunch(fakes({
        env: {[OVERRIDE_ENV]: ''},
        executable: [SNAP_LAUNCHER],
    }));
    deepEq('XH2 an empty override is ignored (unset semantics)',
        emptyOverride.argv, [SNAP_LAUNCHER, 'run', SNAP_APP]);
}

print(failures === 0 ? 'PASS resolve.test.js' : `FAIL resolve.test.js: ${failures} failure(s)`);
System.exit(failures === 0 ? 0 : 1);
