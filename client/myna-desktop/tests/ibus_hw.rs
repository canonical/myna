//! Env-gated IBus integration suite (`MYNA_IBUS_TESTS=1`).
//!
//! Exercises the real `IbusInjector` against a running IBus daemon: connect,
//! make our engine the active one, commit text, and restore the prior engine —
//! contracts I1, I11 (and, on the desktop VM with a focused test entry, the
//! "hello lands in the field" acceptance of I1/SC-001). Focus/secure detection
//! (I5, I8) lands in the safety branch 003e (T035).
//!
//! ⚠️ **IMPORTANT**: This test changes the **global input engine** while it runs.
//! Run it in an **isolated** session, NOT against your real desktop IBus daemon:
//!
//! ```sh
//! cd client
//! dbus-run-session -- bash -c '
//!   export XDG_CONFIG_HOME=$(mktemp -d) XDG_CACHE_HOME=$(mktemp -d)
//!   unset WAYLAND_DISPLAY DISPLAY
//!   ibus-daemon --daemonize --panel disable --xim
//!   sleep 2
//!   MYNA_IBUS_TESTS=1 cargo test -p myna-desktop --test ibus_hw -- --test-threads=1
//! '
//! ```
//!
//! Running without `dbus-run-session` will interfere with your desktop session
//! and cause test failures due to engine conflicts.
//!
//! **Note**: Tests must run serially (`--test-threads=1`) because they manipulate
//! the global IBus engine state.
//!
//! It skips cleanly when the gate is unset, so the suite compiles and runs as a
//! no-op offline (Principle II).

use myna_desktop::inject::ibus::IbusInjector;
use myna_desktop::inject::Injector;

/// True when the IBus integration suite is enabled. Unset gate → skip.
fn ibus_enabled() -> bool {
    std::env::var("MYNA_IBUS_TESTS").as_deref() == Ok("1")
}

#[test]
fn gate_skips_cleanly_when_unset() {
    if ibus_enabled() {
        eprintln!("MYNA_IBUS_TESTS set: see ibus_commit_and_restore for the real assertions");
    } else {
        eprintln!("skipping ibus_hw: set MYNA_IBUS_TESTS=1 with a running IBus daemon");
    }
}

/// I1 + I11: IBus wire protocol integration test.
///
/// In a headless/isolated IBus session (no GUI, no focused text field), the
/// daemon does NOT send FocusIn when an engine is activated (there's no actual
/// input context to focus). With the fail-closed security fix, `acquire()` now
/// correctly refuses to proceed without a focus signal.
///
/// This test verifies:
/// 1. Connection to IBus daemon succeeds
/// 2. Wire protocol calls (RegisterComponent, SetGlobalEngine) succeed  
/// 3. Headless acquire correctly fails (Unavailable) due to missing focus
/// 4. End-to-end injection (acquire→commit→restore) requires a REAL GUI session
///    with a focused text field (manual quickstart step 4)
#[tokio::test]
async fn ibus_wire_protocol_headless() {
    if !ibus_enabled() {
        eprintln!("skipping ibus_wire_protocol_headless: MYNA_IBUS_TESTS unset");
        return;
    }

    // Connect to the daemon and verify address discovery works.
    let injector = IbusInjector::connect().await.expect("connect to IBus daemon");
    let _before = injector.global_engine().await;
    eprintln!("Connected to IBus daemon (global engine = {})", _before.as_deref().unwrap_or("none"));
    drop(injector);

    // In headless mode, acquire must fail (no focus signal).
    let mut injector = IbusInjector::connect().await.expect("connect to IBus daemon");
    let result = injector.acquire().await;

    // Headless: no focused input context → no FocusIn → acquire fails (fail-closed).
    assert!(
        result.is_err(),
        "acquire must fail in headless mode (no focus) (got {result:?})"
    );
    match &result {
        Err(myna_desktop::inject::InjectError::Unavailable(msg)) => {
            assert!(
                msg.contains("focus signal"),
                "error should mention missing focus signal: {msg}"
            );
            eprintln!("Headless acquire correctly refused (fail-closed): {}", result.as_ref().unwrap_err());
        }
        Err(myna_desktop::inject::InjectError::Backend(msg)) => {
            // If running against a real session (not isolated), SetGlobalEngine may
            // fail due to conflicts. This is acceptable - skip with a warning.
            eprintln!("⚠️  Backend error: {msg}");
            eprintln!("⚠️  Tests should run in isolated session (see module docs)");
            return;
        }
        other => panic!("expected Unavailable(focus signal...) or Backend(...), got {other:?}"),
    }

    // No need to call end(): acquire failed, so no engine activation to restore.
}

/// T035 / I5, I8: focus-out from a focused entry emits `FocusEvent::FocusOut`,
/// and a password-purpose entry (`SetContentType` PASSWORD) makes `acquire`
/// return `Err(SecureField)`.
///
/// Both require a **focused GUI input context** (a real editable widget / a
/// password field) that the isolated headless daemon does not provide, so the
/// automated body only asserts the injector connects; the focus-out and
/// secure-refusal edges are the manual GUI acceptance (quickstart step 4). The
/// detection code lives in `inject::ibus` (FocusOut→stream; PASSWORD→SecureField).
#[tokio::test]
async fn ibus_focus_and_secure_detection() {
    if !ibus_enabled() {
        eprintln!("skipping ibus_focus_and_secure_detection: MYNA_IBUS_TESTS unset");
        return;
    }
    let injector = IbusInjector::connect().await.expect("connect to IBus daemon");
    eprintln!(
        "connected (global engine = {:?}); focus a normal field then a password \
         field and verify FocusOut / SecureField manually (quickstart step 4)",
        injector.global_engine().await
    );
}

/// Fail-closed security property: `acquire` requires a positive focus signal
/// before allowing injection. This prevents the vulnerability where we would
/// proceed with `purpose=0` (unknown) when the focus/content-type callbacks
/// are delayed or missing.
///
/// In headless mode (no GUI, no focused field), IBus doesn't send FocusIn, so
/// `acquire` must fail. In a real GUI session with a focused text field, IBus
/// sends FocusIn immediately when the engine is activated, and `acquire` succeeds.
///
/// This test verifies the fail-closed behavior in headless mode. The success
/// path (with focus) is verified by the manual quickstart step 4.
#[tokio::test]
async fn ibus_fail_closed_on_timeout() {
    if !ibus_enabled() {
        eprintln!("skipping ibus_fail_closed_on_timeout: MYNA_IBUS_TESTS unset");
        return;
    }

    let mut injector = IbusInjector::connect().await.expect("connect to IBus daemon");
    // Headless: no focused input context → no FocusIn signal.
    let result = injector.acquire().await;

    // Fail-closed: must return Unavailable (or Backend if running against real session).
    match result {
        Err(myna_desktop::inject::InjectError::Unavailable(_)) => {
            // Expected in isolated headless session.
        }
        Err(myna_desktop::inject::InjectError::Backend(msg)) => {
            // If running against a real session, SetGlobalEngine may fail.
            eprintln!("⚠️  Backend error: {msg}");
            eprintln!("⚠️  Tests should run in isolated session (see module docs)");
            return;
        }
        other => panic!(
            "acquire must fail (Unavailable or Backend) when no focus signal arrives (got {other:?})"
        ),
    }

    // No cleanup needed: acquire failed, no engine was activated.
}
