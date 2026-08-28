// extension.js — GNOME Shell entry point for the Myna dictation indicator
// (feature 004-gnome-shell-indicator, 2026-08-26 architecture revision;
// contract extension.md XH1–XH12).
//
// The extension is now a thin OVERLAY HOST, not a renderer. It no longer
// draws the HUD or consumes com.canonical.Myna.Dictation itself — the standalone
// myna-hud application does both. This file:
//
//   * launches and hosts that application's window as a focus-safe overlay
//     (host.js — spawn, adopt, dock-type, position, supervise), and
//   * owns com.canonical.Myna.Shell for as long as it is enabled (presence.js), so
//     myna-desktop can suppress its own fallback notification indicator
//     while the shell is presenting the HUD (C12/C13).
//
// It deliberately does NOT touch the dictation state: the renderer reads it
// directly. disable() tears the host and the presence name down with no
// leaks (XH7); re-enable re-establishes cleanly.

import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

import {watchDashToDockStruts} from './dockStrutsConsumer.js';
import {OverlayHost} from './host.js';
import {ShellPresence} from './presence.js';

export default class MynaShellExtension extends Extension {
    enable() {
        this._presence = new ShellPresence();
        this._presence.enable();

        this._host = new OverlayHost({
            // The primary monitor's work area, so the pill sits above the
            // dock/panel rather than under them (place.js owns the maths).
            getMonitorWorkArea: () => {
                const { primaryIndex } = Main.layoutManager;
                if (primaryIndex < 0)
                    return null;
                // getWorkAreaForMonitor returns a Meta.Rectangle BOXED
                // struct: its fields are GObject properties, so object
                // spread ({...workArea}) copies nothing from it. Normalize to
                // a plain object here so the host's placement math (which
                // spreads the work area) sees real values.
                const rect = Main.layoutManager.getWorkAreaForMonitor(primaryIndex);
                return {
                    x: rect.x,
                    y: rect.y,
                    width: rect.width,
                    height: rect.height,
                };
            },
            // An overlay dock (dash-to-dock auto-hide) claims no strut, so it
            // does not shrink the work area; its reserved extent lets the
            // host raise the pill above where the dock would slide out.
            getDockReservedExtent: () => this._dockExtent ?? null,
        });
        this._host.enable();

        // Follow Main.layoutManager.dashToDockStruts so the pill is never placed
        // auto-hide dock would cover it. `this` is the connectObject owner,
        // so the handler is auto-disconnected when the struts object is
        // destroyed (dash-to-dock disabled).
        this._dockWatch = watchDashToDockStruts(this, extent => {
            this._dockExtent = extent;
            this._host?.positionNow();
        });
    }

    disable() {
        this._dockWatch?.disconnect();
        this._dockWatch = null;
        this._dockExtent = null;
        this._host?.disable();
        this._host = null;
        this._presence?.disable();
        this._presence = null;
    }
}
