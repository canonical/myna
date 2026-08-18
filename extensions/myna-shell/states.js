// states.js — PURE dictation-state → *semantic descriptor* (feature
// 004-gnome-shell-indicator; data-model E1/E1a). This is the STABLE layer: it
// maps the org.myna.Dictation wire State to a content-free, presentation-free
// descriptor. It says *what* the system is doing — never how to draw it.
// Renderers (view.js implementations) own all pixels: colour, geometry,
// animation, icon choice. Swapping the look never touches this file.
//
// Unit-tested by test/states.test.js without a Shell. Inputs are the state
// string plus an optional content-free reason; nothing here ever carries
// transcript text (constitution V, X6).
//
// 2026-07-30 HUD redesign (data-model E1a, research R13): the wire gained a
// `notice` state alongside `error` — a **recoverable**, non-blocking issue
// (e.g. "no speech detected") distinct from a **critical** error (e.g.
// microphone unavailable). The descriptor's `severity` field
// (`'recoverable' | 'critical' | null`) replaces the old boolean `isError` so
// renderers can distinguish the two tiers; every other known state has
// `severity: null`.

// The stable descriptor for each known State: a machine `key` (renderers switch
// on this), a human, content-free `statusText` (shown to the user), and a
// `severity` (`'recoverable' | 'critical' | null`). Additive: unknown states
// fall through to ACTIVE, never throw (X2).
const DESCRIPTORS = {
    loading: {key: 'loading', statusText: 'Loading model…', severity: null},
    recording: {key: 'recording', statusText: 'Listening', severity: null},
    transcribing: {key: 'transcribing', statusText: 'Transcribing', severity: null},
    finalizing: {key: 'finalizing', statusText: 'Finishing', severity: null},
    notice: {key: 'notice', statusText: 'No speech detected', severity: 'recoverable'},
    error: {key: 'error', statusText: 'Error', severity: 'critical'},
};

// Unknown/extra states degrade to a neutral "active" descriptor (FR-008, X2).
const ACTIVE = {key: 'active', statusText: 'Active', severity: null};

// idle → nothing shown (push-to-talk, FR-002, X3).
const HIDDEN = {key: 'idle', statusText: '', hidden: true, severity: null};

/**
 * Map an org.myna.Dictation State string to a semantic descriptor
 * `{key, statusText, severity, hidden}`.
 *
 * @param {string} state - the wire State (idle|loading|recording|
 *     transcribing|finalizing|notice|error, or an unknown additive value).
 * @param {string} [reason] - content-free reason for a `notice`/`error`
 *     state (E3); ignored for every other state so caller text can never leak
 *     into an unrelated status (X6). `notice`'s reason is shown as-is (it
 *     isn't an error, so no "Error —" prefix); `error`'s reason is appended
 *     after that prefix, matching the pre-2026-07-30 behavior.
 * @returns {{key: string, statusText: string, severity: (string|null), hidden: boolean}}
 */
export function stateToDescriptor(state, reason = '') {
    if (state === 'idle' || state === null || state === undefined)
        return {...HIDDEN};

    const base = DESCRIPTORS[state] ?? ACTIVE;
    let statusText = base.statusText;
    if (reason !== '') {
        if (base.key === 'error')
            statusText = `Error — ${reason}`;
        else if (base.key === 'notice')
            statusText = reason;
    }
    return {key: base.key, statusText, severity: base.severity, hidden: false};
}
