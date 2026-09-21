// mutterCompat.js — the Mutter API transitions the overlay host needs across
// GNOME Shell 46–51. No `gi://` imports: host.js supplies the live APIs,
// while test/mutterCompat.test.js supplies plain stubs.
//
// Mutter 14–16 (Shell 46–48):
//   Meta.WaylandClient.new(context, launcher) → client.spawnv(display, argv)
//   client.make_dock(window), client.hide_from_window_list(window)
//
// Mutter 17+ (Shell 49+):
//   Meta.WaylandClient.new_subprocess(context, launcher, argv)
//   client.get_subprocess(), window.set_type(DOCK), window.hide_from_window_list()
//
// Cursor tracker — Mutter 14–15 (Shell 46–47):
//   Meta.CursorTracker.get_for_display(display)
// Mutter 16+ (Shell 48+):
//   Meta.Backend.get_cursor_tracker() (the per-display accessor was removed)

/** Create the trusted Wayland client and launch its subprocess.
 *
 * `new_subprocess` combines the old construction and launch calls. Its
 * optional-call fallback retains the Mutter 14–16 API without a version
 * table; both paths create the private Wayland socket that makes
 * `client.owns_window(window)` reliable.
 */
export function launchTrustedClient({WaylandClient, context, display, launcher, argv}) {
    const client = WaylandClient.new_subprocess?.(context, launcher, argv) ??
        WaylandClient.new(context, launcher);
    const subprocess = client.get_subprocess?.() ?? client.spawnv(display, argv);
    return {client, subprocess};
}

/** Configure a trusted renderer window as a focus-safe overlay.
 *
 * The moved methods return void, so optional-call `??` would call both APIs;
 * select the window-side generation only when `set_type` exists.
 */
export function configureTrustedWindow({client, window, dockType}) {
    if (window.set_type) {
        window.set_type(dockType);
        window.hide_from_window_list();
    } else {
        client.make_dock(window);
        client.hide_from_window_list(window);
    }
}

/** The display's Meta.CursorTracker, whose `position-invalidated` signal the
 * hover fade watches.
 *
 * Mutter 16+ (Shell 48+) reaches it through the backend; Mutter 14–15 (Shell
 * 46–47) only offers the per-display accessor. Both name the same object.
 */
export function getCursorTracker({backend, display, CursorTracker}) {
    return backend.get_cursor_tracker?.() ?? CursorTracker.get_for_display(display);
}
