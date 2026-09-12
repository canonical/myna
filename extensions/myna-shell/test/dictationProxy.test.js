// dictationProxy.test.js — the shared daemon proxy's readiness contract.
//
// The bug this guards: the proxy is built asynchronously, so an extension
// enabled *after* startup (unlocking the screen re-enables them all) reaches
// its consumers in the same main-loop iteration that started it, with
// `proxy` still null. The host used to dereference it there and die with
// "can't access property connectObject, this._proxy.proxy is null",
// leaving the daemon on the notification fallback for the rest of the
// session. Consumers must attach through whenReady() instead.
//
//     gjs -m test/dictationProxy.test.js     (from extensions/myna-shell/)

import GLib from 'gi://GLib';
import System from 'system';

import {DictationProxy} from '../dictationProxy.js';

let failures = 0;
function check(name, cond) {
    if (cond)
        print(`ok   ${name}`);
    else {
        failures++;
        print(`FAIL ${name}`);
    }
}

// --- Nothing is live in the iteration that starts it ---------------------

{
    const proxy = new DictationProxy();
    proxy.start();
    check('the proxy is not live in the iteration that started it',
        proxy.proxy === null);
    check('an unresolved proxy reads as "daemon absent"', !proxy.present);

    let fired = false;
    proxy.whenReady(() => {
        fired = true;
    });
    check('whenReady defers while creation is in flight', !fired);

    // A teardown inside that window drops the waiter rather than running it
    // against a proxy that is being thrown away.
    proxy.stop();
    let ran = false;
    const loop = GLib.MainLoop.new(null, false);
    GLib.timeout_add(GLib.PRIORITY_DEFAULT, 500, () => {
        ran = true;
        loop.quit();
        return GLib.SOURCE_REMOVE;
    });
    loop.run();
    check('the mainloop ran (the waiter had its chance)', ran);
    check('stop() drops a pending waiter', !fired);
}

// --- It does resolve, and a waiter attached before then still fires ------

if (GLib.getenv('DBUS_SESSION_BUS_ADDRESS')) {
    const proxy = new DictationProxy();
    proxy.start();

    let fired = false;
    proxy.whenReady(() => {
        fired = true;
    });

    const loop = GLib.MainLoop.new(null, false);
    GLib.timeout_add(GLib.PRIORITY_DEFAULT, 2000, () => {
        loop.quit();
        return GLib.SOURCE_REMOVE;
    });
    proxy.whenReady(() => loop.quit());
    loop.run();

    check('a waiter fires once creation resolves', fired);
    check('the proxy is live once resolved', proxy.proxy !== null);

    // Late waiters run immediately — the host's enable() after a
    // re-enable must not wait for a second resolution.
    let immediate = false;
    proxy.whenReady(() => {
        immediate = true;
    });
    check('whenReady on a live proxy fires synchronously', immediate);

    // No stop() here: it calls disconnectObject(), which GNOME Shell adds to
    // GObject and plain gjs does not have. Teardown of a *resolved* proxy is
    // Shell-only ground, covered on hardware.
} else {
    print('skip resolution tests (no session bus)');
}

print(failures === 0 ? 'PASS dictationProxy.test.js' : `FAIL dictationProxy.test.js: ${failures} failure(s)`);
System.exit(failures === 0 ? 0 : 1);
