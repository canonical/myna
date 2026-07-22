// extension.js — GNOME Shell entry point for the Myna dictation indicator
// (feature 004-gnome-shell-indicator; contract extension.md X7–X12).
//
// The stable wiring: watch org.myna.Dictation (dbus.js), turn each State into a
// semantic descriptor (states.js), and drive whatever IndicatorView the
// factory hands us (view.js) — plus feed it the live audio level. The *look*
// lives entirely behind createView(); swapping/redesigning it never touches
// this file, the proxy, the states, or the contract.
//
// The goop is Shell chrome, never a window, so it can never steal keyboard
// focus (X11/SC-001). disable() drops the proxy + name watch and destroys the
// view — no leaks (X9); re-enable re-establishes cleanly (X10).

import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

import {DictationService} from './dbus.js';
import {stateToDescriptor} from './states.js';
import {createView} from './view.js';

export default class MynaShellExtension extends Extension {
    enable() {
        this._view = createView();
        this._service = new DictationService({
            onStateChanged: (state, errorMessage) => {
                const descriptor = stateToDescriptor(state, errorMessage);
                if (descriptor.hidden)
                    this._view?.hide();
                else
                    this._view?.show(descriptor);
            },
            onLevel: (rms, peak) => this._view?.setLevel(rms, peak),
        });
        this._service.enable();
    }

    disable() {
        this._service?.disable();
        this._service = null;
        this._view?.destroy();
        this._view = null;
    }
}
