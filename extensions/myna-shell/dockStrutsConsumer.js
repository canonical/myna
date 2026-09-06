// dockStrutsConsumer.js — consumes Main.layoutManager.dashToDockStruts (feature 004,
// 2026-08-26; research R2x — dash-to-dock reserved-extent export).
//
// dash-to-dock publishes the space its dock reserves on each monitor as
// `Main.layoutManager.dashToDockStruts`: a destroyable GObject with a
// per-monitor `updated` signal (monitor index, or -1 for all) and a
// `monitors` map of `{ side, x, y, width, height }`. This is the myna-shell
// consumer: it follows that object (set / updated / destroyed) and hands the
// whole per-monitor map to the extension, so the pill is never placed where
// an auto-hide bottom dock would slide out, on whichever monitor it was
// targeted at, which is not necessarily the primary one.
//
// The object lives on `Main.layoutManager` (a mutable GObject) because the
// `Main` module namespace is frozen and cannot carry new properties. The
// consumer follows it directly — no probing dash-to-dock's internals, no
// per-UUID capability handshake. When the object is absent, placement falls
// back to the plain work area.

import * as Main from 'resource:///org/gnome/shell/ui/main.js';

/** The dash-to-dock UUIDs across the two distributions we care about. */
export const DASH_TO_DOCK_UUIDS = [
    'dash-to-dock@micxgx.gmail.com',
    'ubuntu-dock@ubuntu.com',
];

/**
 * Watch `Main.layoutManager.dashToDockStruts` and call `onChange(monitors)`
 * with the reserved extent for every monitor, keyed by monitor index
 * (`null` when dash-to-dock is absent). The caller indexes it with whatever
 * monitor the pill is actually on: the dock can be on a monitor the pill is
 * not, and vice versa.
 *
 * @param {object} owner - a GObject (e.g. the extension instance) used as
 *     the connectObject owner, so the destroyable struts object
 *     auto-disconnects our `updated` handler when it is destroyed.
 * @param {function(object|null): void} onChange
 * @returns {{disconnect: function(): void}} a handle to stop watching.
 */
export function watchDashToDockStruts(owner, onChange) {
    const sync = () => {
        const { dashToDockStruts } = Main.layoutManager;
        onChange(dashToDockStruts?.monitors ?? null);
    };

    // Follow the object directly: connect to its `updated` when present.
    // Because it is a destroyable GObject type, its `destroy` auto-cleans
    // our handler — no manual disconnect needed when dash-to-dock exits.
    let { dashToDockStruts: struts } = Main.layoutManager;
    struts?.connectObject('updated', sync, owner);

    // Watch extension state so we (re)connect if dash-to-dock is enabled or
    // disabled while we are watching — the object is set on enable, nulled
    // on disable, so a state change means we must re-sync and re-connect to
    // the (possibly new) object.
    const stateChangedId = Main.extensionManager.connect(
        'extension-state-changed', (_em, extension) => {
            if (!DASH_TO_DOCK_UUIDS.includes(extension.uuid))
                return;

            // Re-connect to whatever object is current now.
            const { dashToDockStruts } = Main.layoutManager;
            struts?.disconnectObject(owner);
            dashToDockStruts?.connectObject('updated', sync, owner);
            struts = dashToDockStruts;
            sync();
        });

    sync();

    return {
        disconnect() {
            Main.layoutManager.dashToDockStruts?.disconnectObject(owner);
            Main.extensionManager.disconnect(stateChangedId);
        },
    };
}
