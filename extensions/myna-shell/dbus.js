// dbus.js — Gio.DBusProxy for org.myna.Dictation with Gio.bus_watch_name
// lifecycle (feature 004, contracts dbus-interface.md / extension.md X7–X10).
//
// Dormant while the name has no owner (no proxy, no state emissions — X7);
// activates on name-appeared (connects + reflects the current State — X8) and
// clears to idle on name-vanished (daemon crash/exit). disable() removes the
// watch, disconnects the proxy, and drops every subscription (X9); re-enable
// re-establishes cleanly (X10).
//
// The Gio seams (_watchName/_unwatchName/_createProxy) are injectable so the
// lifecycle is contract-testable headless against a stub (test/lifecycle.test.js).

import Gio from 'gi://Gio';

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
     * @param {function} [callbacks._createProxy] - test seam (default a
     *     `Gio.DBusProxy` for the interface).
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
            (() => Gio.DBusProxy.new_sync(
                Gio.DBus.session, Gio.DBusProxyFlags.NONE, null,
                BUS_NAME, OBJECT_PATH, BUS_NAME, null));

        this._watchId = 0;
        this._proxy = null;
        this._proxySignalIds = [];
        this._state = 'idle';
        this._errorMessage = '';
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
        this._state = 'idle';
        this._errorMessage = '';
    }

    _onAppeared() {
        this._teardownProxy();
        this._proxy = this._createProxy();
        this._proxySignalIds = [
            this._proxy.connect('g-signal',
                (_proxy, _sender, signal, params) => {
                    if (signal !== 'StateChanged')
                        return;
                    const [state, errorMessage] = params.deep_unpack();
                    this._setState(state, errorMessage);
                }),
            // Audio level (AudioRms/AudioPeak) updates arrive as property
            // changes; forward the latest for the VU (content-free, energy only).
            this._proxy.connect('g-properties-changed',
                () => this._emitLevel()),
        ];
        this._available = true;
        this._onAvailabilityChanged(true);
        // Reflect the current State so a mid-session daemon start shows the
        // right treatment immediately (X8).
        const state = this._proxy.get_cached_property('State')?.deep_unpack() ??
            'idle';
        const errorMessage =
            this._proxy.get_cached_property('ErrorMessage')?.deep_unpack() ?? '';
        this._setState(state, errorMessage);
        this._emitLevel();
    }

    _emitLevel() {
        if (this._proxy === null)
            return;
        const rms = this._proxy.get_cached_property('AudioRms')?.deep_unpack() ?? 0;
        const peak = this._proxy.get_cached_property('AudioPeak')?.deep_unpack() ?? 0;
        this._onLevel(rms, peak);
    }

    _onVanished() {
        this._teardownProxy();
        this._available = false;
        this._onAvailabilityChanged(false);
        // Daemon gone (crash/exit): clear the goop to idle (X8).
        this._setState('idle', '');
    }

    _setState(state, errorMessage) {
        this._state = state;
        this._errorMessage = errorMessage;
        this._onStateChanged(state, errorMessage);
    }

    _teardownProxy() {
        if (this._proxy === null)
            return;
        for (const id of this._proxySignalIds)
            this._proxy.disconnect(id);
        this._proxySignalIds = [];
        this._proxy = null;
    }
}
