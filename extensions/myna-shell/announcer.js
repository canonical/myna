// announcer.js — AT-SPI live announcement for the HUD (T56).
//
// The HUD pill is deliberately non-focusable chrome (DOCK, can_focus=false,
// empty input_region) so a `GtkAccessible::announce()` from the Wayland
// client is not reliably spoken when focus is elsewhere — any unfocused
// client could otherwise spam the screen reader. The reliable anchor is the
// shell chrome itself (always in the AT-SPI tree). This module watches
// `com.canonical.Myna.Dictation` *passively* (no RegisterClient) via the
// shared well-known name proxy's `g-properties-changed` and announces all
// non-idle phases (loading/listening/… + notice/error) via a hidden
// St.Label live region. No visual notification — like a notification for
// Orca, but without a banner.

import Atk from 'gi://Atk';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';

export class DictationAnnouncer {
    /**
     * @param {object} deps
     * @param {DictationProxy} deps.proxy - the shared daemon proxy.
     * @param {function(string):void} [deps.log]
     */
    constructor({proxy, log = () => {}} = {}) {
        this._log = log;
        this._proxy = proxy;
        this._a11yActor = null;
        this._lastState = null;
        this._lastMessage = null;
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

        // The shared proxy owns the name watch and is ready synchronously;
        // the announcer just consumes its g-properties-changed.
        this._proxy.proxy.connectObject('g-properties-changed',
            (p, changed, _invalidated) => this._onPropertiesChanged(changed), this);
        this._reflect();
    }

    disable() {
        this._proxy.proxy?.disconnectObject(this);
        this._a11yActor?.destroy();
        this._a11yActor = null;
        this._lastState = null;
        this._lastMessage = null;
    }

    _reflect() {
        try {
            const stateVar = this._proxy.proxy.get_cached_property('State');
            const messageVar = this._proxy.proxy.get_cached_property('StatusMessage');
            const state = stateVar ? stateVar.unpack() : null;
            const message = messageVar ? messageVar.unpack() : '';
            if (state)
                this._maybeAnnounce(state, message);
        } catch (e) {
            logError(e, 'announcer reflect failed');
        }
    }

    _onPropertiesChanged(changedDict) {
        let state = null;
        let message = null;
        try {
            const s = changedDict.lookup_value('State', null);
            const e = changedDict.lookup_value('StatusMessage', null);
            if (s) state = s.unpack();
            if (e) message = e.unpack();
        } catch (e) {
            logError(e, 'announcer unpack');
            return;
        }

        if (state === null || state === undefined)
            return;

        if (message === null || message === undefined) {
            try {
                const messageVar = this._proxy.proxy?.get_cached_property('StatusMessage');
                message = messageVar ? messageVar.unpack() : '';
            } catch { message = ''; }
        }
        this._maybeAnnounce(state, message || '');
    }

    _maybeAnnounce(state, message) {
        if (state === this._lastState && message === this._lastMessage) return;
        this._lastState = state;
        this._lastMessage = message;

        if (state !== 'idle' && message)
            this._announce(message);
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
