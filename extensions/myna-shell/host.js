// host.js — the overlay host (feature 004-gnome-shell-indicator, 2026-08-26
// architecture revision; research R21/R27; contract extension.md
// XH1–XH12). The stateful glue that launches the standalone myna-hud
// renderer, adopts its window as a focus-safe overlay, positions it, and
// supervises it — everything that needs Shell/mutter APIs. The *rules* it
// obeys are the pure modules it composes:
//
//   resolve.js   — how to launch the renderer (snap run / override)
//   place.js     — where the overlay goes
//   respawn.js   — what to do when it exits
//   presence.js  — owning org.myna.Shell while hosting
//
// This file is intentionally thin: it wires those decisions to
// Meta.WaylandClient, Meta.Window and the Shell's signals, and holds the
// live handles so disable() can tear everything down (XH7).

import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';

import {computePlacement, placementChanged} from './place.js';
import {initialState, planRestart} from './respawn.js';
import {resolveHudLaunch} from './resolve.js';

// Await the subprocess with a Cancellable instead of a bare callback, so
// disable() can cancel the wait rather than relying on a flag to ignore a
// late callback. Promisified once at module load (idempotent).
Gio._promisify(Gio.Subprocess.prototype, 'wait_async', 'wait_finish');

/** A GLib-style env predicate: the file exists and is executable, OR it is a
 * bare command name found on PATH (for `snap`). */
function isExecutable(path) {
    if (path.includes('/'))
        return GLib.file_test(path, GLib.FileTest.IS_EXECUTABLE);
    return GLib.find_program_in_path(path) !== null;
}

/**
 * Hosts exactly one renderer window as an overlay.
 *
 * @param {object} deps
 * @param {function(): (object|null)} deps.getMonitorWorkArea - returns the
 *     primary monitor's work area `{x, y, width, height}` (injected so the
 *     host is testable without a live Shell; the extension passes the real
 *     `Main.layoutManager.getWorkAreaForMonitor` wrapper).
 * @param {function(string): void} [deps.log] - single-line logger.
 */
export class OverlayHost {
    constructor({getMonitorWorkArea, log = msg => console.log(`[myna-shell] ${msg}`)}) {
        this._getMonitorWorkArea = getMonitorWorkArea;
        this._log = log;

        this._client = null;         // Meta.WaylandClient
        this._window = null;         // the adopted Meta.Window
        this._restartState = initialState();
        this._dormant = false;

        this._mapId = 0;
        this._positionHandlerIds = [];   // on the adopted window
        this._layoutHandlerIds = [];     // monitors/work-area
        this._restartTimeoutId = 0;
        this._launchedAtMs = 0;
        this._enabled = false;

        // Cancels the current subprocess wait. A fresh Cancellable is made
        // for each spawn rather than reset()-ing one (reset is discouraged
        // and error-prone once an operation has touched it); disable()
        // cancels whichever is current. _watchExit captures its own
        // instance, so a stale wait can never act on a newer generation's
        // cancellable.
        this._cancellable = null;
    }

    /** Launch the renderer and begin hosting. Idempotent-safe: a second
     * enable() while already hosting is a no-op. */
    enable() {
        if (this._enabled)
            return;
        this._enabled = true;
        this._dormant = false;
        this._restartState = initialState();
        this._spawn();
    }

    /** Terminate the renderer, drop the window, disconnect everything
     * (XH7). Safe to call when never enabled or already disabled. */
    disable() {
        this._enabled = false;
        this._cancelPendingRestart();
        this._disconnectLayout();
        this._disconnectWindow();

        // Cancel the current subprocess wait so its promise rejects as
        // cancelled rather than resolving into _onRendererExited after we
        // have torn down — no late respawn.
        this._cancellable?.cancel();
        this._cancellable = null;

        if (this._mapId) {
            global.window_manager.disconnect(this._mapId);
            this._mapId = 0;
        }
        // Terminating the client kills its subprocess and reaps the adopted
        // window with it.
        try {
            this._client?.destroy();
        } catch (e) {
            logError(e, '[myna-shell] error tearing down renderer client');
        }
        this._client = null;
        this._window = null;
    }

    /** Whether the host has given up after exhausting the restart budget
     * (XH3). Exposed for the extension's presence/logging. */
    get dormant() {
        return this._dormant;
    }

    // ── Launch ──────────────────────────────────────────────────────────

    _spawn() {
        const launch = resolveHudLaunch({getenv: GLib.getenv, isExecutable});
        if (launch.argv === null) {
            this._log(
                `renderer not found (${launch.source}); staying dormant — ` +
                `set ${'MYNA_HUD_BINARY'} or install the myna snap`);
            this._dormant = true;
            return;
        }

        let client;
        try {
            // Launch THROUGH Meta.WaylandClient so the child inherits the
            // compositor's Wayland socket and we can own its window (R27).
            const launcher = new Gio.SubprocessLauncher({
                flags: Gio.SubprocessFlags.NONE,
            });
            client = Meta.WaylandClient.new_subprocess(
                global.context, launcher, launch.argv);
        } catch (e) {
            logError(e, `[myna-shell] failed to launch renderer (${launch.source})`);
            this._scheduleRestart(/* expected= */ false, /* uptimeMs= */ 0);
            return;
        }

        this._client = client;
        this._launchedAtMs = GLib.get_monotonic_time() / 1000;
        this._log(`launched renderer via ${launch.source}: ${launch.argv.join(' ')}`);

        // Adopt the window on MAP, not on `window-created`. window-created
        // fires before the surface is committed, when owns_window() cannot
        // yet match reliably; the window_manager `map` signal fires once the
        // actor exists, which is what DIN (desktop-icons-ng) uses. It also
        // handles the renderer hiding at idle and re-showing: every re-map
        // re-checks ownership, so a window that unmaps and maps again is
        // re-adopted rather than missed.
        this._mapId = global.window_manager.connect_after(
            'map', (_wm, actor) => this._onWindowMapped(actor.get_meta_window()));

        // Watch the subprocess so an exit drives the respawn policy. A fresh
        // Cancellable per spawn, captured by this wait: disable() cancels
        // whatever is current, and _watchExit checks the very instance it was
        // handed — so a superseded wait never confuses the current one.
        const cancellable = new Gio.Cancellable();
        this._cancellable = cancellable;
        this._watchExit(client.get_subprocess(), cancellable);
    }

    async _watchExit(subprocess, cancellable) {
        if (!subprocess)
            return;
        try {
            await subprocess.wait_async(cancellable);
        } catch (e) {
            // A cancelled wait is the disable() path, not a failure. Anything
            // else is a genuine error worth surfacing.
            if (!e.matches?.(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED))
                logError(e, '[myna-shell] error awaiting renderer exit');
            return;
        }
        // Any exit — clean or not — is unexpected while still enabled, and
        // drives the respawn policy. Once cancelled, disable() terminated the
        // renderer itself, so it is not an incident.
        if (!cancellable.is_cancelled())
            this._onRendererExited();
    }

    // ── Adoption (XH4) ──────────────────────────────────────────────────

    _onWindowMapped(window) {
        if (!window)
            return;
        // Already adopted: the renderer hid at idle and re-mapped. It is the
        // same Meta.Window, so re-assert the overlay treatment (mutter can
        // reset some of it across an unmap) and reposition, but do not
        // re-run the one-time wiring.
        if (window === this._window) {
            this._makeOverlay(window);
            this._position();
            return;
        }
        if (this._window)
            return;   // a different, second window (a dialog) — not the HUD

        // owns_window() throws on an X11 window (e.g. anything via XWayland),
        // so guard it: an unrelated window mapping must never take the host
        // down, and a renderer that connected over XWayland instead of the
        // injected Wayland socket simply is not ours.
        let owns = false;
        try {
            owns = this._client?.owns_window(window) ?? false;
        } catch (e) {
            // X11 window (or the client is gone) — not ours.
            return;
        }
        if (!owns)
            return;

        this._window = window;
        this._log('adopted renderer window');
        this._makeOverlay(window);
        this._position();
        this._connectLayout();
        this._connectWindowPosition(window);
    }

    /** Dock-typed, hidden from the window list, on all workspaces, above
     * normal windows, and never focused on map (XH10). */
    _makeOverlay(window) {
        try {
            window.set_type(Meta.WindowType.DOCK);
            window.hide_from_window_list();
            window.stick();            // all workspaces
            window.make_above();       // above normal windows
        } catch (e) {
            logError(e, '[myna-shell] error configuring overlay window');
        }
    }

    // ── Positioning (XH1) ───────────────────────────────────────────────

    _position() {
        if (!this._window)
            return;
        const workArea = this._getMonitorWorkArea();
        if (!workArea)
            return;

        const frame = this._window.get_frame_rect();
        const target = computePlacement(
            workArea, {width: frame.width, height: frame.height});
        const current = {x: frame.x, y: frame.y};
        if (!placementChanged(current, target))
            return;

        // Anti-feedback: our own move fires size/position signals, which
        // would re-enter _position(). Mute the window handlers around the
        // programmatic move.
        this._disconnectWindow();
        try {
            this._window.move_frame(false, target.x, target.y);
        } catch (e) {
            logError(e, '[myna-shell] error positioning overlay window');
        }
        this._connectWindowPosition(this._window);
    }

    _connectLayout() {
        // monitors-changed / work-area changes move the target.
        this._layoutHandlerIds.push([
            global.display,
            global.display.connect('workareas-changed', () => this._position()),
        ]);
        const monitorManager = global.backend.get_monitor_manager?.();
        if (monitorManager) {
            this._layoutHandlerIds.push([
                monitorManager,
                monitorManager.connect('monitors-changed', () => this._position()),
            ]);
        }
    }

    _connectWindowPosition(window) {
        // The renderer resizes as its content changes (idle→active,
        // wrapping errors); recentre when it does.
        this._positionHandlerIds.push([
            window, window.connect('size-changed', () => this._position()),
        ]);
        this._positionHandlerIds.push([
            window, window.connect('position-changed', () => this._position()),
        ]);
    }

    _disconnectWindow() {
        for (const [obj, id] of this._positionHandlerIds) {
            try {
                obj.disconnect(id);
            } catch (_e) { /* window may be gone */ }
        }
        this._positionHandlerIds = [];
    }

    _disconnectLayout() {
        for (const [obj, id] of this._layoutHandlerIds) {
            try {
                obj.disconnect(id);
            } catch (_e) { /* ignore */ }
        }
        this._layoutHandlerIds = [];
    }

    // ── Supervision (XH3) ───────────────────────────────────────────────

    /** Any renderer exit while the host is still enabled — clean OR crashing
     * — is unexpected and drives the respawn policy (XH3). We never asked it
     * to quit: the host owns the renderer's lifetime, so `exit(0)` is as much
     * a surprise as a segfault. The only expected exit is the one disable()
     * causes, and that path is suppressed upstream (the wait is cancelled).
     */
    _onRendererExited() {
        if (!this._enabled)
            return;   // disable() terminated it; not an incident
        const uptimeMs = GLib.get_monotonic_time() / 1000 - this._launchedAtMs;
        this._disconnectWindow();
        this._disconnectLayout();
        if (this._mapId) {
            global.window_manager.disconnect(this._mapId);
            this._mapId = 0;
        }
        this._window = null;
        this._client = null;
        this._scheduleRestart(/* expected= */ false, uptimeMs);
    }

    _scheduleRestart(expected, uptimeMs) {
        const plan = planRestart(this._restartState, {expected, uptimeMs});
        this._restartState = {consecutiveFailures: plan.consecutiveFailures};

        if (plan.dormant) {
            this._dormant = true;
            this._log(
                `renderer failed ${plan.consecutiveFailures} times; ` +
                'giving up (dormant) — will retry on next enable');
            return;
        }
        if (!plan.restart)
            return;

        this._restartTimeoutId = GLib.timeout_add(
            GLib.PRIORITY_DEFAULT, plan.delayMs, () => {
                this._restartTimeoutId = 0;
                if (this._enabled)
                    this._spawn();
                return GLib.SOURCE_REMOVE;
            });
    }

    _cancelPendingRestart() {
        if (this._restartTimeoutId) {
            GLib.source_remove(this._restartTimeoutId);
            this._restartTimeoutId = 0;
        }
    }
}
