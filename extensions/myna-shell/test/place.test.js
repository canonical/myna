// place.test.js — GJS contract test for the hosted window's placement math
// (feature 004-gnome-shell-indicator, 2026-08-26 architecture revision;
// contract extension.md XH1, FR-004). No Shell / no D-Bus.
//
//     gjs -m test/place.test.js        (from extensions/myna-shell/)

import System from 'system';

import {BOTTOM_MARGIN, computePlacement, placementChanged} from '../place.js';

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

// A typical primary monitor with a 32px top panel.
const WORK_AREA = {x: 0, y: 32, width: 1920, height: 1048};
const PILL = {width: 360, height: 72};

// --- XH1: bottom-centred on the work area ---------------------------------

{
    const p = computePlacement(WORK_AREA, PILL);
    eq('XH1 horizontally centred', p.x, (1920 - 360) / 2);
    eq('XH1 sits BOTTOM_MARGIN above the work area bottom',
        p.y, WORK_AREA.y + WORK_AREA.height - PILL.height - BOTTOM_MARGIN);
    check('XH1 inside the work area horizontally',
        p.x >= WORK_AREA.x && p.x + PILL.width <= WORK_AREA.x + WORK_AREA.width);
    check('XH1 inside the work area vertically',
        p.y >= WORK_AREA.y && p.y + PILL.height <= WORK_AREA.y + WORK_AREA.height);
}

// --- XH1: the work area's own origin is respected (multi-monitor) ---------

{
    // A second monitor to the right, with its own panel offset.
    const right = {x: 1920, y: 0, width: 2560, height: 1440};
    const p = computePlacement(right, PILL);
    check('XH1 placement follows the monitor origin', p.x >= right.x);
    eq('XH1 centred on that monitor', p.x, right.x + (right.width - PILL.width) / 2);
    eq('XH1 bottom-anchored on that monitor',
        p.y, right.y + right.height - PILL.height - BOTTOM_MARGIN);
}

// --- XH1: never off-screen, even when the window does not fit ------------

{
    const tiny = {x: 100, y: 50, width: 200, height: 100};
    const p = computePlacement(tiny, PILL);
    check('a too-wide window is clamped to the work-area origin, not pushed off-screen',
        p.x >= tiny.x);
    check('a too-tall window is clamped to the work-area origin, not pushed off-screen',
        p.y >= tiny.y);
}

// --- XH1: recomputes when any input changes ------------------------------

{
    const base = computePlacement(WORK_AREA, PILL);
    const resized = computePlacement(WORK_AREA, {width: 420, height: 72});
    const moved = computePlacement({...WORK_AREA, height: 900}, PILL);
    check('a window size change moves the placement', base.x !== resized.x);
    check('a work-area change moves the placement', base.y !== moved.y);
    check('identical inputs are stable (no churn)',
        placementChanged(base, computePlacement(WORK_AREA, PILL)) === false);
    check('a changed placement is reported as changed',
        placementChanged(base, resized) === true);
}

print(failures === 0 ? 'PASS place.test.js' : `FAIL place.test.js: ${failures} failure(s)`);
System.exit(failures === 0 ? 0 : 1);
