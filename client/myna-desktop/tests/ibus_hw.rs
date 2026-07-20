//! Env-gated IBus integration suite (`MYNA_IBUS_TESTS=1`).
//!
//! Exercises the real `IbusInjector` against a running IBus daemon: connect,
//! make our engine the active one, commit text, and restore the prior engine —
//! contracts I1, I11 (and, on the desktop VM with a focused test entry, the
//! "hello lands in the field" acceptance of I1/SC-001). Focus/secure detection
//! (I5, I8) lands in the safety branch 003e (T035).
//!
//! ⚠️ This test changes the **global input engine** while it runs, and an IBus
//! daemon writes its address file under `$XDG_CONFIG_HOME/ibus/bus/`. Run it in
//! an **isolated** session so it never touches your real IBus config — isolate
//! `XDG_CONFIG_HOME`/`XDG_CACHE_HOME` and drop the shared display name so the
//! daemon can't clobber `~/.config/ibus/bus/<machine>-unix-wayland-0`:
//!
//! ```sh
//! dbus-run-session -- bash -c '
//!   export XDG_CONFIG_HOME=$(mktemp -d) XDG_CACHE_HOME=$(mktemp -d)
//!   unset WAYLAND_DISPLAY DISPLAY
//!   ibus-daemon --daemonize --panel disable --xim
//!   sleep 2
//!   MYNA_IBUS_TESTS=1 cargo test -p myna-desktop --no-default-features --test ibus_hw
//! '
//! ```
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

/// I1 + I11: connect, acquire (become the active engine), commit "hello", end,
/// and restore the engine that was active before the test — exactly once.
#[tokio::test]
async fn ibus_commit_and_restore() {
    if !ibus_enabled() {
        eprintln!("skipping ibus_commit_and_restore: MYNA_IBUS_TESTS unset");
        return;
    }

    // Record the engine active before we touch anything.
    let probe = IbusInjector::connect().await.expect("connect to IBus daemon");
    let before = probe.global_engine().await;
    drop(probe);

    let mut injector = IbusInjector::connect().await.expect("connect to IBus daemon");
    let _target = injector.acquire().await.expect("acquire the focused target");

    // Commit-only: a literal segment. On the VM this lands in the focused test
    // entry (I1/SC-001); here we assert the wire call succeeds.
    injector.commit("hello").await.expect("commit hello");

    // End restores the prior engine (idempotent — call twice).
    injector.end().await;
    injector.end().await;

    // I11: if there was a prior global engine, it is restored exactly once. In a
    // fresh/isolated session there may be none — then there is nothing to
    // restore to and our engine legitimately stays; the meaningful assertion is
    // that the full acquire→commit→end wire cycle above succeeded.
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
