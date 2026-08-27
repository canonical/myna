#!/usr/bin/env -S gjs -m
// compat-probe.js — check shellCompat.js's capability detection against the
// St and Meta this machine actually has (2026-08-27 Shell 46 backport).
//
// This is NOT a unit test. test/shellCompat.test.js covers the pure choice
// (`motionSource`) headlessly; what cannot be covered that way is whether
// the *detection* agrees with the real introspection data — and that is
// precisely what broke the extension on Shell 46. `notify::accent-color` on
// an St.Settings without an `accent-color` property does not degrade, it
// throws, and it throws inside the ribbon actor's constructor: the HUD is
// simply gone. A detection that says "yes" on a Shell that means "no" is
// therefore the whole failure mode, and only a real typelib can catch it.
//
// No display server needed: St.Settings.get() is reachable without a
// compositor, and Meta's unredirect entry points are inspected, not called
// (they need a live display).
//
//     GI_TYPELIB_PATH=/usr/lib/gnome-shell:/usr/lib/x86_64-linux-gnu/mutter-14 \
//     LD_LIBRARY_PATH=... gjs -m test/compat-probe.js
//
// Exit codes follow gpu-probe.sh: 0 pass, 1 fail, 77 cannot judge.

import System from 'system';

import Meta from 'gi://Meta';
import St from 'gi://St';

import {
    boxOrientation,
    motionSource,
    stSettingsCapabilities,
} from '../shellCompat.js';

let failures = 0;

function check(name, condition, detail = '') {
    if (condition) {
        print(`ok   ${name}${detail ? ` ${detail}` : ''}`);
    } else {
        failures++;
        print(`FAIL ${name}${detail ? ` ${detail}` : ''}`);
    }
}

const settings = St.Settings.get();
if (settings === null) {
    print('compat-probe: no St.Settings on this machine; skipping');
    System.exit(77);
}

const caps = stSettingsCapabilities();
print(`# St.Settings: accentColor=${caps.accentColor} ` +
      `reducedMotion=${caps.reducedMotion}`);

// The gate itself being wrong is the one thing this file can catch.
check('accentColor detection matches introspection',
    caps.accentColor === ('accentColor' in settings));
check('reducedMotion detection matches introspection',
    caps.reducedMotion === ('reducedMotion' in settings));

// Whichever branch motionSource picked, the property it reads and the signal
// it connects must both exist here — a wrong pick is a throw at construction.
const motion = motionSource(caps);
const motionProp = caps.reducedMotion ? 'reducedMotion' : 'enableAnimations';
check(`motion source '${motion.signal}' reads an existing property`,
    motionProp in settings, `(${motionProp})`);
check('reading reduced motion returns a boolean',
    typeof motion.read(settings) === 'boolean');

// Connecting is the operation that actually threw on Shell 46, so connect.
try {
    const id = settings.connect(motion.signal, () => {});
    settings.disconnect(id);
    print(`ok   ${motion.signal} connects`);
} catch (e) {
    failures++;
    print(`FAIL ${motion.signal} connects: ${e.message}`);
}

if (caps.accentColor) {
    try {
        const id = settings.connect('notify::accent-color', () => {});
        settings.disconnect(id);
        print('ok   notify::accent-color connects');
    } catch (e) {
        failures++;
        print(`FAIL notify::accent-color connects: ${e.message}`);
    }
} else {
    // The complement of the above: where detection says no, it must really
    // be absent, or the pre-47 palette fallback is masking a live property.
    check('accent-color is genuinely absent where detection says so',
        !('accentColor' in settings));
}

// St.BoxLayout's direction property. Constructing one needs a compositor,
// so check the spelling against introspection instead — which is exactly
// what went wrong: `orientation` on a Shell 46 BoxLayout throws out of the
// constructor and the pill is never built.
print(`# St.BoxLayout: orientation=${caps.boxOrientation}`);
check('boxOrientation detection matches introspection',
    caps.boxOrientation === ('orientation' in St.BoxLayout.prototype));
for (const vertical of [true, false]) {
    const props = boxOrientation(vertical);
    const key = Object.keys(props)[0];
    check(`boxOrientation(${vertical}) picks a property St.BoxLayout has`,
        Object.keys(props).length === 1 && key in St.BoxLayout.prototype,
        `(${key})`);
}

// setUnredirectDisabled cannot be *called* without a display, but exactly
// one of the two spellings it chooses between must exist here.
const hasCompositorUnredirect =
    typeof Meta.Compositor?.prototype?.disable_unredirect === 'function';
const hasDisplayUnredirect =
    typeof Meta.disable_unredirect_for_display === 'function';
print(`# unredirect: compositor=${hasCompositorUnredirect} ` +
      `display=${hasDisplayUnredirect}`);
check('an unredirect API this Shell offers is reachable',
    hasCompositorUnredirect || hasDisplayUnredirect);

print(failures === 0 ? 'PASS compat-probe.js' : `FAIL compat-probe.js (${failures})`);
System.exit(failures === 0 ? 0 : 1);
