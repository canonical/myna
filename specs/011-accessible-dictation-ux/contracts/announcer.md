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

`A8` guaranteed that `GtkIndicator`'s `gtk_accessible_announce()` calls carried
the same text the `atspi`-backed announcer would send for the same state. It is
withdrawn: the opt-in `ui-gtk` overlay `GtkIndicator` belonged to was removed
(project-plan T150), so there is no second Rust call site to hold in step.

## GJS side — withdrawn

This contract previously specified `extensions/myna-shell/a11y.js`: a pure
`formatAnnouncement(stateId, severity)` plus an impure `Announcer` class that
opened `org.a11y.Bus` over `Gio.DBusConnection`, with guarantees G1–G4 (text
parity with the Rust side, matching coalescing, connection lifecycle across
Shell restart, and live delivery to Orca/braille).

That file no longer exists and the guarantees are withdrawn rather than unmet.
The GNOME Shell extension has no shipping vehicle, so an announcement emitted
only when a separately-installed extension happens to be present cannot carry a
MUST; and two emitters meant the verbosity preference had two places to reach,
which is a defect surface with no user-visible benefit. Announcements now leave
from exactly one place — the Rust `atspi`-backed announcer specified above —
so G1's text-parity and G2's coalescing-parity obligations are discharged by
construction rather than by cross-language fixture.

The Shell extension keeps its visual role (hosting the `myna-hud` renderer);
it carries no accessibility guarantee.
