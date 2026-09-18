// coverage.test.js — GJS contract test for the checked-in state→channel
// coverage matrix (feature 011-accessible-dictation-ux,
// contracts/coverage-matrix.md C1-C3, C5).
//
//     gjs -m test/coverage.test.js        (from any directory)
//
// exits 0 when every guarantee holds, 1 otherwise. No Shell / no D-Bus needed.

import GLib from 'gi://GLib';
import System from 'system';

import {checkExhaustive, checkInvariants, loadMatrixFromPath} from './coverage.js';

let failures = 0;

function check(name, condition) {
    if (condition)
        print(`ok   ${name}`);
    else {
        failures++;
        print(`FAIL ${name}`);
    }
}

// Resolve against this module, not the cwd: the coverage run invokes gjs from
// / so that its --coverage-prefix matches, and a bare relative path misses.
const here = GLib.path_get_dirname(GLib.filename_from_uri(import.meta.url)[0]);
const matrix = loadMatrixFromPath(GLib.build_filenamev([here, '..', 'coverage-matrix.json']));

// --- C1-C3: every entry has both channel kinds, no colour/sound-only ------
check('the checked-in matrix has no invariant violations',
    checkInvariants(matrix).length === 0);

// --- C5: every known DictationState id has a matrix entry -----------------
check('every known state id has a matrix entry',
    checkExhaustive(matrix).length === 0);

// --- Invariant-checker unit behaviour (independent of the real file) ------
const emptyVisual = {
    states: [{id: 'idle', channels: {visual: [], non_visual: ['x']}, colour_only: false, sound_only: false}],
};
check('flags an entry with empty visual channels',
    checkInvariants(emptyVisual).some(v => v.type === 'empty_visual' && v.id === 'idle'));

const colourOnly = {
    states: [{id: 'idle', channels: {visual: ['x'], non_visual: ['y']}, colour_only: true, sound_only: false}],
};
check('flags a colour-only entry',
    checkInvariants(colourOnly).some(v => v.type === 'colour_only' && v.id === 'idle'));

const missingState = {states: []};
check('flags every missing state id in an empty matrix',
    checkExhaustive(missingState).length > 0);

System.exit(failures > 0 ? 1 : 0);
