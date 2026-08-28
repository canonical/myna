// presence.test.js — GJS contract test for the com.canonical.Myna.Shell presence name
// lifecycle (feature 004-gnome-shell-indicator, 2026-08-26 architecture
// revision; contract dbus-interface.md C12, extension.md XH5). The bus is a
// stub: no session bus needed.
//
//     gjs -m test/presence.test.js        (from extensions/myna-shell/)

import System from 'system';

import {PRESENCE_NAME, ShellPresence} from '../presence.js';

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

function stubBus({failOwn = false} = {}) {
    const calls = {owned: [], unowned: [], warnings: []};
    let nextId = 1;
    const presence = new ShellPresence({
        ownName: (name, onAcquired) => {
            if (failOwn)
                throw new Error('no bus');
            calls.owned.push(name);
            const id = nextId++;
            onAcquired();
            return id;
        },
        unownName: id => calls.unowned.push(id),
        log: message => calls.warnings.push(message),
    });
    return {presence, calls};
}

// --- XH5/C12: owned while enabled, released on disable -------------------

{
    const {presence, calls} = stubBus();
    check('dormant before enable()', presence.owned === false);

    presence.enable();
    eq('XH5 owns exactly the contract name', calls.owned.join(','), PRESENCE_NAME);
    check('XH5 reports itself owned while enabled', presence.owned === true);

    presence.enable();
    eq('XH5 enable() is idempotent (no second request)', calls.owned.length, 1);

    presence.disable();
    eq('XH5 releases the name on disable', calls.unowned.length, 1);
    check('XH5 reports itself unowned after disable', presence.owned === false);

    presence.disable();
    eq('XH5 disable() is safe when dormant', calls.unowned.length, 1);
}

// --- XH5: re-enable re-acquires (Shell restart / relogin) ---------------

{
    const {presence, calls} = stubBus();
    presence.enable();
    presence.disable();
    presence.enable();
    eq('XH5 re-enable re-acquires the name', calls.owned.length, 2);
    check('XH5 ... and is owned again', presence.owned === true);
    presence.disable();
}

// --- XH5: presence is advisory — a bus failure never breaks hosting -----

{
    const {presence, calls} = stubBus({failOwn: true});
    let threw = false;
    try {
        presence.enable();
    } catch (e) {
        threw = true;
    }
    check('XH5 a bus failure does not throw out of enable()', !threw);
    check('XH5 ... leaves the name unowned', presence.owned === false);
    check('XH5 ... and logs once', calls.warnings.length === 1);
    presence.disable(); // must not throw either
}

// --- XH5: no bus at all (no ownName seam) ------------------------------

{
    const presence = new ShellPresence({log: () => {}});
    let threw = false;
    try {
        presence.enable();
        presence.disable();
    } catch (e) {
        threw = true;
    }
    check('XH5 a missing bus degrades quietly', !threw && presence.owned === false);
}

print(failures === 0 ? 'PASS presence.test.js' : `FAIL presence.test.js: ${failures} failure(s)`);
System.exit(failures === 0 ? 0 : 1);
