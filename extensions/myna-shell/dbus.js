// dbus.js — Gio.DBusProxy for org.myna.Dictation with Gio.bus_watch_name
// lifecycle (feature 004, contracts dbus-interface.md / extension.md X7–X10).
//
// Dormant while the name has no owner (no proxy, no state emissions — X7);
// activates on name-appeared (connects + reflects the current State — X8) and
// clears to idle on name-vanished (daemon crash/exit). disable() removes the
// watch, disconnects the proxy, and drops every subscription (X9); re-enable
// re-establishes cleanly (X10).
//
// All updates arrive one way: the standard
// org.freedesktop.DBus.Properties.PropertiesChanged signal. The proxy applies
// it to its property cache before emitting g-properties-changed, so we simply
// re-read the cached State/ErrorMessage/AudioRms/AudioPeak and forward what
// changed. This is the one push channel that works for EVERY publisher,
// confined or not: snapd's dbus slot policy lets a strictly-confined service
// send on its own path with the Properties interface to
// peer=(name=org.freedesktop.DBus, label=unconfined) — which is exactly the
// shape dbus-daemon assigns a destination-less broadcast — while
// custom-interface signals to unconfined peers are AppArmor-denied. The
// org.myna.Dictation interface therefore defines no custom signals —
// properties only (contract dbus-interface.md §Confinement).
//
// The Gio seams (_watchName/_unwatchName/_createProxy) are injectable so the
// lifecycle is contract-testable headless against a stub (test/lifecycle.test.js).
//
// The proxy is built asynchronously. This runs on the compositor's main
// loop, so a sync round trip stalls the whole desktop until the daemon
// answers; `_createProxy` therefore takes `(cancellable, callback)` rather
// than returning a proxy, and `disable()` cancels one in flight.

import Gio from 'gi://Gio';

import {DictationState} from './states.js';

const BUS_NAME = 'org.myna.Dictation';
const OBJECT_PATH = '/org/myna/Dictation';

export class DictationService {
    /**
     * @param {object} callbacks
     * @param {function(string, string): void} [callbacks.onStateChanged] -
     *     called with `(state, errorMessage)` on every transition, on
     *     name-appeared (reflecting the current State), and with
     *     `('idle', '')` on name-vanished.
     * @param {function(boolean): void} [callbacks.onAvailabilityChanged] -
     *     called when the name gains/loses an owner (drives panel-button dim).
     * @param {function} [callbacks._watchName] - test seam (default
     *     `Gio.bus_watch_name` on the session bus for org.myna.Dictation).
     * @param {function} [callbacks._unwatchName] - test seam (default
     *     `Gio.bus_unwatch_name`).
     * @param {function} [callbacks._createProxy] - test seam, called as
     *     `(cancellable, callback)`; must invoke `callback(proxy|null)` when
     *     the proxy is ready.
     */
    constructor({
        onStateChanged = null,
        onAvailabilityChanged = null,
        onLevel = null,
        _watchName = null,
        _unwatchName = null,
        _createProxy = null,
    } = {}) {
        this._onStateChanged = onStateChanged ?? (() => {});
        this._onAvailabilityChanged = onAvailabilityChanged ?? (() => {});
        this._onLevel = onLevel ?? (() => {});
        this._watchName = _watchName ??
            ((appeared, vanished) => Gio.bus_watch_name(
                Gio.BusType.SESSION, BUS_NAME, Gio.BusNameWatcherFlags.NONE,
                appeared, vanished));
        this._unwatchName = _unwatchName ?? Gio.bus_unwatch_name;
        this._createProxy = _createProxy ??
            ((cancellable, callback) => {
                Gio.DBusProxy.new(
                    Gio.DBus.session, Gio.DBusProxyFlags.NONE, null,
                    BUS_NAME, OBJECT_PATH, BUS_NAME, cancellable,
                    (_source, result) => {
                        let proxy = null;
                        try {
                            proxy = Gio.DBusProxy.new_finish(result);
                        } catch (e) {
                            if (!e.matches(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED))
                                logError(e, 'myna-shell: org.myna.Dictation proxy failed');
                        }
                        callback(proxy);
                    });
            });

        this._watchId = 0;
        this._proxy = null;
        this._proxyCancellable = null;
        this._proxySignalIds = [];
        this._state = DictationState.IDLE;
        this._errorMessage = '';
        this._rms = 0;
        this._peak = 0;
        this._available = false;
    }

    /** Current wire State (idle unless connected + told otherwise). */
    get state() {
        return this._state;
    }

    /** Content-free reason, set only while state == error. */
    get errorMessage() {
        return this._errorMessage;
    }

    /** Whether org.myna.Dictation currently has a bus owner. */
    get available() {
        return this._available;
    }

    /** Start watching the name. Idempotent (a live watch is kept). */
    enable() {
        if (this._watchId !== 0)
            return;
        this._watchId = this._watchName(
            (_conn, _name, _owner) => this._onAppeared(),
            (_conn, _name) => this._onVanished());
    }

    /** Tear down watch + proxy + subscriptions. Safe when dormant. */
    disable() {
        if (this._watchId !== 0) {
            this._unwatchName(this._watchId);
            this._watchId = 0;
        }
        this._teardownProxy();
        this._available = false;
        this._state = DictationState.IDLE;
        this._errorMessage = '';
    }

    _onAppeared() {
        this._teardownProxy();
        const cancellable = new Gio.Cancellable();
        this._proxyCancellable = cancellable;
        this._createProxy(cancellable, proxy => {
            // disable()/vanish raced us to it, or construction failed.
            if (this._proxyCancellable !== cancellable || proxy === null)
                return;
            this._proxyCancellable = null;
            this._proxy = proxy;
            // State transitions and audio levels alike ride PropertiesChanged
            // (the header explains why there is no custom signal to join).
            this._proxySignalIds = [
                proxy.connect('g-properties-changed',
                    () => this._reflectCached()),
            ];
            this._available = true;
            this._onAvailabilityChanged(true);
            // Reflect the current State so a mid-session daemon start shows
            // the right treatment immediately (X8).
            this._reflectCached();
        });
    }

    // Forward the cached snapshot, deduped — the publisher pushes every
    // change, so the cache is always current when this runs.
    _reflectCached() {
        if (this._proxy === null)
            return;
        const state = this._proxy.get_cached_property('State')?.deep_unpack() ??
            DictationState.IDLE;
        const errorMessage =
            this._proxy.get_cached_property('ErrorMessage')?.deep_unpack() ?? '';
        const rms = this._proxy.get_cached_property('AudioRms')?.deep_unpack() ?? 0;
        const peak = this._proxy.get_cached_property('AudioPeak')?.deep_unpack() ?? 0;
        this._setState(state, errorMessage);
        this._setLevel(rms, peak);
    }

    _setLevel(rms, peak) {
        // Do not deduplicate numerically-identical level updates. Arrival time
        // is part of the VU contract: HudView uses each fresh update to reset
        // stale-decay. A steady/quantized signal can legitimately publish the
        // same RMS/peak for many frames; dropping those arrivals made a live
        // microphone decay to the flat floor after ~300 ms.
        this._rms = rms;
        this._peak = peak;
        this._onLevel(rms, peak);
    }

    _onVanished() {
        this._teardownProxy();
        this._available = false;
        this._onAvailabilityChanged(false);
        // Daemon gone (crash/exit): clear the HUD pill to idle (X8).
        this._setState(DictationState.IDLE, '');
    }

    _setState(state, errorMessage) {
        // Dedupe: PropertiesChanged fires per property, so a multi-property
        // transition re-reflects the current state; only real transitions
        // may re-drive the view.
        if (state === this._state && errorMessage === this._errorMessage)
            return;
        this._state = state;
        this._errorMessage = errorMessage;
        this._onStateChanged(state, errorMessage);
    }

    _teardownProxy() {
        if (this._proxyCancellable !== null) {
            this._proxyCancellable.cancel();
            this._proxyCancellable = null;
        }
        if (this._proxy === null)
            return;
        for (const id of this._proxySignalIds)
            this._proxy.disconnect(id);
        this._proxySignalIds = [];
        this._proxy = null;
    }
}
