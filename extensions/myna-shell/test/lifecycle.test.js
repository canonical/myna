// lifecycle.test.js — GJS contract test for the org.myna.Dictation proxy
// lifecycle (feature 004-gnome-shell-indicator, contracts extension.md X7–X10),
// driven against a STUB proxy + name watch — no session bus, no Shell.
//
//     gjs -m test/lifecycle.test.js        (from extensions/myna-shell/)
//
// exits 0 when every guarantee holds, 1 otherwise.

import System from 'system';

import {DictationService} from '../dbus.js';
import {stateToIntent} from '../states.js';

let failures = 0;

function check(name, condition) {
    if (condition) {
        print(`ok   ${name}`);
    } else {
        failures++;
        print(`FAIL ${name}`);
    }
}

function eq(name, actual, expected) {
    check(`${name} (got ${JSON.stringify(actual)})`, actual === expected);
}

// A GLib.Variant stand-in: the only surface dbus.js uses is deep_unpack().
function variant(value) {
    return {deep_unpack: () => value};
}

// Stub D-Bus world: an injectable name watch + a stub Dictation proxy that
// records connect/disconnect and can be driven through StateChanged signals.
function makeStubBus(initialState = 'idle') {
    const calls = {watched: 0, unwatched: 0, proxyCreated: 0, disconnected: 0};
    let appearedCb = null;
    let vanishedCb = null;

    const proxy = {
        state: initialState,
        errorMessage: '',
        _handlers: {},
        connect(signal, cb) {
            this._handlers[signal] = cb;
            return 1;
        },
        disconnect(_id) {
            calls.disconnected++;
        },
        get_cached_property(name) {
            return variant(name === 'State' ? this.state : this.errorMessage);
        },
        // Test driver: emit StateChanged as the real publisher would.
        emitStateChanged(state, errorMessage = '') {
            this.state = state;
            this.errorMessage = errorMessage;
            this._handlers['g-signal']?.(
                this, null, 'StateChanged', variant([state, errorMessage]));
        },
    };

    return {
        calls,
        proxy,
        watchName: (appeared, vanished) => {
            calls.watched++;
            appearedCb = appeared;
            vanishedCb = vanished;
            return 42;
        },
        unwatchName: _id => {
            calls.unwatched++;
        },
        createProxy: () => {
            calls.proxyCreated++;
            return proxy;
        },
        appear: () => appearedCb?.(null, 'org.myna.Dictation', ':1.42'),
        vanish: () => vanishedCb?.(null, 'org.myna.Dictation'),
    };
}

// Wire a service exactly like extension.js does: state → intent → show/hide
// on a spy indicator standing in for the goop actor.
function makeWiredService(initialState = 'idle') {
    const stub = makeStubBus(initialState);
    const shown = [];
    let hides = 0;
    const spyIndicator = {
        show: intent => shown.push(intent),
        hide: () => hides++,
    };
    const service = new DictationService({
        onStateChanged: (state, errorMessage) => {
            const intent = stateToIntent(state, errorMessage);
            if (intent.hidden)
                spyIndicator.hide();
            else
                spyIndicator.show(intent);
        },
        _watchName: stub.watchName,
        _unwatchName: stub.unwatchName,
        _createProxy: stub.createProxy,
    });
    return {stub, service, shown, hides: () => hides};
}

// --- X7: enable() with the name absent stays dormant (no actor, no error) --

{
    const {stub, service, shown} = makeWiredService();
    service.enable();
    eq('X7 watches the name on enable', stub.calls.watched, 1);
    eq('X7 no proxy while absent', stub.calls.proxyCreated, 0);
    eq('X7 no actor while absent', shown.length, 0);
    check('X7 service reports unavailable', !service.available);
    service.disable();
}

// --- X8: name-appeared connects + reflects current State; vanished → idle --

{
    const {stub, service, shown, hides} = makeWiredService('recording');
    service.enable();
    stub.appear();
    eq('X8 proxy created on appeared', stub.calls.proxyCreated, 1);
    check('X8 service reports available', service.available);
    eq('X8 reflects current State', shown.at(-1)?.cssClass, 'myna-goop-recording');
    eq('X8 service state mirrors', service.state, 'recording');

    // Live transitions flow through the signal.
    stub.proxy.emitStateChanged('transcribing');
    eq('X8 signal drives the actor', shown.at(-1)?.cssClass,
        'myna-goop-transcribing');
    stub.proxy.emitStateChanged('error', 'no text field is focused');
    eq('X8 error reason reaches the label', shown.at(-1)?.a11yLabel,
        'Dictation: error — no text field is focused');

    stub.vanish();
    check('X8 vanished → unavailable', !service.available);
    eq('X8 vanished clears to idle', service.state, 'idle');
    eq('X8 goop cleared on vanish', hides(), 1);
    service.disable();
}

// --- X9: disable() tears down watch, proxy, subscriptions — no leaks -------

{
    const {stub, service} = makeWiredService();
    service.enable();
    stub.appear();
    service.disable();
    eq('X9 name unwatched on disable', stub.calls.unwatched, 1);
    eq('X9 proxy disconnected on disable', stub.calls.disconnected, 1);

    // Signals after disable are dead: the watch is gone, so nothing can fire.
    check('X9 service reports unavailable after disable', !service.available);
}

// disable() with the name never present is still clean (watch removed, no
// proxy to disconnect, no throw).
{
    const {stub, service} = makeWiredService();
    service.enable();
    let threw = false;
    try {
        service.disable();
    } catch (e) {
        threw = true;
        print(`     disable() threw: ${e}`);
    }
    check('X9 dormant disable does not throw', !threw);
    eq('X9 dormant disable unwatches', stub.calls.unwatched, 1);
    eq('X9 dormant disable has no proxy to drop', stub.calls.disconnected, 0);
}

// --- X10: re-enable after disable re-establishes cleanly --------------------

{
    const {stub, service, shown} = makeWiredService('transcribing');
    service.enable();
    stub.appear();
    service.disable();
    service.enable();
    stub.appear();
    eq('X10 re-watched on re-enable', stub.calls.watched, 2);
    eq('X10 fresh proxy on re-appear', stub.calls.proxyCreated, 2);
    eq('X10 reflects current State again', shown.at(-1)?.cssClass,
        'myna-goop-transcribing');
    service.disable();
}

print(failures === 0
    ? 'PASS lifecycle.test.js'
    : `FAIL lifecycle.test.js: ${failures} failure(s)`);
System.exit(failures === 0 ? 0 : 1);
