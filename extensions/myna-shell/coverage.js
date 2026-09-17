// coverage.js — PURE checks over the checked-in state→channel coverage
// matrix (feature 011-accessible-dictation-ux, contracts/coverage-matrix.md
// C1-C5). No Shell/D-Bus dependency; `loadMatrixFromPath` is the only impure
// piece (file I/O), kept separate so the checks themselves are trivially
// unit-testable against an in-memory object.

import GLib from 'gi://GLib';
import {DictationState} from './states.js';

/**
 * @param {string} path
 * @returns {object} the parsed coverage-matrix.json
 */
export function loadMatrixFromPath(path) {
    const [ok, contents] = GLib.file_get_contents(path);
    if (!ok)
        throw new Error(`could not read ${path}`);
    return JSON.parse(new TextDecoder().decode(contents));
}

/**
 * C1-C3: every entry has a non-empty visual and non-visual channel list,
 * and neither colour_only nor sound_only is true.
 *
 * @param {object} matrix
 * @returns {Array<{type: string, id: string}>} violations, empty if none
 */
export function checkInvariants(matrix) {
    const violations = [];
    for (const entry of matrix.states) {
        if (!entry.channels?.visual?.length)
            violations.push({type: 'empty_visual', id: entry.id});
        if (!entry.channels?.non_visual?.length)
            violations.push({type: 'empty_non_visual', id: entry.id});
        if (entry.colour_only)
            violations.push({type: 'colour_only', id: entry.id});
        if (entry.sound_only)
            violations.push({type: 'sound_only', id: entry.id});
    }
    return violations;
}

// ACTIVE is states.js's fallback descriptor for unknown/additive states, not
// itself a wire state — excluded from the exhaustiveness check.
const KNOWN_STATE_IDS = Object.values(DictationState).filter(
    id => id !== DictationState.ACTIVE);

/**
 * C5: every known states.js DictationState id has a matching matrix entry.
 *
 * @param {object} matrix
 * @returns {string[]} missing state ids, empty if none
 */
export function checkExhaustive(matrix) {
    const present = new Set(matrix.states.map(s => s.id));
    return KNOWN_STATE_IDS.filter(id => !present.has(id));
}
