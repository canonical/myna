// extension.js — GNOME Shell entry point for the Myna dictation indicator
// (feature 004-gnome-shell-indicator, 2026-08-26 architecture revision;
// contract extension.md XH1–XH12).
//
// The extension is now a thin OVERLAY HOST, not a renderer. It no longer
// draws the HUD or consumes com.canonical.Myna.Dictation itself — the standalone
// myna-hud application does both. This file launches and hosts that
// application's window as a focus-safe overlay (host.js — spawn, adopt,
// dock-type, position, supervise).
//
// It deliberately does NOT touch the dictation state: the renderer reads it
// directly. Fallback suppression uses `com.canonical.Myna.Dictation`
// `RegisterClient`/`NameOwnerChanged` pruning instead. disable() tears the
// host down with no leaks (XH7); re-enable re-establishes cleanly.

import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import Meta from 'gi://Meta';

import {DictationProxy} from './dictationProxy.js';
import {watchDashToDockStruts} from './dockStrutsConsumer.js';
import {OverlayHost} from './host.js';

export default class MynaShellExtension extends Extension {
    enable() {
        // The overlay host adopts the renderer window through
        // Meta.WaylandClient, which only exists under Wayland. On X11 (mutter
        // that still supports it) we leave `this._host` unset and do nothing —
        // the daemon falls back to desktop notifications — rather than retry a
        // Wayland-only API. Mutter 17+/Shell 49+ dropped X11, so an absent
        // probe means Wayland.
        const isWayland = Meta.is_wayland_compositor?.() ?? true;
        if (!isWayland)
            return;

        // One proxy for the daemon, shared by the host (renderer lifetime
        // from `g-name-owner`) and the announcer (State/StatusMessage). A
        // single name watch; avoid a second bus_watch_name.
        this._proxy = new DictationProxy({log: msg =>
            console.log(`[myna-shell] ${msg}`)});
        this._proxy.connect();

        this._host = new OverlayHost({
            proxy: this._proxy,
            // The primary monitor's work area, so the dock/panel rather than
            // under them (handleRemote owns the maths).
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

        // Wait for startup-complete, as DIN and dash-to-dock do.
        if (Main.layoutManager._startingUp) {
            Main.layoutManager.connectObject('startup-complete',
                () => this._host.enable(), this);
        } else {
            this._host.enable();
        }

        // Follow Main.layoutManager.dashToDockStruts so the pill is never placed
        // auto-hide dock would cover it. `this` is the connectObject owner,
        // so the handler is auto-disconnected when the struts object is
        // destroyed (dash-to-dock disabled).
        this._dockWatch = watchDashToDockStruts(this, extent => {
            this._dockExtent = extent;
            this._host.positionNow();
        });
    }

    disable() {
        if (!this._host)
            return;
        Main.layoutManager.disconnectObject(this);
        this._dockWatch?.disconnect();
        this._dockWatch = null;
        this._dockExtent = null;
        this._host.disable();
        this._host = null;
        this._proxy?.disconnect();
        this._proxy = null;
    }
}
