//! A private session bus and X server for the tests that run the real binary.

use std::io::BufRead;
use std::process::{Child, Command, Stdio};

/// A session bus that exists only for one test, torn down on drop.
///
/// Private because those tests assert on name ownership, which only means
/// something on a bus the test controls: on the developer's own bus a
/// running HUD already owns the name and the spawned singleton would forward
/// its activation and exit.
pub struct PrivateBus {
    daemon: Child,
    pub address: String,
}

impl PrivateBus {
    /// Spawn one, or `None` where there is no `dbus-daemon` to spawn.
    pub fn spawn() -> Option<Self> {
        let mut daemon = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address"])
            .stdout(Stdio::piped())
            // The bus activates portals for its one client; that chatter is
            // not the test's diagnostics.
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut address = String::new();
        std::io::BufReader::new(daemon.stdout.take()?)
            .read_line(&mut address)
            .ok()?;
        let address = address.trim().to_string();
        if address.is_empty() {
            let _ = daemon.kill();
            return None;
        }
        Some(Self { daemon, address })
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

/// An X server started for one test, torn down on drop.
pub struct Headless {
    server: Child,
    pub display: String,
}

impl Headless {
    /// Spawn one with a `WxHxD` screen on a display number of its own
    /// choosing (`-displayfd`, the same trick as `xvfb-run -a`), or `None`
    /// where there is no `Xvfb`.
    pub fn spawn(screen: &str) -> Option<Self> {
        let mut server = Command::new("Xvfb")
            .args(["-displayfd", "1", "-screen", "0", screen])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut number = String::new();
        std::io::BufReader::new(server.stdout.take()?)
            .read_line(&mut number)
            .ok()?;
        let number = number.trim();
        if number.is_empty() {
            let _ = server.kill();
            return None;
        }
        Some(Self {
            display: format!(":{number}"),
            server,
        })
    }
}

impl Drop for Headless {
    fn drop(&mut self) {
        let _ = self.server.kill();
        let _ = self.server.wait();
    }
}

/// A HUD process, killed on drop so a failed assertion leaves nothing behind.
pub struct Hud(pub Child);

impl Drop for Hud {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
