//! Env-gated GlobalShortcuts portal integration suite (`MYNA_PORTAL_TESTS=1`).
//!
//! Binds a test shortcut against a live `xdg-desktop-portal` with a
//! GlobalShortcuts backend and asserts `Activated`→`Press` / `Deactivated`→
//! `Release`, plus portal-unavailable → `Err(PortalUnavailable)` (contracts T1,
//! T2, T5, T6). Populated in branch 003c (T023). Skips cleanly when the gate is
//! unset, so the suite compiles and runs as a no-op offline (Principle II).

/// True when the portal integration suite is enabled. Unset gate → skip.
fn portal_enabled() -> bool {
    std::env::var("MYNA_PORTAL_TESTS").as_deref() == Ok("1")
}

#[test]
fn gate_skips_cleanly_when_unset() {
    if portal_enabled() {
        // Real portal assertions land in T023.
    } else {
        eprintln!("skipping portal_hw: set MYNA_PORTAL_TESTS=1 with a running xdg-desktop-portal");
    }
}
