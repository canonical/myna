// presence.js — the extension's D-Bus presence name (feature
// 004-gnome-shell-indicator, 2026-08-26 architecture revision; research R24;
// contract dbus-interface.md §Presence C12, extension.md XH5).
//
// `org.myna.Shell` has NO members: ownership itself is the whole contract.
// `myna-desktop` watches the name to decide which indicator surface is
// live — present means the hosted renderer is the indicator and the
// client's notification fallback stays quiet; absent means the fallback
// applies (C13).
//
// Owning it is advisory, never load-bearing: if the bus is unavailable the
// host keeps hosting (the pill still works), it simply cannot advertise
// itself. `Gio` is imported lazily through an injectable seam so the
// lifecycle is testable headlessly (test/presence.test.js).

/** The well-known name. Consumers only ever watch it. */
export const PRESENCE_NAME = 'org.myna.Shell';

/**
 * Owns [PRESENCE_NAME] for exactly as long as it is enabled.
 */
export class ShellPresence {
    /**
     * @param {object} [deps]
     * @param {function(string, function(): void, function(): void): number} [deps.ownName]
     *     test seam: (name, onAcquired, onLost) → an owner id.
     * @param {function(number): void} [deps.unownName] test seam.
     * @param {function(string): void} [deps.log] test seam (informational logger).
     */
    constructor({ownName = null, unownName = null, log = null} = {}) {
        this._ownName = ownName;
        this._unownName = unownName;
        this._log = log ?? (message => console.log(message));
        this._ownerId = 0;
        this._owned = false;
    }

    /** Whether the name is currently held (the signal consumers watch). */
    get owned() {
        return this._owned;
    }

    /** Request the name. Safe to call twice; never throws (XH5). */
    enable() {
        if (this._ownerId !== 0)
            return;
        if (this._ownName === null) {
            this._log(
                `myna-shell: no bus available; ${PRESENCE_NAME} not advertised`);
            return;
        }
        try {
            this._ownerId = this._ownName(
                PRESENCE_NAME,
                () => {
                    this._owned = true;
                },
                () => {
                    this._owned = false;
                });
        } catch (e) {
            // Presence is advisory: hosting continues without it.
            this._ownerId = 0;
            this._owned = false;
            this._log(`myna-shell: could not own ${PRESENCE_NAME}: ${e}`);
        }
    }

    /** Release the name. Safe when dormant. */
    disable() {
        if (this._ownerId === 0) {
            this._owned = false;
            return;
        }
        if (this._unownName !== null)
            this._unownName(this._ownerId);
        this._ownerId = 0;
        this._owned = false;
    }
}
