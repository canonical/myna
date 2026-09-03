// dictationProxy.js — the single well-known-name proxy for
// com.canonical.Myna.Dictation, shared by every consumer in the extension.
//
// Both the overlay host (to run the renderer only while the daemon is on the
// bus) and the announcer (to read State/StatusMessage) talk to the daemon.
// Creating the proxy once here means one `Gio.DBusProxy` and one source of
// truth; consumers connect to its native signals (`g-name-owner`,
// `g-properties-changed`) rather than each running their own
// `Gio.bus_watch_name`.
//
// The proxy is created asynchronously (so it can be cancelled on teardown)
// and handed out through `proxy` once resolved. Nothing else is wrapped.

import Gio from 'gi://Gio';

const BUS_NAME = 'com.canonical.Myna.Dictation';
const OBJECT_PATH = '/com/canonical/Myna/Dictation';
const IFACE = 'com.canonical.Myna.Dictation';

export class DictationProxy {
    constructor({log = () => {}} = {}) {
        this._log = log;
        this._proxy = null;
        this._cancellable = null;
    }

    /** Create the proxy asynchronously. DO_NOT_AUTO_START means it only
     * reflects whether the name is owned, so it never brings the daemon up.
     * Cancellable, so a teardown that happens while creation is in flight can
     * abort it. */
    connect() {
        if (this._proxy)
            return;

        this._cancellable?.cancel();
        const cancellable = new Gio.Cancellable();
        this._cancellable = cancellable;
        Gio.DBusProxy.new_for_bus(
            Gio.BusType.SESSION,
            Gio.DBusProxyFlags.DO_NOT_CONNECT_SIGNALS |
            Gio.DBusProxyFlags.DO_NOT_AUTO_START,
            null,
            BUS_NAME,
            OBJECT_PATH,
            IFACE,
            cancellable,
            (source, res) => {
                try {
                    this._proxy = Gio.DBusProxy.new_for_bus_finish(res);
                } catch (e) {
                    if (!e.matches?.(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED))
                        this._log(`dictation proxy unavailable: ${e.message ?? e}`);
                }
            }
        );
    }

    /** Drop the proxy, cancelling any in-flight creation. Consumers'
     * disconnectObject() handles the signals they attached. */
    disconnect() {
        this._cancellable?.cancel();
        this._cancellable = null;
        this._proxy?.disconnectObject(this);
        this._proxy = null;
    }

    /** The live Gio.DBusProxy, or null while creation is in flight (connect
     * before most consumers, so it resolves before they need it). */
    get proxy() {
        return this._proxy;
    }

    /** Whether the daemon currently owns the name (null before resolution /
     * after disconnect reads as absent). */
    get present() {
        return (this._proxy?.get_name_owner() ?? null) !== null;
    }
}
