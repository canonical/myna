// a11y.test.js — GJS contract test for the pure announcement-formatting
// module (feature 011-accessible-dictation-ux, contracts/announcer.md G1/G2).
//
//     gjs -m test/a11y.test.js        (from extensions/myna-shell/)
//
// exits 0 when every guarantee holds, 1 otherwise. No Shell / no D-Bus needed.

import System from 'system';

import {Announcer, formatAnnouncement} from '../a11y.js';
import {loadMatrixFromPath} from '../coverage.js';

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

// --- G1: every coverage-matrix state produces a non-empty announcement ----
const matrix = loadMatrixFromPath('coverage-matrix.json');
for (const entry of matrix.states) {
    const {text} = formatAnnouncement(entry.id, null);
    check(`G1 ${entry.id} has a non-empty announcement text`, !!text && text.length > 0);
}

// --- wording matches the Rust side for the states both cover (FR-024a) ----
eq('recording → Listening', formatAnnouncement('recording', null).text, 'Listening');
eq('transcribing → Transcribing', formatAnnouncement('transcribing', null).text, 'Transcribing');
eq('finalizing → Finishing', formatAnnouncement('finalizing', null).text, 'Finishing');
eq('notice → Notice', formatAnnouncement('notice', 'recoverable').text, 'Notice');
eq('error → Error', formatAnnouncement('error', 'critical').text, 'Error');
eq('idle → Idle', formatAnnouncement('idle', null).text, 'Idle');

// --- politeness: critical is assertive, everything else is polite ---------
eq('critical severity is assertive', formatAnnouncement('error', 'critical').politeness, 'assertive');
eq('recoverable severity is polite', formatAnnouncement('notice', 'recoverable').politeness, 'polite');
eq('no severity is polite', formatAnnouncement('recording', null).politeness, 'polite');

// --- unknown state ids degrade to a neutral, non-empty announcement -------
check('unknown state id does not throw', (() => {
    try {
        formatAnnouncement('quantizing', null);
        return true;
    } catch {
        return false;
    }
})());
eq('unknown state id text', formatAnnouncement('quantizing', null).text, 'Active');

// --- G2: Announcer coalesces a burst, only the last call is delivered -----
// A fake scheduler: records the (ms, callback) pair without a real timer,
// and lets the test invoke/cancel it manually — deterministic, no GLib main
// loop needed (mirrors the Rust side's paused/advanced virtual time).
function fakeScheduler() {
    const state = {scheduled: null, cancelledIds: [], nextId: 1};
    return {
        state,
        _scheduleFlush: (ms, callback) => {
            const id = state.nextId++;
            state.scheduled = {id, ms, callback};
            return id;
        },
        _cancelScheduled: id => {
            state.cancelledIds.push(id);
            if (state.scheduled && state.scheduled.id === id)
                state.scheduled = null;
        },
    };
}

{
    const sched = fakeScheduler();
    const emitted = [];
    const announcer = new Announcer({
        _getBusAddress: () => 'unused',
        _openConnection: () => ({emit_signal: () => {}}),
        _scheduleFlush: sched._scheduleFlush,
        _cancelScheduled: sched._cancelScheduled,
    });
    announcer.enable();
    // Patch _emit after construction so the fixture can observe calls
    // without depending on the real GVariant-building internals.
    announcer._emit = (text, politeness) => emitted.push({text, politeness});

    announcer.announce('Loading model…', 'polite');
    announcer.announce('Listening', 'polite');
    announcer.announce('Transcribing', 'polite');
    // Simulate "the window elapsed" by invoking the last scheduled callback.
    sched.state.scheduled.callback();

    eq('G2 a burst delivers only the last announcement', emitted.length, 1);
    check('G2 the delivered text is the final one in the burst',
        emitted[0]?.text === 'Transcribing');
    check('G2 earlier scheduled callbacks were cancelled, not left to fire',
        sched.state.cancelledIds.length === 2);
}

{
    const sched = fakeScheduler();
    const emitted = [];
    const announcer = new Announcer({
        _getBusAddress: () => 'unused',
        _openConnection: () => ({emit_signal: () => {}}),
        _scheduleFlush: sched._scheduleFlush,
        _cancelScheduled: sched._cancelScheduled,
    });
    announcer.enable();
    announcer._emit = (text, politeness) => emitted.push({text, politeness});

    announcer.announce('Listening', 'polite');
    sched.state.scheduled.callback(); // window elapses before the next call
    announcer.announce('Finishing', 'polite');
    sched.state.scheduled.callback();

    eq('G2 announcements spaced beyond the window are both delivered',
        emitted.length, 2);
}

// --- G3: disable() releases the connection and drops any pending
//         announcement rather than delivering it late ---------------------
{
    const sched = fakeScheduler();
    const emitted = [];
    let closed = false;
    const announcer = new Announcer({
        _getBusAddress: () => 'unused',
        _openConnection: () => ({
            emit_signal: () => {},
            close_sync: () => {
                closed = true;
            },
        }),
        _scheduleFlush: sched._scheduleFlush,
        _cancelScheduled: sched._cancelScheduled,
    });
    announcer.enable();
    announcer._emit = (text, politeness) => emitted.push({text, politeness});

    announcer.announce('Listening', 'polite');
    announcer.disable();

    check('G3 disable() closes the bus connection', closed);
    check('G3 disable() is idempotent', (() => {
        try {
            announcer.disable();
            return true;
        } catch {
            return false;
        }
    })());
    eq('G3 a pending announcement is dropped, never delivered late',
        emitted.length, 0);
}

// --- FR-002a/FR-007: a bus-connection failure never throws; announce()
//     silently no-ops afterward rather than crashing the extension --------
{
    const sched = fakeScheduler();
    const announcer = new Announcer({
        _getBusAddress: () => {
            throw new Error('org.a11y.Bus unreachable (simulated)');
        },
        _scheduleFlush: sched._scheduleFlush,
        _cancelScheduled: sched._cancelScheduled,
    });

    check('enable() does not throw when the bus is unreachable', (() => {
        try {
            announcer.enable();
            return true;
        } catch {
            return false;
        }
    })());
    check('announce() after a failed enable() does not throw', (() => {
        try {
            announcer.announce('Listening', 'polite');
            sched.state.scheduled.callback();
            return true;
        } catch {
            return false;
        }
    })());
}

System.exit(failures > 0 ? 1 : 0);
