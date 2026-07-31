// hud-logic.js — PURE logic for the bottom-center HUD pill (feature
// 004-gnome-shell-indicator, 2026-07-30 HUD redesign; research R14/R15;
// contracts extension.md X19-X21). No Shell/gi imports — unit-tested headless
// by test/hud.test.js, the same split states.js/vumeter.js already establish
// between the *stable* pure layer and the *experimental* Shell-dependent
// actor (`hud.js`, harness-tier — see plan.md Constitution Check). hud.js
// owns all pixels; this file only decides positions, icon choice, and
// auto-dismiss behavior.

// Gap between the pill's bottom edge and the screen's bottom edge (R14).
export const MARGIN = 24;

/**
 * Bottom-center position for the pill on `monitor`, matching the placement of
 * GNOME's own volume/brightness OSD (R14, FR-004, X21) — not the prior
 * top-of-panel goop/ribbon placement.
 *
 * @param {{x: number, y: number, width: number, height: number}} monitor
 * @param {number} width - the pill's width.
 * @param {number} height - the pill's height.
 * @param {number} [margin] - gap from the monitor's bottom edge.
 * @returns {{x: number, y: number}}
 */
export function computePosition(monitor, width, height, margin = MARGIN) {
    return {
        x: monitor.x + Math.round((monitor.width - width) / 2),
        y: monitor.y + monitor.height - height - margin,
    };
}

/**
 * Icon choice for a descriptor's severity (X19): a mic-with-slash icon only
 * for a critical error (the microphone genuinely may be at fault); every
 * other treatment — including a recoverable notice, where the microphone
 * itself isn't the problem — keeps the plain filled mic.
 *
 * @param {(string|null)} severity - `'recoverable' | 'critical' | null`.
 * @returns {string} a symbolic icon name.
 */
export function iconForSeverity(severity) {
    return severity === 'critical'
        ? 'microphone-disabled-symbolic'
        : 'audio-input-microphone-symbolic';
}

/**
 * Whether a held notice of this severity auto-dismisses on its own (R15,
 * FR-007a/FR-007b): `recoverable` notices do (a bounded hold, restarting in
 * full on a repeat — X20); `critical` errors never do (persist until the
 * user's explicit dismiss — X22).
 *
 * @param {(string|null)} severity
 * @returns {boolean}
 */
export function severityAutoDismisses(severity) {
    return severity === 'recoverable';
}

/**
 * Whether an incoming descriptor should replace an already-held notice in
 * place rather than being ignored or queued (R15, FR-007a/FR-007d, X20): any
 * new problem descriptor (severity !== null) always replaces whatever is
 * currently held — there is exactly one held-notice slot, never a queue,
 * regardless of whether the severity matches the one already showing.
 *
 * @param {(string|null)} incomingSeverity
 * @returns {boolean}
 */
export function shouldReplaceHeldNotice(incomingSeverity) {
    return incomingSeverity !== null;
}

/**
 * The pill's colour-class name for this state/severity (feature 004
 * follow-up, post-manual-test-review): orange for a recoverable notice, red
 * for a critical error — so severity reads at a glance, not just from text —
 * and a warm "loading" tint for the cold-model-load phase so it's legible as
 * distinct from listening (FR-006) by more than its label alone. Every other
 * state (recording/transcribing/finalizing) gets no colour override — the
 * label alone is enough to distinguish those (FR-005).
 *
 * @param {{key: string, severity: (string|null)}} descriptor
 * @returns {(string|null)} an St CSS class name, or null for no override.
 */
export function pillColorClass({key, severity}) {
    if (severity === 'recoverable')
        return 'myna-hud-severity-recoverable';
    if (severity === 'critical')
        return 'myna-hud-severity-critical';
    if (key === 'loading')
        return 'myna-hud-phase-loading';
    return null;
}

// Every colour class pillColorClass can return, for a view to reset before
// applying the current one (avoids stale classes lingering across states).
export const PILL_COLOR_CLASSES = [
    'myna-hud-severity-recoverable',
    'myna-hud-severity-critical',
    'myna-hud-phase-loading',
];

/**
 * Which wave-ribbon lifecycle phase (ribbon.js) a state transition forces,
 * or `null` when the ribbon manages its own phase internally (2026-07-30
 * wave-ribbon redesign, R17). Only the two transitions that must visibly
 * change the ribbon's motion are forced here:
 *   - `transcribing` → `morph` (FR-010a: session ended, simplified
 *     processing motion).
 *   - `finalizing` → `complete` (FR-010d: the brief quiet-success
 *     indication before the pill clears).
 * Every other key (including `recording`/`loading`) returns `null` so the
 * actor's own unfold→flow auto-transition (fired once, shortly after the
 * actor is created for a new session) is never interrupted by a redundant
 * external phase-set on the very first descriptor a fresh session receives.
 *
 * @param {string} key - the state's `key` (states.js's descriptor field).
 * @returns {('morph'|'complete'|null)}
 */
export function ribbonPhaseForStateKey(key) {
    if (key === 'transcribing')
        return 'morph';
    if (key === 'finalizing')
        return 'complete';
    return null;
}

/**
 * Whether the wave ribbon stays visible for this severity (2026-07-30
 * design refinement — "fabric in gentle airflow" pass). Only a **critical**
 * error fully hides/collapses the ribbon (the pill's icon/border/message
 * carry that state instead); a **recoverable** notice keeps the ribbon
 * visible, tinted amber and gently pulsing rather than hidden — motion
 * "pauses" but never reads as dead. `descriptor.severity` is passed
 * straight through as `ribbon.js`'s `severityTint` input (the values
 * already match: `null | 'recoverable' | 'critical'`).
 *
 * @param {(string|null)} severity
 * @returns {boolean}
 */
export function ribbonVisibleForSeverity(severity) {
    return severity !== 'critical';
}
