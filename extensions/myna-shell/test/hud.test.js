// hud.test.js — GJS contract test for the HUD pill's PURE logic (feature
// 004-gnome-shell-indicator, 2026-07-30 HUD redesign; contracts extension.md
// X19-X21). Exercises hud-logic.js only — no Shell, no Clutter/St. The
// actor's compositor behavior (focus-safety X11, timing X12, per-state
// treatments X13, bar rendering X14, dismiss click X22) is manual-acceptance
// only (quickstart.md §5/5a/5b), same harness-tier split RibbonView had.
//
//     gjs -m test/hud.test.js        (from extensions/myna-shell/)

import System from 'system';

import {
    computePosition,
    iconForSeverity,
    ribbonPhaseForStateKey,
    ribbonVisibleForSeverity,
    pillColorClass,
    PILL_COLOR_CLASSES,
    MARGIN,
} from '../hud-logic.js';

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

// --- X21: bottom-center positioning, matching GNOME's native OSD -----------

{
    const monitor = {x: 0, y: 0, width: 1920, height: 1080};
    const pos = computePosition(monitor, 400, 80);
    eq('X21 horizontally centered', pos.x, Math.round((1920 - 400) / 2));
    eq('X21 anchored to the bottom edge', pos.y, 1080 - 80 - MARGIN);
    check('X21 not top-anchored (was the goop/ribbon placement)', pos.y > 1080 / 2);
}

// A non-origin (multi-monitor) monitor offset is respected.
{
    const monitor = {x: 1920, y: 100, width: 1280, height: 800};
    const pos = computePosition(monitor, 300, 60);
    eq('X21 multi-monitor: x offset by monitor.x', pos.x, 1920 + Math.round((1280 - 300) / 2));
    eq('X21 multi-monitor: y offset by monitor.y', pos.y, 100 + 800 - 60 - MARGIN);
}

// --- X19: mic vs. mic-slash icon, contextual on severity --------------------

{
    eq('X19 critical → mic-slash', iconForSeverity('critical'), 'microphone-disabled-symbolic');
    eq('X19 recoverable → plain mic (mic itself is not at fault)',
        iconForSeverity('recoverable'), 'audio-input-microphone-symbolic');
    eq('X19 no severity (loading/recording/...) → plain mic',
        iconForSeverity(null), 'audio-input-microphone-symbolic');
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

// --- 2026-07-30, R17: which state keys force a ribbon phase change --------

{
    eq('transcribing forces the ribbon into morph',
        ribbonPhaseForStateKey('transcribing'), 'morph');
    eq('finalizing forces the ribbon into complete (FR-010d)',
        ribbonPhaseForStateKey('finalizing'), 'complete');
    for (const key of ['idle', 'loading', 'recording', 'notice', 'error', 'unknown-state']) {
        eq(`${key} does not force a phase (the ribbon manages unfold/flow itself)`,
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
