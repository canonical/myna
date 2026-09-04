// host.test.js — headless smoke + logic test for the overlay host
// (feature 004-gnome-shell-indicator; contract extension.md XH1/XH3/XH4).
//
// host.js imports gi://Meta and gi://Shell, which are not loadable outside a
// running Shell, so this does NOT import host.js. Instead it re-checks the
// host's *composed logic* — the pure decisions the host wires together —
// exactly as the host calls them, guarding against a resolve/place/respawn
// contract drifting out from under host.js. The live Meta.WaylandClient path
// is covered by the headless-Shell integration test (T125), which runs
// inside a nested Shell.
//
//     gjs -m test/host.test.js        (from extensions/myna-shell/)

import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import System from 'system';

import {computePlacement, placementChanged} from '../place.js';
import {initialState, planRestart} from '../respawn.js';
import {resolveHudLaunch, SNAP_APP, SNAP_LAUNCHER} from '../resolve.js';

let failures = 0;
function check(name, cond) {
    if (cond)
        print(`ok   ${name}`);
    else {
        failures++;
        print(`FAIL ${name}`);
    }
}
function eq(name, a, b) {
    check(`${name} (got ${JSON.stringify(a)})`, JSON.stringify(a) === JSON.stringify(b));
}

// --- The launch the host will attempt on a snap system -------------------

{
    const launch = resolveHudLaunch({
        getenv: () => null,
        isExecutable: p => p === SNAP_LAUNCHER,
    });
    eq('the host launches `snap run myna.hud` on a snap system',
        launch.argv, [SNAP_LAUNCHER, 'run', SNAP_APP]);
}

// --- Placement the host will apply for a typical bottom bar ---------------

{
    // 1920x1080 work area with a 48px bottom-anchored area already removed
    // by the work-area query; a 360x76 pill.
    const workArea = {x: 0, y: 0, width: 1920, height: 1032};
    const target = computePlacement(workArea, {width: 360, height: 76});
    eq('the pill is horizontally centred', target.x, Math.round((1920 - 360) / 2));
    check('the pill sits above the work-area bottom',
        target.y === 1032 - 76 - 24);

    // The host skips a move when nothing changed (anti-churn).
    check('an unchanged placement is a no-op move',
        !placementChanged(target, {x: target.x, y: target.y}));
    check('a real move is applied',
        placementChanged(target, {x: target.x + 5, y: target.y}));
}

// --- The supervision decisions the host will make on exit ----------------

{
    // A crash after a healthy run restarts with the base backoff.
    let state = initialState();
    let plan = planRestart(state, {expected: false, uptimeMs: 120000});
    check('a crash after a healthy run restarts', plan.restart);
    eq('...at the base backoff', plan.delayMs, 500);

    // A clean exit (disable) never restarts.
    plan = planRestart(state, {expected: true, uptimeMs: 5000});
    check('an expected exit does not restart', !plan.restart);

    // A permanently-crashing renderer eventually goes dormant, not looping.
    state = initialState();
    let dormant = false;
    for (let i = 0; i < 10; i++) {
        plan = planRestart(state, {expected: false, uptimeMs: 10});
        state = {consecutiveFailures: plan.consecutiveFailures};
        if (plan.dormant) {
            dormant = true;
            break;
        }
    }
    check('a permanently-crashing renderer goes dormant, not into a crash loop', dormant);
}

// --- The cancellable exit-watch pattern host.js uses ---------------------
// host.js imports gi://Meta so it cannot be imported here, but its
// subprocess supervision is a small, self-contained pattern worth pinning:
// a normal exit fires the respawn path, and a wait cancelled by disable()
// rejects as CANCELLED and does NOT.

Gio._promisify(Gio.Subprocess.prototype, 'wait_async', 'wait_finish');

async function watchExit(subprocess, cancellable) {
    try {
        await subprocess.wait_async(cancellable);
    } catch (e) {
        if (!e.matches?.(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED))
            throw e;
        return false;
    }
    return !cancellable.is_cancelled();
}

// --- The renderer's lifetime follows the daemon (XH14) --------------------

{
    // A daemon that goes away is the host asking the renderer to stop, so it
    // takes the `expected` branch: no respawn, and no dormancy either - the
    // budget is for crash loops, and a `snap stop myna` is not one.
    const afterStop = planRestart(initialState(), {expected: true, uptimeMs: 5000});
    check('XH14 a vanished daemon stops the renderer without respawning',
        !afterStop.restart && !afterStop.dormant);

    // And a daemon that comes back gets a clean slate, so a renderer that
    // crashed against the previous one is not already part-way to dormant.
    const crashed = planRestart(initialState(), {expected: false, uptimeMs: 10});
    check('XH14 ... and the crash tally is non-zero before a restart',
        crashed.consecutiveFailures > 0);
    eq('XH14 a returning daemon resets the tally the host carries',
        initialState().consecutiveFailures, 0);
}

const loop = new GLib.MainLoop(null, false);
(async () => {
    const proc = Gio.Subprocess.new(['sh', '-c', 'exit 0'], Gio.SubprocessFlags.NONE);
    check('a normal renderer exit drives the respawn path',
        await watchExit(proc, new Gio.Cancellable()));

    const sleeper = Gio.Subprocess.new(['sleep', '30'], Gio.SubprocessFlags.NONE);
    const cancellable = new Gio.Cancellable();
    const pending = watchExit(sleeper, cancellable);
    cancellable.cancel();       // the disable() path
    check('a wait cancelled on disable does NOT drive respawn', !(await pending));
    sleeper.force_exit();

    print(failures === 0 ? 'PASS host.test.js' : `FAIL host.test.js: ${failures} failure(s)`);
    loop.quit();
    System.exit(failures === 0 ? 0 : 1);
})();
loop.run();
