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

/**
 * @param {string} stateId - a coverage-matrix.json state id.
 * @param {?string} severity - 'recoverable' | 'critical' | null.
 * @returns {{text: string, politeness: string}}
 */
export function formatAnnouncement(stateId, severity) {
    throw new Error('formatAnnouncement: not yet implemented (T039)');
}

export class Announcer {
    // enable()/disable()/announce(text, politeness) — see contracts/announcer.md G2/G3.
}
