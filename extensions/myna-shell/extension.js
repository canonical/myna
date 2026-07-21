// extension.js — GNOME Shell entry point for the Myna dictation indicator
// (feature 004-gnome-shell-indicator; contract extension.md X7–X12).
//
// enable(): wire the org.myna.Dictation proxy (dbus.js) through the pure
// states.js intent to the goop (indicator.js) living on the Shell chrome —
// never a window, so it can never steal keyboard focus (X11/SC-001).
// disable(): drop the proxy + name watch and destroy every actor/timer/
// subscription (X9 — no leaks); re-enable re-establishes cleanly (X10).

import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

import {DictationService} from './dbus.js';
import {GoopIndicator} from './indicator.js';
import {stateToIntent} from './states.js';

export default class MynaShellExtension extends Extension {
    enable() {
        this._indicator = new GoopIndicator();
        this._service = new DictationService({
            onStateChanged: (state, errorMessage) => {
                const intent = stateToIntent(state, errorMessage);
                if (intent.hidden)
                    this._indicator?.hide();
                else
                    this._indicator?.show(intent);
            },
        });
        this._service.enable();
    }

    disable() {
        this._service?.disable();
        this._service = null;
        this._indicator?.destroy();
        this._indicator = null;
    }
}
