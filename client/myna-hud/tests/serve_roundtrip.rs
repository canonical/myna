// tests/serve_roundtrip.rs — end-to-end check of the `--serve-dbus`
// simulator over a REAL session bus (feature 004, T132; contract
// dbus-interface.md C1/C4/C6), the port of `dbus_headless.py`.
//
// Runs on a session bus of its own, spawned here. It used to take whatever
// `DBUS_SESSION_BUS_ADDRESS` pointed at, which meant the developer's real
// session bus - and `serve()` claims `com.canonical.Myna.Dictation`, which the
// real daemon already owns there. The test therefore failed on precisely the
// machines most likely to run it (anyone with myna installed) with
// "already owned (myna-desktop running?)", and passed only under
// `dbus-run-session`. A private bus is the property the test actually wants:
// it asserts on singleton *name ownership*, which is only meaningful in a bus
// it controls. Skips cleanly where `dbus-daemon` is not installed.

// The `serve` module is dev-lab-only (#[cfg(dev_lab)]); skip this test when
// dev_lab is off (e.g. coverage builds, per build.rs / T171).
#![cfg(dev_lab)]

use std::io::BufRead;
use std::time::Duration;

use myna_hud::serve::{serve, Controls, Shared};
use myna_hud::states::wire;

/// A session bus that exists only for this test binary, torn down on drop.
///
/// Points `DBUS_SESSION_BUS_ADDRESS` at itself, which is what every
/// `zbus::Connection::session()` below resolves against. Sound to set here
/// because this is the whole of the test binary and nothing has spawned a
/// thread yet.
struct PrivateBus(std::process::Child);

impl PrivateBus {
    /// Spawn one, or `None` where there is no `dbus-daemon` to spawn.
    fn spawn() -> Option<Self> {
        let mut child = std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .ok()?;
        let mut address = String::new();
        std::io::BufReader::new(child.stdout.take()?)
            .read_line(&mut address)
            .ok()?;
        let address = address.trim();
        if address.is_empty() {
            let _ = child.kill();
            return None;
        }
        std::env::set_var("DBUS_SESSION_BUS_ADDRESS", address);
        Some(Self(child))
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
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
    fn status_message(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn audio_rms(&self) -> zbus::Result<f64>;
    fn start(&self) -> zbus::Result<(bool, String)>;
    fn stop(&self) -> zbus::Result<()>;
    fn toggle(&self) -> zbus::Result<()>;
}

#[tokio::test]
async fn serve_publishes_and_answers_over_the_bus() {
    let Some(_bus) = PrivateBus::spawn() else {
        eprintln!("     (skip) no dbus-daemon to stand a private session bus on");
        return;
    };

    let shared = Shared::default();
    // Recording, mid-level — an active session so levels flow.
    shared.set_controls(Controls {
        state: wire::RECORDING.into(),
        status_message: "Listening".into(),
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
    assert_eq!(
        proxy.status_message().await.expect("StatusMessage"),
        "Listening",
        "the publisher-owned recording label is observable on the bus"
    );

    // A lab edit is the simulator publisher's StatusMessage override. The
    // next publish tick must carry it to a real D-Bus client, not merely the
    // embedded preview.
    shared.set_controls(Controls {
        state: wire::RECORDING.into(),
        status_message: "Listening through the test publisher".into(),
        envelope: 0.6,
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        proxy.status_message().await.expect("updated StatusMessage"),
        "Listening through the test publisher",
        "a live simulator override is published over D-Bus"
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
