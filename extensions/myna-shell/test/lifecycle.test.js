// lifecycle.test.js — GJS contract test for the org.myna.Dictation proxy
// lifecycle (feature 004-gnome-shell-indicator, contracts extension.md X7–X10),
// driven against a STUB proxy + name watch — no session bus, no Shell.
//
//     gjs -m test/lifecycle.test.js        (from extensions/myna-shell/)
//
// exits 0 when every guarantee holds, 1 otherwise.

import System from 'system';

import {DictationService} from '../dbus.js';
import {stateToDescriptor} from '../states.js';

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
        rms: 0,
        peak: 0,
        _handlers: {},
        connect(signal, cb) {
            this._handlers[signal] = cb;
            return 1;
        },
        disconnect(_id) {
            calls.disconnected++;
        },
        get_cached_property(name) {
            switch (name) {
            case 'State': return variant(this.state);
            case 'ErrorMessage': return variant(this.errorMessage);
            case 'AudioRms': return variant(this.rms);
            case 'AudioPeak': return variant(this.peak);
            default: return null;
            }
        },
        // Test driver: emit StateChanged as the real publisher would.
        emitStateChanged(state, errorMessage = '') {
            this.state = state;
            this.errorMessage = errorMessage;
            this._handlers['g-signal']?.(
                this, null, 'StateChanged', variant([state, errorMessage]));
        },
        // Test driver: emit an audio-level property change.
        emitLevel(rms, peak = rms) {
            this.rms = rms;
            this.peak = peak;
            this._handlers['g-properties-changed']?.(this, variant({}), []);
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
    const levels = [];
    let hides = 0;
    // A spy IndicatorView (view.js shape): records what it's told to render.
    const spyView = {
        show: descriptor => shown.push(descriptor),
        setLevel: (rms, peak) => levels.push([rms, peak]),
        hide: () => hides++,
        destroy: () => {},
    };
    const service = new DictationService({
        onStateChanged: (state, errorMessage) => {
            const descriptor = stateToDescriptor(state, errorMessage);
            if (descriptor.hidden)
                spyView.hide();
            else
                spyView.show(descriptor);
        },
        onLevel: (rms, peak) => spyView.setLevel(rms, peak),
        _watchName: stub.watchName,
        _unwatchName: stub.unwatchName,
        _createProxy: stub.createProxy,
    });
    return {stub, service, shown, levels, hides: () => hides};
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
    const {stub, service, shown, levels, hides} = makeWiredService('recording');
    service.enable();
    stub.appear();
    eq('X8 proxy created on appeared', stub.calls.proxyCreated, 1);
    check('X8 service reports available', service.available);
    eq('X8 reflects current State', shown.at(-1)?.key, 'recording');
    eq('X8 service state mirrors', service.state, 'recording');

    // Live transitions flow through the signal.
    stub.proxy.emitStateChanged('transcribing');
    eq('X8 signal drives the view', shown.at(-1)?.key, 'transcribing');
    stub.proxy.emitStateChanged('error', 'no audio source available');
    eq('X8 error reason reaches the status', shown.at(-1)?.statusText,
        'Error — no audio source available');
    check('X8 error descriptor flagged', shown.at(-1)?.isError === true);

    // Audio level property changes are forwarded for the VU.
    stub.proxy.emitLevel(0.42, 0.6);
    eq('X8 level forwarded to the view', JSON.stringify(levels.at(-1)),
        JSON.stringify([0.42, 0.6]));

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
    // Both proxy subscriptions (StateChanged + properties-changed) are dropped.
    eq('X9 proxy signals disconnected on disable', stub.calls.disconnected, 2);

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
    eq('X10 reflects current State again', shown.at(-1)?.key, 'transcribing');
    service.disable();
}

print(failures === 0
    ? 'PASS lifecycle.test.js'
    : `FAIL lifecycle.test.js: ${failures} failure(s)`);
System.exit(failures === 0 ? 0 : 1);
