// respawn.test.js — GJS contract test for the renderer supervision policy
// (feature 004-gnome-shell-indicator, 2026-08-26 architecture revision;
// contract extension.md XH3, FR-026, SC-016). Pure: the caller owns the
// clock and the spawning; this is only the decision.
//
//     gjs -m test/respawn.test.js        (from extensions/myna-shell/)

import System from 'system';

import {
    BASE_BACKOFF_MS,
    HEALTHY_UPTIME_MS,
    MAX_BACKOFF_MS,
    RESTART_BUDGET,
    initialState,
    planRestart,
} from '../respawn.js';

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

// --- FR-026: an unexpected exit restarts, after a bounded wait ------------

{
    const first = planRestart(initialState(), {expected: false, uptimeMs: 200});
    check('XH3 an unexpected exit restarts', first.restart === true);
    eq('XH3 the first retry waits the base backoff', first.delayMs, BASE_BACKOFF_MS);
    check('XH3 never dormant on the first failure', first.dormant === false);
}

// --- XH3: backoff grows, but stays bounded -------------------------------

{
    let state = initialState();
    const delays = [];
    for (let i = 0; i < RESTART_BUDGET; i++) {
        const plan = planRestart(state, {expected: false, uptimeMs: 100});
        delays.push(plan.delayMs);
        state = {consecutiveFailures: plan.consecutiveFailures};
    }
    check('XH3 backoff is non-decreasing',
        delays.every((d, i) => i === 0 || d >= delays[i - 1]));
    check('XH3 backoff grows (it is not a flat retry loop)',
        delays[delays.length - 1] > delays[0]);
    check('XH3 backoff never exceeds the cap',
        delays.every(d => d <= MAX_BACKOFF_MS));

    // One more failure past the budget: stop trying, go dormant.
    const exhausted = planRestart(state, {expected: false, uptimeMs: 100});
    check('XH3 a permanently-crashing binary stops being retried',
        exhausted.restart === false);
    check('XH3 ... and is reported as dormant (log once, degrade quietly)',
        exhausted.dormant === true);
}

// --- XH3: an expected exit is never restarted ----------------------------

{
    const stopped = planRestart({consecutiveFailures: 3}, {expected: true, uptimeMs: 5000});
    check('XH3 disable()/shutdown never respawns', stopped.restart === false);
    check('XH3 ... and is not dormancy either', stopped.dormant === false);
    eq('XH3 an expected exit clears the tally', stopped.consecutiveFailures, 0);
}

// --- XH3: a healthy run resets the budget --------------------------------

{
    const afterLongRun = planRestart(
        {consecutiveFailures: RESTART_BUDGET},
        {expected: false, uptimeMs: HEALTHY_UPTIME_MS + 1});
    check('XH3 a crash after a healthy run is a fresh incident, not dormancy',
        afterLongRun.restart === true);
    eq('XH3 ... and its tally restarts at one', afterLongRun.consecutiveFailures, 1);
    eq('XH3 ... with the base backoff again', afterLongRun.delayMs, BASE_BACKOFF_MS);
}

print(failures === 0 ? 'PASS respawn.test.js' : `FAIL respawn.test.js: ${failures} failure(s)`);
System.exit(failures === 0 ? 0 : 1);
