//! policy — the launcher policy for the indicator surface (feature 004,
//! T151; contract publisher.md P20–P23).
//!
//! `myna-desktop` always serves `com.canonical.Myna.Dictation` (the myna-hud
//! client consumes it). The policy decides the FALLBACK surface:
//!
//! * while at least one `myna-hud` client is registered via `RegisterClient`
//!   (and pruned via `NameOwnerChanged`), the `NotifyIndicator` fallback is
//!   **suppressed** — there is already a hosted HUD, a second notification
//!   would be a duplicate (P20/C14);
//! * when the last client leaves (`UnregisterClient` or vanished), the
//!   fallback is restored so dictation stays observable (P21/C15);
//! * client-set watching never blocks or fails dictation — a bus error
//!   degrades to the fallback, never an abort (P22);
//! * the non-GNOME spawn seam (launch `myna-hud` standalone where a
//!   focus-safe overlay backend exists) is **contract only** — the hook
//!   exists, no backend ships this pass (P23).
//!
//! Fallback suppression now uses the `RegisterClient` client set. This module
//! is retained for the `Policy` seam and tests; real fallback now lives in
//! `indicator::dynamic::DynamicIndicator` + `dbus::serve::ClientRegistry`.

/// What to do with the indicator surface given the shell-host presence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SurfaceDecision {
    /// Whether to use the `NotifyIndicator` fallback (true when no shell
    /// host is present to host the overlay HUD).
    pub uses_notify_fallback: bool,
    /// The spawn action for a non-GNOME backend — contract-only this pass.
    pub spawn_hud_action: &'static str,
}

impl SurfaceDecision {
    /// P20/P21/P22: the notification fallback is used exactly when the shell
    /// host is NOT present; a missing/errored probe reads as "absent" (P22),
    /// so dictation always has a surface and never aborts.
    pub fn for_shell_presence(shell_present: bool) -> Self {
        Self {
            uses_notify_fallback: !shell_present,
            // P23: no backend ships this pass; the seam returns "none".
            spawn_hud_action: "none",
        }
    }
}

/// Presence seam — now DEPRECATED. Real fallback uses
/// `ClientRegistry::has_clients()` (`indicator::dynamic`). This trait is kept
/// for `tests/policy.rs` hermetic coverage of the old `SurfaceDecision` pure
/// logic.
pub trait Policy: Send {
    fn shell_present(&self) -> bool;

    /// Begin watching for presence changes; `on_change` fires immediately
    /// with the current state and thereafter on owner changes.
    fn watch(&self, _on_change: Box<dyn Fn(bool) + Send>) {}

    /// Decide the indicator surface from the current presence.
    fn decide_surface(&self) -> SurfaceDecision {
        SurfaceDecision::for_shell_presence(self.shell_present())
    }

    /// The non-GNOME spawn hook — contract only (P23).
    fn spawn_hud_action(&self) -> &'static str {
        "none"
    }
}

/// Deprecated: use `ClientRegistry` / `DynamicIndicator`. Kept for tests.
pub async fn probe_shell_presence() -> bool {
    false
}
