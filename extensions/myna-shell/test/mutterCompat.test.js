// mutterCompat.test.js — headless contract test for the Mutter 14–16 versus
// Mutter 17+ trusted-client APIs that host.js adapts for GNOME Shell 46–51.
//
//     gjs -m test/mutterCompat.test.js   (from extensions/myna-shell/)

import System from 'system';

import {configureTrustedWindow, launchTrustedClient} from '../mutterCompat.js';

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

// Mutter 14 (GNOME Shell 46): construct first, then launch through the
// client so Mutter gives the child a trusted private Wayland socket.
{
    const calls = [];
    const subprocess = {name: 'old-subprocess'};
    const client = {
        spawnv(display, argv) {
            calls.push(['spawnv', display, argv]);
            return subprocess;
        },
    };
    const WaylandClient = {
        new(context, launcher) {
            calls.push(['new', context, launcher]);
            return client;
        },
    };
    const result = launchTrustedClient({
        WaylandClient, context: 'context', display: 'display', launcher: 'launcher', argv: ['hud', '--lab'],
    });
    eq('Mutter 14: returns the constructed client', result.client, client);
    eq('Mutter 14: returns spawnv subprocess', result.subprocess, subprocess);
    eq('Mutter 14: constructs before spawning', JSON.stringify(calls),
        JSON.stringify([['new', 'context', 'launcher'], ['spawnv', 'display', ['hud', '--lab']]]));
}

// Mutter 17+ (GNOME Shell 49+): one constructor launches the process; the
// subprocess is retrieved afterwards from the returned client.
{
    const calls = [];
    const subprocess = {name: 'new-subprocess'};
    const client = {
        get_subprocess() {
            calls.push(['get_subprocess']);
            return subprocess;
        },
    };
    const WaylandClient = {
        new_subprocess(context, launcher, argv) {
            calls.push(['new_subprocess', context, launcher, argv]);
            return client;
        },
        new() {
            throw new Error('old constructor must not run');
        },
    };
    const result = launchTrustedClient({
        WaylandClient, context: 'context', display: 'display', launcher: 'launcher', argv: ['hud'],
    });
    eq('Mutter 17: returns the new client', result.client, client);
    eq('Mutter 17: returns its subprocess', result.subprocess, subprocess);
    eq('Mutter 17: new_subprocess replaces new + spawnv', JSON.stringify(calls),
        JSON.stringify([['new_subprocess', 'context', 'launcher', ['hud']], ['get_subprocess']]));
}

// Mutter 14–16 window configuration lives on Meta.WaylandClient.
{
    const calls = [];
    const window = {};
    const client = {
        make_dock(w) { calls.push(['make_dock', w]); },
        hide_from_window_list(w) { calls.push(['hide_from_window_list', w]); },
    };
    configureTrustedWindow({client, window, dockType: 'dock'});
    eq('Mutter 14: client makes the window dock', calls[0][0], 'make_dock');
    eq('Mutter 14: client receives the window for dock typing', calls[0][1], window);
    eq('Mutter 14: client hides the window from the list', calls[1][0], 'hide_from_window_list');
    eq('Mutter 14: client receives the window for hiding', calls[1][1], window);
}

// Mutter 17+ moves the same operations to Meta.Window.
{
    const calls = [];
    const window = {
        set_type(type) { calls.push(['set_type', type]); },
        hide_from_window_list() { calls.push(['hide_from_window_list']); },
    };
    const client = {
        make_dock() { throw new Error('old client API must not run'); },
        hide_from_window_list() { throw new Error('old client API must not run'); },
    };
    configureTrustedWindow({client, window, dockType: 'dock'});
    eq('Mutter 17: window sets its dock type', JSON.stringify(calls[0]),
        JSON.stringify(['set_type', 'dock']));
    eq('Mutter 17: window hides itself from the list', calls[1][0], 'hide_from_window_list');
}

print(failures === 0
    ? 'PASS mutterCompat.test.js'
    : `FAIL mutterCompat.test.js (${failures})`);
System.exit(failures === 0 ? 0 : 1);
