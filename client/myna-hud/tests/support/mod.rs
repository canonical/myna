//! A private session bus and X server for the tests that run the real binary.

// Not every suite publishes.
#[allow(dead_code)]
pub mod dictation;

use std::io::BufRead;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// How long a suite waits for the HUD to do something.
#[allow(dead_code)] // Not every suite polls.
pub const WAIT: Duration = Duration::from_secs(20);

#[allow(dead_code)]
pub fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    wait_for(what, || done().then_some(()))
}

/// What `found` finds, polled for up to [`WAIT`].
#[allow(dead_code)]
pub fn wait_for<T>(what: &str, mut found: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(value) = found() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

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
#[allow(dead_code)] // Not every suite has an X server.
pub struct Headless {
    server: Child,
    pub display: String,
}

#[allow(dead_code)]
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

impl Hud {
    /// SIGTERM, as its supervisor stops it, and how it exited.
    #[allow(dead_code)] // Not every suite stops it.
    pub fn terminate(&mut self) -> ExitStatus {
        let pid = self.0.id() as libc::pid_t;
        assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0, "kill failed");
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(status) = self.0.try_wait().expect("try_wait") {
                return status;
            }
            assert!(Instant::now() < deadline, "myna-hud ignored SIGTERM");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Hud {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
