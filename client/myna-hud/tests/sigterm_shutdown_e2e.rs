// tests/sigterm_shutdown_e2e.rs — stopping the shipped binary the way its
// host does (feature 004, T124).
//
// `myna-desktop` retires the HUD with a signal, as does Ctrl-C on a dev run.
// The end-user symptom of getting that wrong is not visible in-process: the
// renderer dies mid-frame, `com.canonical.Myna.Hud` stays owned until the
// bus reaps the connection, and `myna-desktop`'s `NameOwnerChanged` pruning
// is what has to clean up after it. So this test runs the real `myna-hud`
// binary, in hosted mode, on a session bus of its own, and asserts it leaves
// through `main` with status 0 rather than being killed by the signal.
//
// The companion in-process check is `tests/quit_on_signal.rs`; this one is
// the E2E. It brings its own bus, and its own X server where the machine has
// no display (`cargo test` in CI), so it runs there rather than skipping into
// a green nothing. It skips only where neither `dbus-daemon` nor `Xvfb` is
// installed.

use std::io::BufRead;
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// The HUD's well-known name, owned once the hosted app is up.
const HUD_NAME: &str = "com.canonical.Myna.Hud";

/// A session bus that exists only for this test, torn down on drop.
///
/// Private because the assertion is about *name ownership*, which is only
/// meaningful on a bus this test controls - on the developer's own bus a
/// running HUD already owns the name and the spawned singleton would forward
/// its activation and exit. Same reasoning as `tests/serve_roundtrip.rs`,
/// except the address is passed to the child rather than set process-wide.
struct PrivateBus {
    daemon: Child,
    address: String,
}

impl PrivateBus {
    /// Spawn one, or `None` where there is no `dbus-daemon` to spawn.
    fn spawn() -> Option<Self> {
        let mut daemon = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address"])
            .stdout(Stdio::piped())
            // The bus activates portals for its one client; that chatter is
            // not this test's diagnostics.
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

/// A HUD process, killed on drop so a failed assertion leaves nothing behind.
struct Hud(Child);

impl Drop for Hud {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// An X server started for this test, torn down on drop.
struct Headless {
    server: Child,
    display: String,
}

impl Headless {
    /// Spawn one on a display number of its own choosing (`-displayfd`, the
    /// same trick as `xvfb-run -a`), or `None` where there is no `Xvfb`.
    fn spawn() -> Option<Self> {
        let mut server = Command::new("Xvfb")
            .args(["-displayfd", "1", "-screen", "0", "800x600x24"])
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

/// True when the environment already has a display for GTK to open.
fn has_display() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some()
}

#[tokio::test]
async fn sigterm_quits_the_application_instead_of_killing_it() {
    let Some(bus) = PrivateBus::spawn() else {
        eprintln!("skipping sigterm_shutdown_e2e: no dbus-daemon");
        return;
    };
    // Borrow the developer's session where there is one; stand up an X
    // server where there is not.
    let headless = if has_display() {
        None
    } else {
        let Some(headless) = Headless::spawn() else {
            eprintln!("skipping sigterm_shutdown_e2e: no display and no Xvfb");
            return;
        };
        Some(headless)
    };

    let mut command = Command::new(env!("CARGO_BIN_EXE_myna-hud"));
    command
        .env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
        .stdout(Stdio::null());
    if let Some(headless) = &headless {
        command
            .env("DISPLAY", &headless.display)
            .env_remove("WAYLAND_DISPLAY");
    }
    let mut hud = Hud(command.spawn().expect("spawn myna-hud"));
    let pid = hud.0.id() as libc::pid_t;

    // Up means owning the name: GTK is running, the window exists (unmapped,
    // as idle), and the bus consumer is attached - i.e. there is a frame loop
    // for the signal to interrupt.
    let connection = zbus::connection::Builder::address(bus.address.as_str())
        .expect("private bus address")
        .build()
        .await
        .expect("connect to the private bus");
    let dbus = zbus::fdo::DBusProxy::new(&connection)
        .await
        .expect("org.freedesktop.DBus proxy");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let owned = dbus
            .name_has_owner(HUD_NAME.try_into().unwrap())
            .await
            .expect("NameHasOwner");
        if owned {
            break;
        }
        if let Some(status) = hud.0.try_wait().expect("try_wait") {
            panic!("myna-hud exited before owning {HUD_NAME}: {status}");
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "myna-hud never owned {HUD_NAME}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0, "kill failed");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = hud.0.try_wait().expect("try_wait") {
            break status;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "myna-hud ignored SIGTERM"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    assert_eq!(
        status.signal(),
        None,
        "myna-hud was killed by the signal instead of quitting on it"
    );
    assert_eq!(status.code(), Some(0), "myna-hud exited {status}");
}
