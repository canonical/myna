//! policy — the launcher policy for the indicator surface (feature 004,
//! T151; contract publisher.md P20–P23).
//!
//! `myna-desktop` always serves `org.myna.Dictation` (the myna-shell
//! extension consumes it). The policy decides the FALLBACK surface:
//!
//! * while `org.myna.Shell` has an owner (the extension host is up and
//!   hosting the `myna-hud` overlay), the `NotifyIndicator` fallback is
//!   **suppressed** — there is already a hosted HUD, a second notification
//!   would be a duplicate (P20);
//! * when it vanishes (extension disabled/removed/Shell crash), the fallback
//!   is restored so dictation stays observable (P21);
//! * presence watching never blocks or fails dictation — a bus error
//!   degrades to the fallback, never an abort (P22);
//! * the non-GNOME spawn seam (launch `myna-hud` standalone where a
//!   focus-safe overlay backend exists) is **contract only** — the hook
//!   exists, no backend ships this pass (P23).
//!
//! The [`Policy`] trait is the seam: the real implementation watches the
//! session bus for `org.myna.Shell`, and tests inject a fake presence. The
//! decision is a pure function of "is the shell host present", so it is
//! trivially hermetic.

/// The presence name the extension host owns while enabled (contract
/// dbus-interface.md C12/C13).
pub const PRESENCE_NAME: &str = "org.myna.Shell";

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

/// The presence seam: report the shell host's presence and watch for
/// changes. The real implementation queries the session bus; tests use a
/// fake.
pub trait Policy: Send {
    /// Whether `org.myna.Shell` currently has an owner. An unreachable bus
    /// returns `false` (P22 — degrade to the fallback, never abort).
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

/// Probe the session bus for the shell host's presence.
///
/// Uses the crate's stale-guid-tolerant session connect
/// ([`crate::dbus::serve::connect_session`]); an unreachable bus returns
/// `false` (P22 — degrade to the fallback, never abort).
pub async fn probe_shell_presence() -> bool {
    use zbus::names::BusName;

    let Ok(connection) = crate::dbus::serve::connect_session().await else {
        return false;
    };
    let Ok(db) = zbus::fdo::DBusProxy::new(&connection).await else {
        return false;
    };
    let Ok(name) = BusName::try_from(PRESENCE_NAME) else {
        return false;
    };
    db.name_has_owner(name).await.unwrap_or(false)
}
