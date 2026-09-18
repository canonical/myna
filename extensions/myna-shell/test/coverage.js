// coverage.js — PURE checks over the checked-in state→channel coverage
// matrix (feature 011-accessible-dictation-ux, contracts/coverage-matrix.md
// C1-C5). No Shell/D-Bus dependency; `loadMatrixFromPath` is the only impure
// piece (file I/O), kept separate so the checks themselves are trivially
// unit-testable against an in-memory object.

import GLib from 'gi://GLib';

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

// The known wire state ids (feature 004's DictationState/states.js was
// removed in T170 when the in-Shell renderer was replaced by the standalone
// myna-hud application — see client/myna-hud/src/states.rs's `wire` module,
// the ported/canonical source of truth for these strings now). `active` is
// the fallback descriptor for an unknown/additive state, not itself a wire
// state — excluded from the exhaustiveness check, matching the old
// states.js's ACTIVE exclusion.
const KNOWN_STATE_IDS = [
    'loading',
    'recording',
    'transcribing',
    'finalizing',
    'notice',
    'error',
];

/**
 * C5: every known wire state id (see `KNOWN_STATE_IDS` above) has a matching
 * matrix entry.
 *
 * @param {object} matrix
 * @returns {string[]} missing state ids, empty if none
 */
export function checkExhaustive(matrix) {
    const present = new Set(matrix.states.map(s => s.id));
    return KNOWN_STATE_IDS.filter(id => !present.has(id));
}
