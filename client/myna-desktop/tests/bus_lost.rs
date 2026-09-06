//! The served session bus going away is detected, so the daemon can exit and
//! be restarted on the new one (the 2026-09-06 logout regression: GNOME's
//! `gnome-session-restart-dbus.service` replaces the user bus at logout, the
//! user service outlived it, and nothing on the new bus could find the
//! daemon).
//!
//! Needs a bus of its own to kill, so it spawns a private `dbus-daemon`
//! rather than joining the `MYNA_DBUS_TESTS` suite on a shared session; skips
//! cleanly where there is no `dbus-daemon` to spawn.

use std::io::BufRead;
use std::time::Duration;

use myna_desktop::dbus::serve::ZbusBus;

/// A session bus that exists only for this test binary.
///
/// Points `DBUS_SESSION_BUS_ADDRESS` at itself, which is what
/// `ZbusBus::serve` resolves against. Sound to set here because this is the
/// whole of the test binary and nothing has spawned a thread yet.
struct PrivateBus(std::process::Child);

impl PrivateBus {
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

    fn kill(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        self.kill();
    }
}

#[tokio::test]
async fn a_dead_bus_is_reported_lost() {
    let Some(mut bus) = PrivateBus::spawn() else {
        eprintln!("     (skip) no dbus-daemon to stand a private session bus on");
        return;
    };
    let served = ZbusBus::serve().await.expect("serve on the private bus");
    let lost = served.lost();

    // Not lost while the bus is up: the future must not resolve early, or the
    // daemon would exit at startup.
    tokio::pin!(lost);
    assert!(
        tokio::time::timeout(Duration::from_millis(300), lost.as_mut())
            .await
            .is_err(),
        "reported lost while the bus is alive"
    );

    bus.kill();

    tokio::time::timeout(Duration::from_secs(5), lost)
        .await
        .expect("bus death not detected within 5s");
}
