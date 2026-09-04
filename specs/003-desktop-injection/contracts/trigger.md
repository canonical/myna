# Contract: GlobalShortcut activation (`Trigger` via the portal)

**Feature**: 003-desktop-injection · **Crate**: `client/myna-desktop`

`GlobalShortcutTrigger` implements the **existing, unchanged** `Trigger` trait
(`myna-orchestrator::trigger`) over `org.freedesktop.portal.GlobalShortcuts`.
`ScriptedTrigger` (orchestrator) is the test fixture. Restates the guarantees for
TDD.

## Interface (reused trait; new implementor)

```rust
// existing:
pub enum TriggerEdge { Press, Release }
pub trait Trigger: Send { async fn next_edge(&mut self) -> Option<TriggerEdge>; }

// new implementor:
impl GlobalShortcutTrigger {
    pub async fn bind(shortcut_id: &str, preferred_trigger: Option<&str>)
        -> Result<Self, TriggerError>;
}
impl Trigger for GlobalShortcutTrigger { /* Activated→Press, Deactivated→Release */ }
pub enum TriggerError { PortalUnavailable(String), BindRejected(String) }
```

Portal signatures (verified in `org.freedesktop.portal.GlobalShortcuts.xml`):
`Activated`/`Deactivated` carry `(session_handle: o, shortcut_id: s,
timestamp: t, options: a{sv})`.

## Guarantees (each row → at least one test)

| # | Given | When | Then | Spec |
|---|-------|------|------|------|
| T1 | the shortcut is registered + bound | the user presses & holds it | one `TriggerEdge::Press` is yielded (session starts) | FR-006, FR-007, US2-1 |
| T2 | a session active from a held shortcut | the user releases it | one `TriggerEdge::Release` is yielded (graceful stop) | FR-007, US2-2 |
| T3 | the key is held and the compositor autorepeats `Activated` | repeats arrive before `Deactivated` | only the first yields `Press`; repeats are ignored until `Release` | FR-008, SC-003, US2-3 |
| T4 | the shortcut is unbound / portal session ends | — | `next_edge()` returns `None` (trigger ends cleanly) | FR-010 |
| T5 | the portal is unavailable | `bind()` | `Err(PortalUnavailable(..))` (clear failure) | FR-023 |
| T6 | a `preferred_trigger` is supplied | `bind()` | it is offered to the portal's own confirm/rebind UI; the app ships no shortcut-config UI | FR-006, FR-009 |
| T7 | any binding | trigger runs | no global key grab outside the portal; no synthesized input | FR-015 (safety), R2 |

## Test homes

- **Hermetic**: T1–T4 (edge mapping + autorepeat dedup) are covered by driving the
  dedup/mapping logic with a scripted portal-signal source (`ScriptedTrigger` and
  a fake signal stream). No D-Bus/portal. The dedup state machine (first-Activated-
  wins-until-Deactivated) is a pure unit test.
- **Integration (env-gated `MYNA_PORTAL_TESTS=1`, real portal)**: T1, T2, T5, T6
  against a live `xdg-desktop-portal` with a GlobalShortcuts backend (or the
  portal test template under `~/probe/ubuntu/xdg-desktop-portal-gu/tests`), binding
  a test shortcut and asserting `Activated`/`Deactivated` become `Press`/`Release`.
  `tests/portal_hw.rs`.

## Non-goals

- No in-app shortcut-configuration UI (the desktop's portal UI owns rebinding).
- No fallback to X11/compositor key grabs (impossible on Wayland; R2).
