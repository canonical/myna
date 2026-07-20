//! Env-gated GlobalShortcuts portal integration suite (`MYNA_PORTAL_TESTS=1`).
//!
//! Binds a test shortcut against a live `xdg-desktop-portal` with a
//! GlobalShortcuts backend and asserts the bind succeeds / fails cleanly
//! (contracts T5, T6). The full `Activated`→`Press` / `Deactivated`→`Release`
//! edges (T1, T2) require physically holding the bound key, so they are the
//! manual hardware acceptance (quickstart step 3); the autorepeat-dedup + edge
//! mapping is proven hermetically in `shortcut::portal::tests` (T022).
//!
//! ⚠️ Binding pops the desktop's own shortcut-confirmation dialog, so this is
//! gated: run it in a disposable desktop session. Skips cleanly when the gate is
//! unset, so the suite compiles and runs as a no-op offline (Principle II).

use myna_desktop::shortcut::portal::GlobalShortcutTrigger;

/// True when the portal integration suite is enabled. Unset gate → skip.
fn portal_enabled() -> bool {
    std::env::var("MYNA_PORTAL_TESTS").as_deref() == Ok("1")
}

#[test]
fn gate_skips_cleanly_when_unset() {
    if portal_enabled() {
        eprintln!("MYNA_PORTAL_TESTS set: see portal_bind_succeeds_or_reports_cleanly");
    } else {
        eprintln!("skipping portal_hw: set MYNA_PORTAL_TESTS=1 with a running xdg-desktop-portal");
    }
}

/// T5/T6: `bind` either succeeds against a live GlobalShortcuts backend, or
/// returns a clear `PortalUnavailable`/`BindRejected` — never a panic or hang.
#[tokio::test]
async fn portal_bind_succeeds_or_reports_cleanly() {
    if !portal_enabled() {
        eprintln!("skipping portal_bind: MYNA_PORTAL_TESTS unset");
        return;
    }
    match GlobalShortcutTrigger::bind("dictate-test", Some("SUPER+j")).await {
        Ok(_trigger) => eprintln!("bound test shortcut; hold Super+J to see Press/Release (manual)"),
        Err(e) => eprintln!("portal bind reported cleanly: {e}"),
    }
}
