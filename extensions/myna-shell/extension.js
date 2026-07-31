// extension.js — GNOME Shell entry point for the Myna dictation indicator
// (feature 004-gnome-shell-indicator; contract extension.md X7–X12).
//
// The stable wiring: watch org.myna.Dictation (dbus.js), turn each State into a
// semantic descriptor (states.js), and drive whatever IndicatorView the
// factory hands us (view.js) — plus feed it the live audio level. The *look*
// lives entirely behind createView(); swapping/redesigning it never touches
// this file, the proxy, the states, or the contract.
//
// The HUD pill is Shell chrome, never a window, so it can never steal keyboard
// focus (X11/SC-001). disable() drops the proxy + name watch and destroys the
// view — no leaks (X9); re-enable re-establishes cleanly (X10).

import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import GLib from 'gi://GLib';

import {DictationService} from './dbus.js';
import {IndicatorController} from './indicator-controller.js';
import {connectHudStyle} from './settings-logic.js';
import {stateToDescriptor} from './states.js';
import {createView} from './view.js';

export default class MynaShellExtension extends Extension {
    enable() {
        this._settings = this.getSettings();
        this._controller = new IndicatorController({
            style: this._settings.get_string('hud-style'),
            createView,
            now: () => GLib.get_monotonic_time() / 1000,
            schedule: (delay, callback) => GLib.timeout_add(
                GLib.PRIORITY_DEFAULT, delay, () => {
                    callback();
                    return GLib.SOURCE_REMOVE;
                }),
            cancel: id => GLib.source_remove(id),
        });
        this._disconnectHudStyle = connectHudStyle(
            this._settings, style => this._controller?.setStyle(style));
        this._service = new DictationService({
            onStateChanged: (state, errorMessage) => {
                const descriptor = stateToDescriptor(state, errorMessage);
                this._controller?.onDescriptor(descriptor);
            },
            onLevel: (rms, peak) => this._controller?.onLevel(
                rms, peak, GLib.get_monotonic_time()),
            onAvailabilityChanged: available => {
                if (!available)
                    this._controller?.onServiceUnavailable();
            },
        });
        this._service.enable();
    }

    disable() {
        this._service?.disable();
        this._service = null;
        this._disconnectHudStyle?.();
        this._disconnectHudStyle = null;
        this._settings = null;
        this._controller?.destroy();
        this._controller = null;
    }
}
