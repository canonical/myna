// announcer.js — AT-SPI live announcement for the HUD (T56).
//
// The HUD pill is deliberately non-focusable chrome (DOCK, can_focus=false,
// empty input_region) so a `GtkAccessible::announce()` from the Wayland
// client is not reliably spoken when focus is elsewhere — any unfocused
// client could otherwise spam the screen reader. The reliable anchor is the
// shell chrome itself (always in the AT-SPI tree). This module watches
// `com.canonical.Myna.Dictation` *passively* (no RegisterClient) via the
// well-known name proxy's `g-properties-changed` and announces all
// non-idle phases (loading/listening/… + notice/error) via a hidden
// St.Label live region. No visual notification — like a notification for
// Orca, but without a banner.

import Atk from 'gi://Atk';
import Gio from 'gi://Gio';
import St from 'gi://St';
import {gettext as _} from 'gettext';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';

const BUS_NAME = 'com.canonical.Myna.Dictation';
const OBJECT_PATH = '/com/canonical/Myna/Dictation';
const IFACE = 'com.canonical.Myna.Dictation';

function descriptorFor(state, reason) {
    // Mirrors `myna-hud/src/states.rs::base_for` + `state_to_descriptor`.
    // Announce the same content-free status_text the pill shows — all
    // non-idle phases. Unknown values degrade to "Active" like the HUD.
    // All msgids are marked for xgettext via `_()` — the pot is
    // `extensions/myna-shell/po` (domain MynaShellExtension) plus the
    // shared `client/myna-hud/po` (domain myna). Shell's `gettext` import
    // is domain-bound to MynaShellExtension when run as an extension; under
    // plain `gjs -m` it falls back to identity (English source).
    if (state === 'idle' || !state) return null;
    if (state === 'loading') return _('Loading model…');
    if (state === 'recording') return _('Listening');
    if (state === 'transcribing') return _('Transcribing');
    if (state === 'finalizing') return _('Finishing');
    if (state === 'notice') {
        return reason && reason.length > 0 ? reason : _('No speech detected');
    }
    if (state === 'error') {
        return reason && reason.length > 0 ? _('Error — %s').format(reason) : _('Error');
    }
    return _('Active');
}

export class DictationAnnouncer {
    /**
     * @param {object} deps
     * @param {function(string):void} [deps.log]
     */
    constructor({log = () => {}} = {}) {
        this._log = log;
        this._proxy = null;
        this._signalId = 0;
        this._a11yActor = null;
        this._lastState = null;
        this._lastReason = null;
        this._cancellable = null;
    }

    enable() {
        // Hidden live region — mirrors real shell's
        // `js/ui/messageList.js:480 Atk.Role.NOTIFICATION` (the message tray's
        // own role). Not `ALERT` — notice is transient status, not an assertive
        // alert. Always in the a11y tree even though opacity 0 / off-screen.
        this._a11yActor = new St.Label({
            text: '',
            visible: true,
            opacity: 0,
            x: -1000,
            can_focus: false,
            accessible_role: Atk.Role.NOTIFICATION,
        });
        Main.layoutManager.uiGroup.add_child(this._a11yActor);

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
                    this._signalId = this._proxy.connect('g-properties-changed',
                        (p, changed, _invalidated) => this._onPropertiesChanged(changed));
                    this._reflectFromProxy();
                } catch (e) {
                    if (!e.matches?.(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED))
                        this._log(`announcer: async proxy unavailable: ${e.message ?? e}`);
                }
            }
        );
    }

    disable() {
        this._cancellable?.cancel();
        this._cancellable = null;
        if (this._proxy && this._signalId) {
            try { this._proxy.disconnect(this._signalId); } catch {}
        }
        this._proxy = null;
        this._signalId = 0;
        if (this._a11yActor) {
            try { Main.layoutManager.uiGroup.remove_child(this._a11yActor); } catch {}
            this._a11yActor.destroy();
            this._a11yActor = null;
        }
        this._lastState = null;
        this._lastReason = null;
    }

    _reflectFromProxy() {
        if (!this._proxy) return;
        try {
            const stateVar = this._proxy.get_cached_property('State');
            const errVar = this._proxy.get_cached_property('ErrorMessage');
            const state = stateVar ? stateVar.unpack() : null;
            const err = errVar ? errVar.unpack() : '';
            if (state)
                this._maybeAnnounce(state, err);
        } catch {}
    }

    _onPropertiesChanged(changedDict) {
        let state = null;
        let reason = null;
        try {
            const s = changedDict.lookup_value('State', null);
            const e = changedDict.lookup_value('ErrorMessage', null);
            if (s) state = s.unpack();
            if (e) reason = e.unpack();
        } catch (e) {
            logError(e, 'announcer unpack');
            return;
        }

        if (state === null || state === undefined)
            return;

        if (reason === null || reason === undefined) {
            try {
                const errVar = this._proxy?.get_cached_property('ErrorMessage');
                if (errVar) reason = errVar.unpack();
                else reason = '';
            } catch { reason = ''; }
        }
        this._maybeAnnounce(state, reason || '');
    }

    _maybeAnnounce(state, reason) {
        if (state === this._lastState && reason === this._lastReason) return;
        this._lastState = state;
        this._lastReason = reason;

        const text = descriptorFor(state, reason);
        if (!text) return;

        this._announce(text);
    }

    _announce(text) {
        if (!this._a11yActor) return;
        try {
            this._a11yActor.text = text;
            this._a11yActor.get_accessible()?.emit?.('announcement', text);
        } catch (e) {
            logError(e, 'announce failed');
        }
    }
}
