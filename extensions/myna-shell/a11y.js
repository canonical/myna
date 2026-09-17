// a11y.js — content-free AT-SPI accessibility announcements (feature
// 011-accessible-dictation-ux, contracts/announcer.md, research.md R1).
//
// Two parts, deliberately separated so the mapping logic is testable without
// a Shell or a D-Bus connection (mirrors states.js's pure/impure split):
// - formatAnnouncement(stateId, severity): PURE — given a coverage-matrix
//   state id and severity, returns the {text, politeness} to announce. No
//   Gio/GLib import here.
// - Announcer: IMPURE — opens org.a11y.Bus via Gio.DBusConnection and emits
//   the AT-SPI "Announcement" event (org.a11y.atspi.Event.Object), the same
//   underlying primitive GTK 4.14's gtk_accessible_announce() uses.
//
// Unit-tested by test/a11y.test.js (formatAnnouncement only — no Shell
// needed). Announcer's live bus behaviour is exercised by the manual
// acceptance protocol (quickstart.md), per research.md R6.
//
// Wording is kept identical to the Rust side's
// `accessibility::format_state_announcement` (FR-024a) for every state both
// sides cover (recording/transcribing/finalizing/notice/error → Listening/
// Transcribing/Finishing/Notice/Error). "loading" has no Rust-side
// equivalent today — myna-desktop's IndicatorState never distinguishes the
// cold-load phase from Recording, only the D-Bus wire encoding this Shell
// reads does (see controller.rs's event_to_indicator doc comment) — so its
// text ("Loading model…") is taken from states.js's existing visual label
// instead, an asymmetry rather than a disagreement (Rust simply never
// announces that specific text today).

import Gio from 'gi://Gio';
import GLib from 'gi://GLib';

const ANNOUNCEMENT_TEXT = {
    idle: 'Idle',
    loading: 'Loading model…',
    recording: 'Listening',
    transcribing: 'Transcribing',
    finalizing: 'Finishing',
    notice: 'Notice',
    error: 'Error',
};

/**
 * @param {string} stateId - a coverage-matrix.json state id.
 * @param {?string} severity - 'recoverable' | 'critical' | null.
 * @returns {{text: string, politeness: string}}
 */
export function formatAnnouncement(stateId, severity) {
    const text = ANNOUNCEMENT_TEXT[stateId] ?? 'Active';
    const politeness = severity === 'critical' ? 'assertive' : 'polite';
    return {text, politeness};
}

export class Announcer {
    /**
     * @param {object} [opts]
     * @param {number} [opts.coalesceMs] - the coalescing window (contract
     *     announcer.md A4/G2): a burst of `announce()` calls within this
     *     window delivers only the last one. Matches the Rust side's
     *     `CoalescingAnnouncer` treatment conceptually (same "latest wins,
     *     earlier ones dropped" rule); the exact window value is tuned
     *     independently per surface, not a shared numeric constant, since
     *     the two run in different processes with no shared clock to pin it
     *     to.
     * @param {function} [opts._getBusAddress] - test seam: `() => string`
     *     resolving `org.a11y.Bus`'s address (default: a real
     *     `org.a11y.Bus.GetAddress()` call on the session bus).
     * @param {function} [opts._openConnection] - test seam:
     *     `(address) => Gio.DBusConnection` (default:
     *     `Gio.DBusConnection.new_for_address_sync`).
     * @param {function} [opts._scheduleFlush] - test seam:
     *     `(ms, callback) => id` (default: `GLib.timeout_add`). Injecting a
     *     fake here — one that records `callback` without a real timer and
     *     lets a test invoke it manually — makes the coalescing window
     *     deterministic to test (contract G2), the same role tokio's
     *     paused/advanced virtual time plays on the Rust side.
     * @param {function} [opts._cancelScheduled] - test seam: `(id) => void`
     *     (default: `GLib.source_remove`).
     */
    constructor({
        coalesceMs = 300,
        _getBusAddress = null,
        _openConnection = null,
        _scheduleFlush = null,
        _cancelScheduled = null,
    } = {}) {
        this._coalesceMs = coalesceMs;
        this._getBusAddress = _getBusAddress ?? Announcer._defaultGetBusAddress;
        this._openConnection = _openConnection ?? Announcer._defaultOpenConnection;
        this._scheduleFlush = _scheduleFlush ??
            ((ms, callback) => GLib.timeout_add(GLib.PRIORITY_DEFAULT, ms, () => {
                callback();
                return GLib.SOURCE_REMOVE;
            }));
        this._cancelScheduled = _cancelScheduled ?? GLib.source_remove;
        this._connection = null;
        this._pendingTimeoutId = 0;
        this._pending = null;
    }

    static _defaultGetBusAddress() {
        const sessionBus = Gio.DBus.session;
        const result = sessionBus.call_sync(
            'org.a11y.Bus', '/org/a11y/bus', 'org.a11y.Bus', 'GetAddress',
            null, new GLib.VariantType('(s)'), Gio.DBusCallFlags.NONE, -1, null);
        return result.deep_unpack()[0];
    }

    static _defaultOpenConnection(address) {
        return Gio.DBusConnection.new_for_address_sync(
            address,
            Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT,
            null, null);
    }

    /** Connects to `org.a11y.Bus`. Idempotent.
     *
     * Uses sync Gio calls (`GetAddress` + `new_for_address_sync`), same as
     * this file's `dbus.js` sibling explicitly avoids for `org.myna.Dictation`
     * — but here the calls are to `org.a11y.Bus`, a well-known service every
     * accessible toolkit (GTK included) already bootstraps synchronously at
     * app startup, and is not expected to be slow or absent in a normal
     * session; if it IS unreachable (e.g. a headless test environment, or an
     * AppArmor/dbus-broker policy denying the call — see
     * `client/myna-desktop/src/accessibility/atspi.rs`'s module doc comment
     * for a concrete example of the latter), this MUST NOT crash the
     * extension or block dictation (FR-002a/FR-007): caught here, leaving
     * `announce()` a silent no-op rather than propagating. */
    enable() {
        if (this._connection)
            return;
        try {
            const address = this._getBusAddress();
            this._connection = this._openConnection(address);
        } catch (e) {
            logError(e, 'myna-shell: could not connect to org.a11y.Bus; announcements disabled');
            this._connection = null;
        }
    }

    /** Releases the bus connection and any pending coalesced announcement
     * (feature 004's `dbus.js` lifecycle contract, G3). Idempotent. */
    disable() {
        if (this._pendingTimeoutId) {
            this._cancelScheduled(this._pendingTimeoutId);
            this._pendingTimeoutId = 0;
        }
        this._pending = null;
        if (this._connection) {
            this._connection.close_sync(null);
            this._connection = null;
        }
    }

    /**
     * Coalesced announce (contract A4/G2): resets the window on every call,
     * replacing whatever was pending — only the last call in an unbroken
     * burst is ever actually emitted.
     *
     * @param {string} text - content-free announcement text.
     * @param {string} politeness - 'polite' | 'assertive'.
     */
    announce(text, politeness) {
        if (this._pendingTimeoutId)
            this._cancelScheduled(this._pendingTimeoutId);
        this._pending = {text, politeness};
        this._pendingTimeoutId = this._scheduleFlush(this._coalesceMs, () => {
            this._pendingTimeoutId = 0;
            const pending = this._pending;
            this._pending = null;
            if (pending && this._connection)
                this._emit(pending.text, pending.politeness);
        });
    }

    // The AT-SPI2 event-body shape shared by every org.a11y.atspi.Event.*
    // interface: (minor-type: unused, detail1: unused, detail2: the
    // politeness value, any_data: the announced text, properties: unused).
    // The same shape the Rust atspi crate's AnnouncementEvent serializes to
    // (atspi_common::events::event_body::EventBodyOwned) — kept in sync by
    // hand since GJS has no shared crate to derive it from.
    _emit(text, politeness) {
        const politenessValue = politeness === 'assertive' ? 2 : 1;
        const body = new GLib.Variant('(siiva{sv})', [
            '', 0, politenessValue, GLib.Variant.new_string(text), {},
        ]);
        this._connection.emit_signal(
            null, '/org/myna/dictation', 'org.a11y.atspi.Event.Object',
            'Announcement', body);
    }
}
