// vumeter.test.js — GJS contract test for the pure VU logic (feature 004,
// contract extension.md X5, SC-004). No Shell / no D-Bus.
//
//     gjs -m test/vumeter.test.js        (from extensions/myna-shell/)

import System from 'system';

import {levelToIntensity, levelToBars, STALE_MS} from '../vumeter.js';

let failures = 0;

function check(name, condition) {
    if (condition)
        print(`ok   ${name}`);
    else {
        failures++;
        print(`FAIL ${name}`);
    }
}

// --- X5: intensity is monotonic, clamped, and decays when stale ------------

check('X5 louder → higher (monotonic)',
    levelToIntensity(0.2, 0) < levelToIntensity(0.8, 0));
check('X5 clamps above 1', levelToIntensity(5.0, 0) <= 1.0);
check('X5 clamps below 0', levelToIntensity(-3.0, 0) >= 0.0);
check('X5 NaN is safe', levelToIntensity(NaN, 0) >= 0.0);

// Fresh loud vs stale loud: stale decays to (near) the floor.
{
    const fresh = levelToIntensity(0.9, 0);
    const halfStale = levelToIntensity(0.9, STALE_MS / 2);
    const stale = levelToIntensity(0.9, STALE_MS + 50);
    check('X5 decays across the stale window', fresh > halfStale && halfStale > stale);
    check('X5 stale reaches the floor', stale <= levelToIntensity(0.0, 0) + 1e-9);
}

// --- bar profile ------------------------------------------------------------

{
    const bars = levelToBars(0.8, 0, 24);
    check('bars: correct count', bars.length === 24);
    check('bars: all within [0,1]', bars.every(b => b >= 0 && b <= 1));
    // Centre is tallest, edges shortest (spindle shape).
    check('bars: centre ≥ edge', bars[12] > bars[0] && bars[12] > bars[23]);
    check('bars: symmetric', Math.abs(bars[0] - bars[23]) < 1e-9);

    const quiet = levelToBars(0.05, 0, 24);
    const loud = levelToBars(0.95, 0, 24);
    check('bars: louder is taller at centre', loud[12] > quiet[12]);

    const stale = levelToBars(0.95, STALE_MS + 100, 24);
    check('bars: stale collapses toward floor', stale[12] < loud[12]);

    // Degenerate bar counts don't throw.
    check('bars: barCount=1 ok', levelToBars(0.5, 0, 1).length === 1);
}

// --- X6: no content in outputs (numbers only) ------------------------------

check('X6 outputs are numbers only',
    typeof levelToIntensity(0.5, 0) === 'number' &&
    levelToBars(0.5, 0, 8).every(b => typeof b === 'number'));

print(failures === 0 ? 'PASS vumeter.test.js' : `FAIL vumeter.test.js: ${failures} failure(s)`);
System.exit(failures === 0 ? 0 : 1);
