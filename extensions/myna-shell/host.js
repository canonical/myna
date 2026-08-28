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
//
// This file is intentionally thin: it wires those decisions to
// Meta.WaylandClient, Meta.Window and the Shell's signals, and holds the
// live handles so disable() can tear everything down (XH7).

import GObject from 'gi://GObject';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';

import {computePlacement, placementChanged, shrinkWorkAreaForDock} from './place.js';
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
 * @param {function(): ({x: number, y: number, width: number, height: number}|null)} [deps.getDockReservedExtent]
 *     - returns the region reserved on the pill's monitor by an overlay
 *     dock that claims no strut (dash-to-dock in auto-hide mode), or null
 *     when none. The host raises the pill above it so it is never covered
 *     when the dock slides out.
 * @param {function(string): void} [deps.log] - single-line logger.
 */
export class OverlayHost {
    constructor({
        getMonitorWorkArea,
        getDockReservedExtent = () => null,
        log = msg => console.log(`[myna-shell] ${msg}`),
    }) {
        this._getMonitorWorkArea = getMonitorWorkArea;
        this._getDockReservedExtent = getDockReservedExtent;
        this._log = log;

        this._client = null;         // Meta.WaylandClient
        this._subprocess = null;     // its GSubprocess (for force_exit)
        this._window = null;         // the adopted Meta.Window
        this._restartState = initialState();
        this._dormant = false;

        // Signals are tracked with connectObject()/disconnectObject(): host-
        // lifetime signals (the map watch, the overview) use `this` as the
        // tracker owner and are dropped in disable(); window-scoped signals
        // (position/size/unmanaged) use a separate per-adoption token so
        // they can be dropped when a window unmanages at idle without
        // touching the host-lifetime ones.
        this._windowSignals = null;

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

        // Drop every tracked signal: the host-lifetime ones (map watch,
        // overview) keyed on `this`, and the window-scoped ones keyed on the
        // per-adoption token.
        global.window_manager.disconnectObject(this);
        Main.overview.disconnectObject(this);
        global.display.disconnectObject(this);
        global.backend.get_monitor_manager?.()?.disconnectObject(this);
        this._disconnectWindowSignals();

        // Cancel the current subprocess wait so its promise rejects as
        // cancelled rather than resolving into _onRendererExited after we
        // have torn down — no late respawn.
        this._cancellable?.cancel();
        this._cancellable = null;

        // Return the actor to the window group before we drop the window, so
        // it is never orphaned above the overview.
        this._raiseAboveOverview(false);

        // Kill the renderer process. Meta.WaylandClient has NO destroy() (it
        // only exposes get_subprocess/owns_window), so terminating the
        // subprocess is what stops the renderer and reaps its window; the
        // wait was cancelled above, so this exit does not schedule a
        // respawn. force_exit is a hard kill — the renderer is a HUD with no
        // unsaved state, and a graceful SIGTERM would only delay teardown.
        try {
            this._subprocess?.force_exit();
        } catch (e) {
            logError(e, '[myna-shell] error terminating renderer');
        }
        this._subprocess = null;
        this._client = null;
        this._window = null;
    }

    /** Whether the host has given up after exhausting the restart budget
     * (XH3). Exposed for the extension's presence/logging. */
    get dormant() {
        return this._dormant;
    }

    /** Recompute placement now (e.g. the dock's reserved extent changed). */
    positionNow() {
        this._position();
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
        global.window_manager.connectObject(
            'map',
            (_wm, actor) => this._onWindowMapped(actor.get_meta_window()),
            GObject.ConnectFlags.AFTER,
            this);

        // Watch the subprocess so an exit drives the respawn policy. A fresh
        // Cancellable per spawn, captured by this wait: disable() cancels
        // whatever is current, and _watchExit checks the very instance it was
        // handed — so a superseded wait never confuses the current one.
        const cancellable = new Gio.Cancellable();
        this._cancellable = cancellable;
        this._subprocess = client.get_subprocess();
        this._watchExit(this._subprocess, cancellable);
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
        // Already tracking this exact window — a spurious re-map of the one
        // we hold. Re-assert the overlay treatment (mutter can reset some of
        // it) and reposition, but do not re-run the one-time wiring.
        if (window === this._window) {
            this._makeOverlay(window);
            this._position();
            return;
        }
        // A window is already adopted and this is a different one: on Wayland
        // a GTK hide/show at idle destroys and recreates the surface, so the
        // remapped HUD is a NEW Meta.Window. We must only skip *additional*
        // windows of an already-live one — but the old one is unmanaged when
        // its surface is destroyed (see _adopt), which clears this._window,
        // so reaching here with this._window set means a genuine second
        // window (a dialog), not the re-mapped HUD.
        if (this._window)
            return;

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

        this._adopt(window);
    }

    _adopt(window) {
        this._window = window;
        this._log('adopted renderer window');
        this._makeOverlay(window);
        this._connectOverview();

        // Window-scoped signals, keyed on a fresh per-adoption token so they
        // can be dropped when this window unmanages at idle without touching
        // the host-lifetime signals. `unmanaged` clears our tracking so the
        // fresh window that maps on the next non-idle state is adopted rather
        // than rejected as a "second window" — the never-track-loss across
        // the idle unmap/remap. `size-changed`/`position-changed` recentre
        // the pill as its content changes (idle→active, wrapping errors).
        this._windowSignals = {};
        window.connectObject(
            'unmanaged', () => this._onWindowUnmanaged(window),
            'size-changed', () => this._position(),
            'position-changed', () => this._position(),
            this._windowSignals);

        // Layout changes that move the target, keyed on `this` (host
        // lifetime — they outlive any single adopted window).
        global.display.connectObject(
            'workareas-changed', () => this._position(), this);
        const monitorManager = global.backend.get_monitor_manager?.();
        monitorManager?.connectObject(
            'monitors-changed', () => this._position(), this);

        this._position();
    }

    _onWindowUnmanaged(window) {
        if (window !== this._window)
            return;
        this._disconnectOverview();
        this._disconnectWindowSignals();
        global.display.disconnectObject(this);
        global.backend.get_monitor_manager?.()?.disconnectObject(this);
        this._window = null;
        // The renderer is still running (this is an idle hide, not an exit);
        // the next non-idle state maps a fresh window that _onWindowMapped
        // adopts.
    }

    /** Drop the window-scoped signals (position/size/unmanaged). */
    _disconnectWindowSignals() {
        if (this._windowSignals) {
            this._window?.disconnectObject(this._windowSignals);
            this._windowSignals = null;
        }
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

    /** Keep the overlay visible when the overview opens.
     *
     * The overview hides ordinary compositor windows (ours included) behind
     * its own UI. A dictation indicator must persist there — you may open the
     * overview mid-session to find a window — so the window's actor is
     * reparented into `Main.layoutManager.uiGroup`, which draws above the
     * overview, for the duration the overview is showing, then returned to
     * the window group. This is the mechanism docks use for the same need.
     */
    _connectOverview() {
        Main.overview.connectObject(
            'showing', () => this._raiseAboveOverview(true),
            'hidden', () => this._raiseAboveOverview(false),
            this);
        // If enabled while the overview is already open, apply immediately.
        if (Main.overview.visible)
            this._raiseAboveOverview(true);
    }

    _raiseAboveOverview(above) {
        const actor = this._window?.get_compositor_private();
        if (!actor)
            return;
        try {
            const parent = actor.get_parent();
            const target = above ? Main.layoutManager.uiGroup : global.window_group;
            if (parent === target)
                return;
            parent?.remove_child(actor);
            target.add_child(actor);
            if (above)
                Main.layoutManager.uiGroup.set_child_above_sibling(actor, null);
        } catch (e) {
            logError(e, '[myna-shell] error raising overlay over the overview');
        }
    }

    _disconnectOverview() {
        // Make sure the actor is back in the window group before we stop
        // tracking, or it would be orphaned above the overview.
        this._raiseAboveOverview(false);
        Main.overview.disconnectObject(this);
    }

    // ── Positioning (XH1) ───────────────────────────────────────────────

    _position() {
        if (!this._window || this._positioning)
            return;
        let workArea = this._getMonitorWorkArea();
        if (!workArea)
            return;

        // If an overlay dock (no strut, so not reflected in the work area)
        // reserves the bottom of this monitor, raise the placement above it:
        // a pill sitting at the work area's bottom edge would be covered the
        // moment the dock slides out.
        workArea = shrinkWorkAreaForDock(
            workArea, this._getDockReservedExtent(), St.Side.BOTTOM);

        const frame = this._window.get_frame_rect();
        const target = computePlacement(
            workArea, {width: frame.width, height: frame.height});
        const current = {x: frame.x, y: frame.y};
        if (!placementChanged(current, target))
            return;

        // Anti-feedback: our own move fires size/position signals, which
        // would re-enter _position(). A guard flag is enough now that the
        // handlers are owner-tracked (previously we disconnected/reconnected
        // them around the move).
        this._positioning = true;
        try {
            this._window.move_frame(false, target.x, target.y);
        } catch (e) {
            logError(e, '[myna-shell] error positioning overlay window');
        } finally {
            this._positioning = false;
        }
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
        // Drop all tracked signals for this dead renderer. _spawn()
        // reconnects the map watch for the respawn, so it is dropped here too
        // rather than left dangling on the exited process's would-be windows.
        this._disconnectOverview();
        this._disconnectWindowSignals();
        global.display.disconnectObject(this);
        global.backend.get_monitor_manager?.()?.disconnectObject(this);
        global.window_manager.disconnectObject(this);
        this._window = null;
        this._client = null;
        this._subprocess = null;
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
