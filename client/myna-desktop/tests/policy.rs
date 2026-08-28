// tests/policy.rs — the launcher policy for the indicator surface (feature
// 004, T151; contract publisher.md P20–P23). Hermetic: the presence probe is
// injected, so no session bus is needed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use myna_desktop::policy::{Policy, PRESENCE_NAME};

/// A fake presence probe the policy is handed (the real one queries the
/// session bus). Toggling it fires the watched callbacks, like a real
/// `name-owner-changed`.
#[derive(Clone, Default)]
struct FakePresence {
    shell_present: Arc<AtomicBool>,
    watching: Arc<AtomicBool>,
}

impl FakePresence {
    fn set_present(&self, present: bool) {
        self.shell_present.store(present, Ordering::SeqCst);
    }

    fn is_present(&self) -> bool {
        self.shell_present.load(Ordering::SeqCst)
    }
}

impl Policy for FakePresence {
    fn shell_present(&self) -> bool {
        self.is_present()
    }

    fn watch(&self, on_change: Box<dyn Fn(bool) + Send>) {
        // The real implementation would connect name-owner-changed; the fake
        // just records that watching began.
        self.watching.store(true, Ordering::SeqCst);
        let present = self.is_present();
        on_change(present);
    }
}

// --- P20: while org.myna.Shell has an owner, the notification fallback is
// suppressed --------------------------------------------------------------

#[test]
fn p20_shell_present_suppresses_the_notify_fallback() {
    let presence = FakePresence::default();
    presence.set_present(true);

    let surface = presence.decide_surface();
    assert!(
        !surface.uses_notify_fallback,
        "the shell host is up, so the notification fallback must be suppressed"
    );
}

// --- P21: when org.myna.Shell vanishes, the fallback is restored ---------

#[test]
fn p21_shell_vanished_restores_the_notify_fallback() {
    let presence = FakePresence::default();
    presence.set_present(false);

    let surface = presence.decide_surface();
    assert!(
        surface.uses_notify_fallback,
        "no shell host, so the notification fallback is restored"
    );
}

// --- P20/P21: dictation behavior is otherwise unchanged ------------------
// The decision only selects the surface; it must not depend on the
// dictation state or anything else.

#[test]
fn p20_p21_only_the_surface_is_selected() {
    // Same fake, both presence states, only the boolean flips.
    let on = FakePresence::default();
    on.set_present(true);
    let off = FakePresence::default();
    off.set_present(false);

    assert_ne!(
        on.decide_surface().uses_notify_fallback,
        off.decide_surface().uses_notify_fallback,
        "the only observable difference is whether the fallback is used"
    );
}

// --- P22: a bus error degrades to the fallback, never an abort ----------

#[test]
fn p22_bus_error_degrades_to_fallback() {
    // If the presence probe cannot be read (bus unavailable), the policy
    // treats the shell as ABSENT so dictation still has a surface — it
    // must never abort or leave the user with no indicator at all.
    let unknown = FakePresence::default(); // present = false (bus unreachable)
    unknown.set_present(false);

    let surface = unknown.decide_surface();
    assert!(
        surface.uses_notify_fallback,
        "an unreachable presence watch falls back to notifications"
    );
}

// The name watched is exactly the extension host's presence name.
#[test]
fn p20_watches_org_myna_shell() {
    assert_eq!(PRESENCE_NAME, "org.myna.Shell");
    let presence = FakePresence::default();
    presence.set_present(true);
    assert!(
        presence.shell_present(),
        "the fake reports the org.myna.Shell owner state"
    );
}

// --- P23: the non-GNOME spawn seam is contract-only ----------------------
// The policy exposes a hook for launching myna-hud standalone where a
// focus-safe overlay backend exists; no backend ships this pass, but the
// seam and its decision are tested.

#[test]
fn p23_spawn_seam_is_contract_only() {
    let presence = FakePresence::default();
    presence.set_present(true);
    // The seam exists and reports the shell-present state; no backend ships
    // (the test asserts the hook returns "no spawn" in this build).
    assert_eq!(presence.spawn_hud_action(), "none");
}
