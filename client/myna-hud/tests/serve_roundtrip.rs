// tests/serve_roundtrip.rs — end-to-end check of the `--serve-dbus`
// simulator over a REAL session bus (feature 004, T132; contract
// dbus-interface.md C1/C4/C6), the port of `dbus_headless.py`.
//
// Gated: it needs a session bus. Run under `dbus-run-session`, e.g.
//   dbus-run-session -- cargo test -p myna-hud --test serve_roundtrip
// Without one it skips (returns early) rather than failing, matching the
// harness convention for env-dependent checks.

use std::time::Duration;

use myna_hud::serve::{serve, Controls, Shared};
use myna_hud::states::wire;

fn have_session_bus() -> bool {
    std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some()
}

/// A minimal consumer proxy for the served interface.
#[zbus::proxy(
    interface = "com.canonical.Myna.Dictation",
    default_service = "com.canonical.Myna.Dictation",
    default_path = "/com/canonical/Myna/Dictation"
)]
trait Dictation {
    #[zbus(property)]
    fn state(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn audio_rms(&self) -> zbus::Result<f64>;
    fn start(&self) -> zbus::Result<(bool, String)>;
    fn stop(&self) -> zbus::Result<()>;
    fn toggle(&self) -> zbus::Result<()>;
}

#[tokio::test]
async fn serve_publishes_and_answers_over_the_bus() {
    if !have_session_bus() {
        eprintln!("     (skip) no DBUS_SESSION_BUS_ADDRESS — run under dbus-run-session");
        return;
    }

    let shared = Shared::default();
    // Recording, mid-level — an active session so levels flow.
    shared.set_controls(Controls {
        state: wire::RECORDING.into(),
        reason: String::new(),
        envelope: 0.6,
    });

    let _server = serve(shared.clone()).await.expect("claim the name");

    let client = zbus::Connection::session().await.expect("client bus");
    let proxy = DictationProxy::new(&client).await.expect("proxy");
    tokio::time::sleep(Duration::from_millis(200)).await;

    // --- Launching with a non-idle control publishes IMMEDIATELY ------
    // No Start/Toggle needed: the control set already implies a live
    // session (the lab's state selector is its session control). This is
    // the property that was missing when `--serve-dbus` showed nothing
    // until the user manually called Toggle over the bus.
    assert_eq!(
        proxy.state().await.expect("State"),
        wire::RECORDING,
        "a non-idle control set publishes on launch, with no Toggle"
    );

    // --- C6: Start begins a session and reports success ---------------
    let (ok, reason) = proxy.start().await.expect("Start");
    assert!(ok && reason.is_empty(), "Start reports success (C7 shape)");

    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        proxy.state().await.expect("State"),
        wire::RECORDING,
        "an active recording session publishes `recording`"
    );

    // --- C4: levels are non-zero while recording ----------------------
    let rms = proxy.audio_rms().await.expect("AudioRms");
    assert!(rms > 0.0, "levels flow during a live session: {rms}");

    // --- Stop clears the pill to idle (dbus_headless.py) ---------------
    proxy.stop().await.expect("Stop");
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        proxy.state().await.expect("State"),
        wire::IDLE,
        "Stop clears to idle"
    );
    // ...and levels fall silent.
    let rms_idle = proxy.audio_rms().await.expect("AudioRms idle");
    assert_eq!(rms_idle, 0.0, "no levels while idle");

    // --- C6: Toggle round-trips a single session ----------------------
    proxy.toggle().await.expect("Toggle on");
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(proxy.state().await.expect("State"), wire::RECORDING);
    proxy.toggle().await.expect("Toggle off");
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(proxy.state().await.expect("State"), wire::IDLE);

    // --- A second server stands down rather than fighting -------------
    // The well-known name is a singleton; while `_server` owns it, a
    // second `serve()` must refuse (DoNotQueue, no replacement — the
    // simulator never fights the real daemon). Done here, in the same
    // test, because the name is process-wide and two owners cannot
    // coexist across parallel tests.
    let second = serve(Shared::default()).await;
    assert!(
        second.is_err(),
        "a second server stands down when the name is taken"
    );
}
