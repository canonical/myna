# Contract: `AccessibilityAnnouncer` seam + AT-SPI wire behaviour

## Rust seam (`myna-desktop/src/accessibility/mod.rs`)

```rust
/// A content-free string safe to announce. Constructible only from a
/// `&'static str` literal — a runtime transcript is always an owned `String`,
/// so it cannot reach `announce()`/`set_state()` at all (compile-time
/// enforcement of A1, not a runtime check).
pub struct AnnouncementText(&'static str);

#[async_trait]
pub trait AccessibilityAnnouncer: Send {
    /// Emit a content-free announcement. MUST NOT block the caller beyond a
    /// bounded internal timeout; a failure here MUST NOT propagate as a
    /// session error (FR-002a).
    async fn announce(&mut self, text: AnnouncementText, severity: Option<Severity>) -> Result<(), AnnounceError>;

    /// Update the on-demand-queryable accessible name/description/state
    /// (FR-001) without necessarily emitting a proactive announcement.
    async fn set_state(&mut self, name: AnnouncementText, description: AnnouncementText);
}
```

## Guarantees (row per FR, each an executable test before implementation)

| ID | Guarantee | Test tier |
|---|---|---|
| A1 | `announce()`/`set_state()` can only ever be called with an `AnnouncementText`, which is constructible only from a `&'static str` literal — a runtime transcript (always an owned `String`) cannot be passed, by construction (FR-003). | compile-time (type system) + hermetic round-trip test |
| A2 | A verbosity of `Off` suppresses all `announce()` calls but `set_state()` still updates the queryable name/description (FR-004). | hermetic |
| A3 | A verbosity of `FailuresOnly` suppresses non-`Critical`/`Recoverable` announcements. | hermetic |
| A4 | A burst of transitions within the coalescing window produces at most one delivered announcement, and a superseded announcement is never delivered after its state has passed (FR-005, SC-003). | hermetic (fake announcer + simulated clock) |
| A5 | If the underlying bus call fails, `announce()` returns `Err` but the caller's session flow continues unaffected, and the error is surfaced as a `Recoverable` failure through `FailurePresentation` (FR-002a). | hermetic (fake announcer forced to fail) |
| A6 | The real `atspi`-backed implementation emits the AT-SPI `Announcement` event (`org.a11y.atspi.Event.Object`) on the accessibility bus and registers an accessible object whose name/description are queryable at any time, not only at the instant of a transition (FR-001, FR-002). | integration, `MYNA_ATSPI_TESTS=1` |
| A7 | Registering the accessible object and connecting to `org.a11y.Bus` costs nothing measurable when no AT is listening (FR-007). | watermark (Rust, hermetic) |
| A8 | `GtkIndicator`'s `gtk_accessible_announce()` calls carry the same text the `atspi`-backed announcer would have sent for the same state (FR-006 "identically"). | hermetic (shared fixture comparing both call sites' formatted text) |

## GJS side (`extensions/myna-shell/a11y.js`)

```js
// Pure, testable: given a state id + coverage-matrix entry, produce the
// announcement text/politeness. Never touches Gio/D-Bus itself.
export function formatAnnouncement(stateId, severity) { /* ... */ }

// Impure: opens org.a11y.Bus (Gio.DBusConnection) once, emits Announcement.
export class Announcer { /* enable()/disable()/announce(text, politeness) */ }
```

| ID | Guarantee | Test tier |
|---|---|---|
| G1 | `formatAnnouncement` returns identical text to the Rust side for the same state id (cross-checked against a shared fixture derived from `coverage-matrix.json`). | hermetic (GJS contract test) |
| G2 | `Announcer.announce()` coalesces bursts the same way as the Rust side (A4) — same coalescing window constant, defined once and referenced by both (`contracts/coverage-matrix.md`). | hermetic (GJS contract test, fake clock) |
| G3 | `Announcer` releases its `org.a11y.Bus` connection on `disable()` and re-inits cleanly across Shell restart (mirrors feature 004's `dbus.js` lifecycle contract). | manual acceptance (quickstart.md) — no nested-compositor headless path (research.md R6) |
| G4 | The real bus emission reaches Orca/braille in a live GNOME session. | manual acceptance only (R6) |
