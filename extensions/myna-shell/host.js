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
import Shell from 'gi://Shell';

import {computePlacement, placementChanged} from './place.js';
import {initialState, planRestart} from './respawn.js';
import {resolveHudLaunch} from './resolve.js';

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

        this._windowCreatedId = 0;
        this._positionHandlerIds = [];   // on the adopted window
        this._layoutHandlerIds = [];     // monitors/work-area
        this._restartTimeoutId = 0;
        this._launchedAtMs = 0;
        this._enabled = false;
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

        if (this._windowCreatedId) {
            global.display.disconnect(this._windowCreatedId);
            this._windowCreatedId = 0;
        }
        if (this._client) {
            // The pending wait_async callback still fires after this, but it
            // routes through _onRendererExited, which no-ops once _enabled is
            // false (set above) — so terminating here does not schedule a
            // respawn. Killing the client reaps its subprocess and the
            // adopted window with it.
            try {
                this._client.destroy();
            } catch (e) {
                this._log(`error tearing down client: ${e}`);
            }
            this._client = null;
        }
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
            this._log(`failed to launch renderer (${launch.source}): ${e}`);
            this._scheduleRestart(/* expected= */ false, /* uptimeMs= */ 0);
            return;
        }

        this._client = client;
        this._launchedAtMs = GLib.get_monotonic_time() / 1000;
        this._log(`launched renderer via ${launch.source}: ${launch.argv.join(' ')}`);

        // Adopt the window when it appears.
        this._windowCreatedId = global.display.connect(
            'window-created', (_display, window) => this._onWindowCreated(window));

        // Watch the subprocess so an exit drives the respawn policy.
        const subprocess = client.get_subprocess();
        if (subprocess) {
            subprocess.wait_async(null, (proc, result) => {
                try {
                    proc.wait_finish(result);
                } catch (_e) {
                    // ignore — we only care that it exited
                }
                this._onRendererExited();
            });
        }
    }

    // ── Adoption (XH4) ──────────────────────────────────────────────────

    _onWindowCreated(window) {
        // Ignore windows we do not own, and adopt exactly once — a second
        // window from the same client (a dialog) is not the HUD.
        if (this._window)
            return;
        if (!this._client || !this._client.owns_window(window))
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
            this._log(`error configuring overlay: ${e}`);
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
            this._log(`error positioning overlay: ${e}`);
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

    _onRendererExited() {
        if (!this._enabled)
            return;   // disable() terminated it; not an incident
        const uptimeMs = GLib.get_monotonic_time() / 1000 - this._launchedAtMs;
        this._disconnectWindow();
        this._disconnectLayout();
        if (this._windowCreatedId) {
            global.display.disconnect(this._windowCreatedId);
            this._windowCreatedId = 0;
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
