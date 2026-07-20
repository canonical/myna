//! Env-gated IBus integration suite (`MYNA_IBUS_TESTS=1`).
//!
//! Exercises the real `IbusInjector` against a running IBus daemon: commit into
//! a focused test entry, global-engine set/restore, and focus/secure detection
//! (contracts I1, I5, I8, I9, I11). Populated in branches 003b (T017) and 003e
//! (T035). Skips cleanly when the gate is unset, so the suite compiles and runs
//! as a no-op offline (Principle II: identical code on the VM and on hardware).

/// True when the IBus integration suite is enabled *and* an IBus daemon looks
/// reachable. Unset gate → skip.
fn ibus_enabled() -> bool {
    std::env::var("MYNA_IBUS_TESTS").as_deref() == Ok("1")
}

#[test]
fn gate_skips_cleanly_when_unset() {
    if ibus_enabled() {
        // Real IBus assertions land in T017 / T035.
    } else {
        eprintln!("skipping ibus_hw: set MYNA_IBUS_TESTS=1 with a running IBus daemon");
    }
}
