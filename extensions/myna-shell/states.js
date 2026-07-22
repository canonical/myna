// states.js — PURE dictation-state → *semantic descriptor* (feature
// 004-gnome-shell-indicator; data-model E1). This is the STABLE layer: it maps
// the org.myna.Dictation wire State to a content-free, presentation-free
// descriptor. It says *what* the system is doing — never how to draw it.
// Renderers (view.js implementations) own all pixels: colour, geometry,
// animation. Swapping the look never touches this file.
//
// Unit-tested by test/states.test.js without a Shell. Inputs are the state
// string plus an optional content-free error reason; nothing here ever carries
// transcript text (constitution V, X6).

// The stable descriptor for each known State: a machine `key` (renderers switch
// on this) and a human, content-free `statusText` (shown to the user / read by
// Orca). Additive: unknown states fall through to ACTIVE, never throw (X2).
const DESCRIPTORS = {
    loading: {key: 'loading', statusText: 'Loading model…'},
    recording: {key: 'recording', statusText: 'Listening'},
    transcribing: {key: 'transcribing', statusText: 'Transcribing'},
    finalizing: {key: 'finalizing', statusText: 'Finishing'},
    error: {key: 'error', statusText: 'Error'},
};

// Unknown/extra states degrade to a neutral "active" descriptor (FR-008, X2).
const ACTIVE = {key: 'active', statusText: 'Active'};

// idle → nothing shown (push-to-talk, FR-002, X3).
const HIDDEN = {key: 'idle', statusText: '', hidden: true, isError: false};

/**
 * Map an org.myna.Dictation State string to a semantic descriptor
 * `{key, statusText, isError, hidden}`.
 *
 * @param {string} state - the wire State (idle|loading|recording|
 *     transcribing|finalizing|error, or an unknown additive value).
 * @param {string} [errorReason] - content-free reason appended to the error
 *     statusText (E3); ignored for every other state so caller text can never
 *     leak into a non-error status (X6).
 * @returns {{key: string, statusText: string, isError: boolean, hidden: boolean}}
 */
export function stateToDescriptor(state, errorReason = '') {
    if (state === 'idle' || state === null || state === undefined)
        return {...HIDDEN};

    const base = DESCRIPTORS[state] ?? ACTIVE;
    const isError = base.key === 'error';
    let statusText = base.statusText;
    if (isError && errorReason !== '')
        statusText = `Error — ${errorReason}`;
    return {key: base.key, statusText, isError, hidden: false};
}
