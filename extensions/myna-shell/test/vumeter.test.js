// vumeter.test.js — GJS contract test for the pure envelope logic (feature
// 004, contract extension.md X5, SC-004), reused unchanged by ribbon.js
// (2026-07-30 wave-ribbon redesign). No Shell / no D-Bus.
//
//     gjs -m test/vumeter.test.js        (from extensions/myna-shell/)

import System from 'system';

import {
    levelsToIntensity,
    STALE_MS,
} from '../vumeter.js';

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

check('X5 louder → higher in the speech range (monotonic)',
    levelsToIntensity(0.002, 0.002, 0) < levelsToIntensity(0.02, 0.02, 0));
check('X5 clamps above 1', levelsToIntensity(5.0, 5.0, 0) <= 1.0);
check('X5 clamps below 0', levelsToIntensity(-3.0, -3.0, 0) >= 0.0);
check('X5 NaN is safe', levelsToIntensity(NaN, NaN, 0) >= 0.0);

// Hardware calibration (Blackwire C5220, 2026-07-30): ordinary speech was
// RMS≈0.009 / peak≈0.025 (median), while room/device noise was
// RMS≈0.00003 / peak≈0.0001. Normal speech must occupy a clearly visible
// middle section of the meter instead of hugging its floor.
{
    const noise = levelsToIntensity(0.00003, 0.0001, 0);
    const normal = levelsToIntensity(0.009, 0.025, 0);
    const strong = levelsToIntensity(0.024, 0.067, 0);
    const overload = levelsToIntensity(0.05, 0.16, 0);
    check('Blackwire noise stays near meter floor', noise < 0.12);
    check('Blackwire normal speech clearly moves meter', normal >= 0.45 && normal <= 0.7);
    check('Blackwire strong speech reaches yellow zone', strong >= 0.65);
    check('near-overload speech reaches red zone', overload >= 0.85);
    check('combined level remains monotonic', noise < normal && normal < strong && strong < overload);
}

// Fresh loud vs stale loud: stale decays to (near) the floor.
{
    const fresh = levelsToIntensity(0.9, 0.9, 0);
    const halfStale = levelsToIntensity(0.9, 0.9, STALE_MS / 2);
    const stale = levelsToIntensity(0.9, 0.9, STALE_MS + 50);
    check('X5 decays across the stale window', fresh > halfStale && halfStale > stale);
    check('X5 stale reaches the floor', stale <= levelsToIntensity(0.0, 0.0, 0) + 1e-9);
}

// --- conventional loudness zones (still true of the underlying envelope,
// --- even though the wave ribbon no longer renders discrete colour zones) --

check('quiet input stays near the floor', levelsToIntensity(0.00003, 0.0001, 0) < 0.12);
check('normal speech clearly moves the envelope',
    (() => {
        const normal = levelsToIntensity(0.009, 0.025, 0);
        return normal >= 0.45 && normal <= 0.7;
    })());

// --- X6: no content in outputs (numbers only) ------------------------------

check('X6 outputs are numbers only',
    typeof levelsToIntensity(0.5, 0.5, 0) === 'number');

print(failures === 0 ? 'PASS vumeter.test.js' : `FAIL vumeter.test.js: ${failures} failure(s)`);
System.exit(failures === 0 ? 0 : 1);
