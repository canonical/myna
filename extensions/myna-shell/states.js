// states.js — PURE dictation-state → visual-intent mapping for the Myna goop
// (feature 004-gnome-shell-indicator; data-model E1 + "State → visual-intent
// mapping"; contract extension.md X1–X4, X6).
//
// Consumed by indicator.js (the actor layer); unit-tested by
// test/states.test.js without a Shell. The record is CSS-class + animation +
// a11y label only — tunables live in stylesheet.css, and nothing here ever
// carries transcript text (constitution V): inputs are the state string plus
// an optional content-free error reason.

// Visual intent for each known org.myna.Dictation State (additive wire enum —
// unknowns fall through to ACTIVE_INTENT, never throw; X2).
const INTENTS = {
    loading: {
        cssClass: 'myna-goop-loading',
        animation: 'breathe',
        a11yLabel: 'Dictation: loading model',
    },
    recording: {
        cssClass: 'myna-goop-recording',
        animation: 'ripple',
        a11yLabel: 'Dictation: listening',
    },
    transcribing: {
        cssClass: 'myna-goop-transcribing',
        animation: 'shimmer',
        a11yLabel: 'Dictation: transcribing',
    },
    finalizing: {
        cssClass: 'myna-goop-finalizing',
        animation: 'flash',
        a11yLabel: 'Dictation: finishing',
    },
    error: {
        cssClass: 'myna-goop-error',
        animation: 'shake',
        a11yLabel: 'Dictation: error',
    },
};

// Unknown/extra states degrade to a neutral "active" treatment (FR-008, X2).
const ACTIVE_INTENT = {
    cssClass: 'myna-goop-active',
    animation: 'pulse',
    a11yLabel: 'Dictation: active',
};

// idle → no actor at all (push-to-talk, FR-002, X3).
const HIDDEN_INTENT = {
    cssClass: null,
    animation: 'none',
    a11yLabel: null,
    hidden: true,
};

/**
 * Map an org.myna.Dictation State string to a visual-intent record
 * `{cssClass, animation, a11yLabel, hidden}`.
 *
 * @param {string} state - the wire State (idle|loading|recording|
 *     transcribing|finalizing|error, or an unknown additive value).
 * @param {string} [errorReason] - content-free reason shown only in the
 *     `error` a11y label (E3); ignored for every other state so caller text
 *     can never leak into a label (X6).
 * @returns {{cssClass: string|null, animation: string, a11yLabel: string|null, hidden: boolean}}
 */
export function stateToIntent(state, errorReason = '') {
    if (state === 'idle' || state === null || state === undefined)
        return {...HIDDEN_INTENT};

    const intent = INTENTS[state] ?? ACTIVE_INTENT;
    let a11yLabel = intent.a11yLabel;
    if (state === 'error' && errorReason !== '')
        a11yLabel = `Dictation: error — ${errorReason}`;
    return {...intent, a11yLabel, hidden: false};
}
