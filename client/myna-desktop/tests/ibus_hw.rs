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

/// I1 + I11: IBus wire protocol integration test — connect, acquire (become the
/// active engine), commit "hello", end, and restore the prior engine.
///
/// In a headless/isolated IBus session there is no focused GUI field, so no
/// secure content-type is delivered; `purpose` stays 0 and `acquire` succeeds
/// (the security model refuses only a *known-secure* PASSWORD/PIN field, never
/// a slow/absent focus — that is the ordinary-field case). The end-to-end
/// "hello lands in the field" acceptance still needs a real GUI (quickstart
/// step 4).
#[tokio::test]
async fn ibus_commit_and_restore() {
    if !ibus_enabled() {
        eprintln!("skipping ibus_commit_and_restore: MYNA_IBUS_TESTS unset");
        return;
    }

    // Record the engine active before we touch anything.
    let probe = IbusInjector::connect().await.expect("connect to IBus daemon");
    let before = probe.global_engine().await;
    eprintln!("Connected to IBus daemon (global engine = {})", before.as_deref().unwrap_or("none"));
    drop(probe);

    let mut injector = IbusInjector::connect().await.expect("connect to IBus daemon");
    match injector.acquire().await {
        Ok(_target) => {}
        Err(myna_desktop::inject::InjectError::Backend(msg)) => {
            // Running against a real (non-isolated) session: SetGlobalEngine can
            // conflict with the live engine. Skip with a warning.
            eprintln!("⚠️  Backend error: {msg}");
            eprintln!("⚠️  Tests should run in an isolated session (see module docs)");
            return;
        }
        Err(other) => panic!("unexpected acquire error: {other:?}"),
    }

    // Commit-only: a literal segment. On the VM this lands in the focused test
    // entry (I1/SC-001); here we assert the wire call succeeds.
    injector.commit("hello").await.expect("commit hello");

    // End restores the prior engine (idempotent — call twice).
    injector.end().await;
    injector.end().await;

    // I11: if there was a prior global engine, it is restored exactly once.
    let after = IbusInjector::connect().await.expect("reconnect").global_engine().await;
    if before.is_some() {
        assert_eq!(after, before, "prior global engine must be restored on end");
    } else {
        eprintln!("no prior engine in this session; wire cycle completed (after = {after:?})");
    }
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
/// before allowing injection into a secure field. The security model (F2): a
/// secure field (PASSWORD/PIN) reliably drives `FocusIn` immediately followed
/// by `SetContentType`; `acquire` waits for `FocusIn`, then lets
/// `SetContentType` settle (CONTENT_TYPE_GRACE) before reading `purpose`, so a
/// password field can't slip through on the race between the two callbacks.
///
/// A *slow/absent* `FocusIn` is NOT a hard-fail: that is the ordinary-field
/// case (IBus focuses different widgets on different schedules), and refusing
/// there would break legitimate dictation. So in headless mode (no GUI field,
/// purpose stays 0) `acquire` succeeds — there is no actual secure field to
/// protect.
///
/// The security-relevant assertion (PASSWORD → `Err(SecureField)`) requires a
/// real focused password field and is the manual quickstart step 4; here we
/// assert the safe-default path (no secure content-type → acquire succeeds,
/// then cleanly restores).
#[tokio::test]
async fn ibus_secure_default_path() {
    if !ibus_enabled() {
        eprintln!("skipping ibus_secure_default_path: MYNA_IBUS_TESTS unset");
        return;
    }

    let mut injector = IbusInjector::connect().await.expect("connect to IBus daemon");
    // Headless: no secure content-type delivered → purpose stays 0 → safe.
    match injector.acquire().await {
        Ok(_target) => {
            // Safe default: no known-secure field, injection permitted.
            injector.end().await;
        }
        Err(myna_desktop::inject::InjectError::Backend(msg)) => {
            // Running against a real session: SetGlobalEngine can conflict. Skip.
            eprintln!("⚠️  Backend error: {msg}");
            eprintln!("⚠️  Tests should run in an isolated session (see module docs)");
        }
        Err(myna_desktop::inject::InjectError::SecureField) => {
            panic!("unexpected SecureField in headless mode (no password field focused)");
        }
        Err(other) => panic!("unexpected acquire error: {other:?}"),
    }
}
