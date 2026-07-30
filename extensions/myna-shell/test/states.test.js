// states.test.js — GJS contract test for the pure state → semantic descriptor
// mapping (feature 004-gnome-shell-indicator, contract extension.md X1–X4,
// X6, X19 [2026-07-30 HUD redesign: severity replaces isError]).
//
//     gjs -m test/states.test.js        (from extensions/myna-shell/)
//
// exits 0 when every guarantee holds, 1 otherwise. No Shell / no D-Bus needed.

import System from 'system';

import {stateToDescriptor} from '../states.js';

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

// --- X1: every known State maps to its semantic descriptor -----------------
// (2026-07-30: `severity` replaces the old boolean `isError` — X19)

const EXPECTED = {
    loading: {key: 'loading', statusText: 'Loading model…', severity: null},
    recording: {key: 'recording', statusText: 'Listening', severity: null},
    transcribing: {key: 'transcribing', statusText: 'Transcribing', severity: null},
    finalizing: {key: 'finalizing', statusText: 'Finishing', severity: null},
    notice: {key: 'notice', statusText: 'No speech detected', severity: 'recoverable'},
    error: {key: 'error', statusText: 'Error', severity: 'critical'},
};

for (const [state, want] of Object.entries(EXPECTED)) {
    let d;
    let threw = false;
    try {
        d = stateToDescriptor(state);
    } catch (e) {
        threw = true;
        print(`     stateToDescriptor(${state}) threw: ${e}`);
    }
    check(`X1 ${state}: does not throw`, !threw);
    if (threw)
        continue;
    eq(`X1 ${state}: key`, d.key, want.key);
    if (state !== 'notice') // notice's default reason is asserted separately below
        eq(`X1 ${state}: statusText`, d.statusText, want.statusText);
    check(`X1 ${state}: visible`, d.hidden === false);
    eq(`X19 ${state}: severity`, d.severity, want.severity);
}

// Error with a reason surfaces it in the status text (content-free, E3).
{
    const d = stateToDescriptor('error', 'no audio source available');
    eq('X1 error+reason: statusText', d.statusText, 'Error — no audio source available');
    eq('X19 error+reason: severity', d.severity, 'critical');
}

// Notice with a reason surfaces it directly as the status text (no "Error —"
// prefix — it isn't an error), and is 'recoverable' severity (X19).
{
    const d = stateToDescriptor('notice', 'No speech detected');
    eq('X19 notice+reason: statusText', d.statusText, 'No speech detected');
    eq('X19 notice+reason: severity', d.severity, 'recoverable');
}

// --- X2: unknown State → neutral "active" descriptor, never throws ---------

for (const bogus of ['quantizing', '', 'RECORDING', 'idle ']) {
    let d;
    let threw = false;
    try {
        d = stateToDescriptor(bogus);
    } catch (e) {
        threw = true;
        print(`     stateToDescriptor(${JSON.stringify(bogus)}) threw: ${e}`);
    }
    check(`X2 unknown ${JSON.stringify(bogus)}: does not throw`, !threw);
    if (threw)
        continue;
    eq(`X2 unknown ${JSON.stringify(bogus)}: key`, d.key, 'active');
    eq(`X2 unknown ${JSON.stringify(bogus)}: severity`, d.severity, null);
    check(`X2 unknown ${JSON.stringify(bogus)}: visible`, d.hidden === false);
}

// --- X3: idle → hidden (no actor; push-to-talk) ----------------------------

{
    const d = stateToDescriptor('idle');
    check('X3 idle: hidden', d.hidden === true);
    check('X3 null: hidden', stateToDescriptor(null).hidden === true);
}

// --- X4: loading and recording are distinct --------------------------------

{
    const loading = stateToDescriptor('loading');
    const recording = stateToDescriptor('recording');
    check('X4 loading ≠ recording: key', loading.key !== recording.key);
    check('X4 loading ≠ recording: statusText',
        loading.statusText !== recording.statusText);
}

// --- X19: notice and error are mutually exclusive severities ---------------

{
    const notice = stateToDescriptor('notice');
    const error = stateToDescriptor('error');
    check('X19 notice ≠ error: key', notice.key !== error.key);
    check('X19 notice severity is recoverable', notice.severity === 'recoverable');
    check('X19 error severity is critical', error.severity === 'critical');
    check('X19 severities differ', notice.severity !== error.severity);
    // Every other known state has no severity.
    for (const state of ['loading', 'recording', 'transcribing', 'finalizing']) {
        check(`X19 ${state}: no severity`, stateToDescriptor(state).severity === null);
    }
}

// --- X6: descriptor carries only state + content-free status ---------------
// (2026-07-30: `severity` replaces `isError` in the allowed key set)

{
    const KEYS = ['hidden', 'severity', 'key', 'statusText'].sort();
    let onlyDescriptorKeys = true;
    for (const state of [...Object.keys(EXPECTED), 'bogus']) {
        const keys = Object.keys(stateToDescriptor(state)).sort();
        if (JSON.stringify(keys) !== JSON.stringify(KEYS))
            onlyDescriptorKeys = false;
    }
    check('X6 descriptors carry only key/statusText/severity/hidden',
        onlyDescriptorKeys);
    // Non-problem status text is fixed; caller text cannot flow into it.
    eq('X6 non-problem status is fixed',
        stateToDescriptor('recording', 'a transcript here').statusText,
        'Listening');
}

print(failures === 0 ? 'PASS states.test.js' : `FAIL states.test.js: ${failures} failure(s)`);
System.exit(failures === 0 ? 0 : 1);
