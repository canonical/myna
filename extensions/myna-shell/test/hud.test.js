// hud.test.js — GJS contract test for the HUD pill's PURE logic (feature
// 004-gnome-shell-indicator, 2026-07-30 HUD redesign; contracts extension.md
// X19-X21). Exercises hudLogic.js only — no Shell, no Clutter/St. The
// actor's compositor behavior (focus-safety X11, timing X12, per-state
// treatments X13, bar rendering X14, dismiss click X22) is manual-acceptance
// only (quickstart.md §5/5a/5b), same harness-tier split RibbonView had.
//
// X21's bottom-centre placement is no longer arithmetic to test: hud.js
// declares it with a `Layout.MonitorConstraint` + `y_align: END` +
// stylesheet.css's `margin-bottom` (2026-08-20), so the former
// `computePosition` unit tests were removed along with the function.
//
//     gjs -m test/hud.test.js        (from extensions/myna-shell/)

import System from 'system';

import {
    iconForSeverity,
    ribbonPhaseForStateKey,
    ribbonVisibleForSeverity,
    severityAutoDismisses,
    shouldReplaceHeldNotice,
    pillColorClass,
    PILL_COLOR_CLASSES,
} from '../hudLogic.js';

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

// --- X19: mic vs. mic-slash icon, contextual on severity --------------------

{
    eq('X19 critical → mic-slash', iconForSeverity('critical'), 'microphone-disabled-symbolic');
    eq('X19 recoverable → plain mic (mic itself is not at fault)',
        iconForSeverity('recoverable'), 'audio-input-microphone-symbolic');
    eq('X19 no severity (loading/recording/...) → plain mic',
        iconForSeverity(null), 'audio-input-microphone-symbolic');
}

// --- FR-007a/FR-007b: auto-dismiss behavior by severity ---------------------

{
    check('recoverable notices auto-dismiss', severityAutoDismisses('recoverable') === true);
    check('critical errors do not auto-dismiss', severityAutoDismisses('critical') === false);
    check('non-problem states have no auto-dismiss concept', severityAutoDismisses(null) === false);
}

// --- X20: replace-in-place — any new problem descriptor replaces the held --
// --- slot; there is never a queue, regardless of matching severity.       --

{
    check('a recoverable notice replaces a held slot',
        shouldReplaceHeldNotice('recoverable') === true);
    check('a critical error replaces a held slot',
        shouldReplaceHeldNotice('critical') === true);
    check('a non-problem state does not "replace" (nothing to hold)',
        shouldReplaceHeldNotice(null) === false);
}

// --- Manual-test follow-up: severity/phase colour classes ------------------

{
    eq('recoverable → orange colour class',
        pillColorClass({key: 'notice', severity: 'recoverable'}),
        'myna-hud-severity-recoverable');
    eq('critical → red colour class',
        pillColorClass({key: 'error', severity: 'critical'}),
        'myna-hud-severity-critical');
    eq('loading → warm phase colour class',
        pillColorClass({key: 'loading', severity: null}),
        'myna-hud-phase-loading');
    for (const key of ['recording', 'transcribing', 'finalizing']) {
        eq(`${key} → no colour override`,
            pillColorClass({key, severity: null}), null);
    }
    check('every possible colour class is listed in PILL_COLOR_CLASSES',
        PILL_COLOR_CLASSES.includes('myna-hud-severity-recoverable') &&
        PILL_COLOR_CLASSES.includes('myna-hud-severity-critical') &&
        PILL_COLOR_CLASSES.includes('myna-hud-phase-loading'));
}

// --- 2026-07-30, R17 / 2026-08-21 fix: which state keys force a phase ------

{
    eq('transcribing forces the ribbon into morph',
        ribbonPhaseForStateKey('transcribing'), 'morph');
    eq('finalizing forces the ribbon into complete (FR-010d)',
        ribbonPhaseForStateKey('finalizing'), 'complete');
    // Live states pin the ribbon to flow — this is what recovers it after a
    // morph/complete, which was previously stuck until idle/a new session.
    for (const key of ['loading', 'recording', 'active']) {
        eq(`${key} forces the ribbon into flow`,
            ribbonPhaseForStateKey(key), 'flow');
    }
    // idle never shows; notice/error are carried by tint/visibility, not phase.
    for (const key of ['idle', 'notice', 'error', 'unknown-state']) {
        eq(`${key} does not force a phase`,
            ribbonPhaseForStateKey(key), null);
    }
}

// --- 2026-07-30, R17a: ribbon visibility by severity (only critical hides) -

{
    check('the ribbon stays visible for a recoverable notice (amber/paused instead of hidden)',
        ribbonVisibleForSeverity('recoverable') === true);
    check('the ribbon hides for a critical error',
        ribbonVisibleForSeverity('critical') === false);
    check('the ribbon stays visible for non-problem states',
        ribbonVisibleForSeverity(null) === true);
}

print(failures === 0 ? 'PASS hud.test.js' : `FAIL hud.test.js: ${failures} failure(s)`);
System.exit(failures === 0 ? 0 : 1);
