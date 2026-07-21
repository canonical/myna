// states.test.js — GJS contract test for the pure state → visual-intent
// mapping (feature 004-gnome-shell-indicator, contracts extension.md X1–X4,
// X6; data-model "State → visual-intent mapping"). Harness-tier: run with
//
//     gjs -m test/states.test.js        (from extensions/myna-shell/)
//
// exits 0 when every guarantee holds, 1 otherwise. No Shell / no D-Bus needed.

import System from 'system';

import {stateToIntent} from '../states.js';

let failures = 0;

function check(name, condition) {
    if (condition) {
        print(`ok   ${name}`);
    } else {
        failures++;
        print(`FAIL ${name}`);
    }
}

function eq(name, actual, expected) {
    check(`${name} (got ${JSON.stringify(actual)})`, actual === expected);
}

// --- X1: every known State maps to its visual-intent record (E-mapping) ----

const EXPECTED = {
    loading: {
        cssClass: 'myna-goop-loading',
        animation: 'breathe',
        a11yLabel: 'Dictation: loading model',
    },
    recording: {
        cssClass: 'myna-goop-recording',
        animation: 'ripple',
        a11yLabel: 'Dictation: listening',
    },
    transcribing: {
        cssClass: 'myna-goop-transcribing',
        animation: 'shimmer',
        a11yLabel: 'Dictation: transcribing',
    },
    finalizing: {
        cssClass: 'myna-goop-finalizing',
        animation: 'flash',
        a11yLabel: 'Dictation: finishing',
    },
    error: {
        cssClass: 'myna-goop-error',
        animation: 'shake',
        a11yLabel: 'Dictation: error',
    },
};

for (const [state, want] of Object.entries(EXPECTED)) {
    let intent;
    let threw = false;
    try {
        intent = stateToIntent(state);
    } catch (e) {
        threw = true;
        print(`     stateToIntent(${state}) threw: ${e}`);
    }
    check(`X1 ${state}: does not throw`, !threw);
    if (threw)
        continue;
    eq(`X1 ${state}: cssClass`, intent.cssClass, want.cssClass);
    eq(`X1 ${state}: animation`, intent.animation, want.animation);
    eq(`X1 ${state}: a11yLabel`, intent.a11yLabel, want.a11yLabel);
    check(`X1 ${state}: visible (not hidden)`, intent.hidden === false);
}

// Error with a reason surfaces it in the a11y label (E3; stays content-free —
// the reason is a user-facing state string, never transcript).
{
    const intent = stateToIntent('error', 'no text field is focused');
    eq('X1 error+reason: a11yLabel', intent.a11yLabel,
        'Dictation: error — no text field is focused');
}

// --- X2: unknown State → neutral "active" intent, never throws -------------

for (const bogus of ['quantizing', '', 'RECORDING', 'idle ']) {
    let intent;
    let threw = false;
    try {
        intent = stateToIntent(bogus);
    } catch (e) {
        threw = true;
        print(`     stateToIntent(${JSON.stringify(bogus)}) threw: ${e}`);
    }
    check(`X2 unknown ${JSON.stringify(bogus)}: does not throw`, !threw);
    if (threw)
        continue;
    eq(`X2 unknown ${JSON.stringify(bogus)}: cssClass`, intent.cssClass,
        'myna-goop-active');
    eq(`X2 unknown ${JSON.stringify(bogus)}: animation`, intent.animation,
        'pulse');
    eq(`X2 unknown ${JSON.stringify(bogus)}: a11yLabel`, intent.a11yLabel,
        'Dictation: active');
    check(`X2 unknown ${JSON.stringify(bogus)}: visible`, intent.hidden === false);
}

// --- X3: idle → hidden (no actor; push-to-talk) ----------------------------

{
    const intent = stateToIntent('idle');
    check('X3 idle: hidden', intent.hidden === true);
}

// --- X4: loading and recording are distinct intents -------------------------

{
    const loading = stateToIntent('loading');
    const recording = stateToIntent('recording');
    check('X4 loading ≠ recording: cssClass',
        loading.cssClass !== recording.cssClass);
    check('X4 loading ≠ recording: animation',
        loading.animation !== recording.animation);
    check('X4 loading ≠ recording: a11yLabel',
        loading.a11yLabel !== recording.a11yLabel);
}

// --- X6: mapping output is state + level only — no content channel ---------

{
    const KEYS = ['cssClass', 'animation', 'a11yLabel', 'hidden'].sort();
    let onlyIntentKeys = true;
    for (const state of [...Object.keys(EXPECTED), 'idle', 'bogus']) {
        const keys = Object.keys(stateToIntent(state)).sort();
        if (JSON.stringify(keys) !== JSON.stringify(KEYS))
            onlyIntentKeys = false;
    }
    check('X6 intent records carry only cssClass/animation/a11yLabel/hidden',
        onlyIntentKeys);

    // Labels are fixed state strings; nothing caller-supplied can flow into a
    // non-error label.
    check('X6 non-error labels are fixed', stateToIntent('recording',
        'hello world this is a transcript').a11yLabel === 'Dictation: listening');
}

print(failures === 0 ? 'PASS states.test.js' : `FAIL states.test.js: ${failures} failure(s)`);
System.exit(failures === 0 ? 0 : 1);
