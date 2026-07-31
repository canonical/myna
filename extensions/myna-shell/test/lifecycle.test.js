// lifecycle.test.js — GJS contract test for the org.myna.Dictation proxy
// lifecycle (feature 004-gnome-shell-indicator, contracts extension.md X7–X10),
// driven against a STUB proxy + name watch — no session bus, no Shell.
//
//     gjs -m test/lifecycle.test.js        (from extensions/myna-shell/)
//
// exits 0 when every guarantee holds, 1 otherwise.

import System from 'system';

import {DictationService} from '../dbus.js';
import {IndicatorController} from '../indicator-controller.js';
import {connectHudStyle} from '../settings-logic.js';
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
// records connect/disconnect and can be driven through property changes (the
// publisher's only update channel — see dbus.js's header).
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
        // Test driver: a state transition, pushed as a property change the
        // way the real publisher does it (State + ErrorMessage sets, then
        // PropertiesChanged).
        emitState(state, errorMessage = '') {
            this.errorMessage = errorMessage;
            this.state = state;
            this._handlers['g-properties-changed']?.(this, variant({}), []);
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

// Feature 009 settings lifecycle: connect before initial read, propagate live
// changes, and become inert after disable.
{
    const seen = [];
    const settings = {
        value: 'basic',
        callback: null,
        disconnected: 0,
        connect(signal, callback) {
            check('009 listens only to detailed hud-style signal',
                signal === 'changed::hud-style');
            this.callback = callback;
            return 7;
        },
        disconnect(id) {
            check('009 disconnects the installed settings signal', id === 7);
            this.disconnected++;
        },
        get_string() {
            return this.value;
        },
        emit(value) {
            this.value = value;
            this.callback?.();
        },
    };
    const disconnect = connectHudStyle(settings, style => seen.push(style));
    eq('009 initial style read happens after connect', seen[0], 'basic');
    settings.emit('wave');
    eq('009 live settings change propagates', seen.at(-1), 'wave');
    disconnect();
    settings.emit('basic');
    eq('009 settings change after disable is inert', seen.at(-1), 'wave');
    disconnect();
    eq('009 settings disconnect is idempotent', settings.disconnected, 1);
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

    // Live transitions flow through pushed property changes.
    stub.proxy.emitState('transcribing');
    eq('X8 pushed change drives the view', shown.at(-1)?.key, 'transcribing');
    stub.proxy.emitState('error', 'no audio source available');
    eq('X8 error reason reaches the status', shown.at(-1)?.statusText,
        'Error — no audio source available');
    check('X8 error descriptor flagged', shown.at(-1)?.severity === 'critical');

    // Audio level property changes are forwarded for the VU.
    stub.proxy.emitLevel(0.42, 0.6);
    eq('X8 level forwarded to the view', JSON.stringify(levels.at(-1)),
        JSON.stringify([0.42, 0.6]));

    // A fresh level update is meaningful even when the quantized values are
    // numerically identical: HudView uses arrival time for stale-decay. If the
    // proxy deduplicates this update, a steady signal falsely decays to a flat
    // VU floor after STALE_MS (the manual-test regression from 2026-07-30).
    const beforeRepeat = levels.length;
    stub.proxy.emitLevel(0.42, 0.6);
    eq('X8 repeated fresh level refreshes VU timestamp',
        levels.length, beforeRepeat + 1);
    eq('X8 repeated level values are forwarded unchanged',
        JSON.stringify(levels.at(-1)), JSON.stringify([0.42, 0.6]));

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
    // The proxy subscription (g-properties-changed) is dropped.
    eq('X9 proxy signals disconnected on disable', stub.calls.disconnected, 1);

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

// Feature 009: the same service stream drives either view through one
// presentation-independent controller. Equal levels stay fresh and unknown
// states retain the neutral descriptor mapping.
{
    const stub = makeStubBus('recording');
    const views = [];
    const controller = new IndicatorController({
        style: 'basic',
        createView: (style, _options) => {
            const calls = [];
            const view = {
                style,
                calls,
                show: descriptor => calls.push(['show', descriptor]),
                setLevel: (rms, peak, receivedAt) => calls.push(['level', rms, peak, receivedAt]),
                hide: () => calls.push(['hide']),
                destroy: () => calls.push(['destroy']),
            };
            views.push(view);
            return view;
        },
        now: () => 1000,
        schedule: () => 1,
        cancel: () => {},
    });
    let arrival = 1000;
    const service = new DictationService({
        onStateChanged: (state, reason) => controller.onDescriptor(
            stateToDescriptor(state, reason)),
        onLevel: (rms, peak) => controller.onLevel(rms, peak, arrival++),
        onAvailabilityChanged: available => {
            if (!available)
                controller.onServiceUnavailable();
        },
        _watchName: stub.watchName,
        _unwatchName: stub.unwatchName,
        _createProxy: stub.createProxy,
    });
    service.enable();
    stub.appear();
    stub.proxy.emitState('quantizing');
    eq('009 unknown state stays neutral through controller',
        views[0].calls.filter(c => c[0] === 'show').at(-1)[1].key, 'active');
    stub.proxy.emitLevel(0.4, 0.6);
    stub.proxy.emitLevel(0.4, 0.6);
    const levelCalls = views[0].calls.filter(c => c[0] === 'level');
    check('009 repeated equal levels keep distinct arrival timestamps',
        levelCalls.at(-1)[3] > levelCalls.at(-2)[3]);
    controller.setStyle('wave');
    eq('009 second style receives same semantic descriptor',
        views[1].calls.find(c => c[0] === 'show')[1].key, 'active');
    stub.vanish();
    eq('009 service loss clears ordinary display',
        JSON.stringify(views[1].calls.at(-1)), JSON.stringify(['hide']));
    service.disable();
    controller.destroy();
}

print(failures === 0
    ? 'PASS lifecycle.test.js'
    : `FAIL lifecycle.test.js: ${failures} failure(s)`);
System.exit(failures === 0 ? 0 : 1);
