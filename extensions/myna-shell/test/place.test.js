// place.test.js — GJS contract test for the hosted window's placement math
// (feature 004-gnome-shell-indicator, 2026-08-26 architecture revision;
// contract extension.md XH1, FR-004). No Shell / no D-Bus.
//
//     gjs -m test/place.test.js        (from extensions/myna-shell/)

import System from 'system';

import {BOTTOM_MARGIN, computePlacement, placementChanged, shrinkWorkAreaForDock} from '../place.js';

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

// --- Shrinking for a bottom overlay dock (dash-to-dock auto-hide) -------
// An auto-hide dock claims no strut, so its extent is NOT in the work area;
// the host raises the work area's bottom to the dock's top edge so the pill
// is never covered when the dock slides out. `side` is the raw St.Side enum
// value (the consumer passes St.Side.BOTTOM as `bottomSide`); a numeric
// stand-in is used here since this module is gi-free.
const BOTTOM_SIDE = 2; // St.Side.BOTTOM
const LEFT_SIDE = 3;   // St.Side.LEFT

{
    const workArea = {x: 0, y: 0, width: 1920, height: 1032};

    // A bottom dock whose top edge is at y=950 reserves the bottom 82px.
    const dock = {
        side: BOTTOM_SIDE, x: 480, y: 950, width: 960, height: 82,
        affectsStruts: false,
    };
    const shrunk = shrinkWorkAreaForDock(workArea, dock, BOTTOM_SIDE);
    check('a bottom dock raises the work area bottom to its top edge',
        shrunk.height === 950 && shrunk.y === 0);
    check('width and origin are untouched', shrunk.width === 1920);

    // The pill then sits above the dock, not under it.
    const placed = computePlacement(shrunk, {width: 360, height: 76});
    check('the pill ends up above the dock, plus the bottom margin',
        placed.y === 950 - 76 - BOTTOM_MARGIN);

    // A side dock (left) does not affect a bottom-anchored pill.
    const side = {side: LEFT_SIDE, x: 0, y: 100, width: 60, height: 832};
    check('a side dock leaves the work area unchanged',
        shrinkWorkAreaForDock(workArea, side, BOTTOM_SIDE).height === 1032);

    // No dock → unchanged.
    check('no dock → unchanged',
        shrinkWorkAreaForDock(workArea, null, BOTTOM_SIDE).height === 1032);

    // A dock that ALREADY claims a mutter strut (dock-fixed mode) must not
    // shrink again — the work area already excludes it.
    const fixed = {
        side: BOTTOM_SIDE, x: 0, y: 950, width: 1920, height: 82,
        affectsStruts: true,
    };
    check('an affectsStruts dock leaves the work area unchanged',
        shrinkWorkAreaForDock(workArea, fixed, BOTTOM_SIDE).height === 1032);

    // An extent whose top is already at/above the work area bottom must not
    // over-shrink (e.g. a stale or coincident extent).
    const stale = {side: BOTTOM_SIDE, x: 0, y: 1032, width: 1920, height: 82};
    check('an extent already above the work area bottom does not shrink',
        shrinkWorkAreaForDock(workArea, stale, BOTTOM_SIDE).height === 1032);
}

// A Meta.Rectangle work area is a BOXED struct: its fields are GObject
// properties, so object spread ({...rect}) copies NONE of them. The shrink
// must therefore read x/y/width explicitly, or it returns a broken
// {height}-only object that lands the pill at the top-left origin.
{
    // Simulate the boxed struct: enumerable only via property accessors,
    // like a GObject boxed whose props are not own enumerable properties.
    const boxedWorkArea = {};
    Object.defineProperties(boxedWorkArea, {
        x: {get: () => 0, enumerable: false},
        y: {get: () => 32, enumerable: false},
        width: {get: () => 1920, enumerable: false},
        height: {get: () => 1032, enumerable: false},
    });

    const dock = {
        side: BOTTOM_SIDE, x: 282, y: 726, width: 717, height: 74,
        affectsStruts: false,
    };
    const shrunk = shrinkWorkAreaForDock(boxedWorkArea, dock, BOTTOM_SIDE);
    check('a boxed-struct work area keeps all fields after shrinking',
        shrunk.x === 0 && shrunk.y === 32 && shrunk.width === 1920 &&
        shrunk.height === 694);
    // ...and the pill lands above the dock, not at the origin.
    const placed = computePlacement(shrunk, {width: 360, height: 76});
    check('a boxed-struct work area places the pill above the dock',
        placed.x === 780 && placed.y === 626);
}

print(failures === 0 ? 'PASS place.test.js' : `FAIL place.test.js: ${failures} failure(s)`);
System.exit(failures === 0 ? 0 : 1);
