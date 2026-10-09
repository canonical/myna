//! The HUD host supervisor against a private session bus and a fake HUD
//! script that records what happens to it.

use std::io::BufRead;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use gio::glib::{MainContext, Variant};
use gio::prelude::*;

const NAME: &str = "com.canonical.Myna.Dictation";

struct Killer(Child);

impl Drop for Killer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Rig {
    dir: PathBuf,
    address: String,
    _bus: Killer,
    daemon: gio::DBusConnection,
    host: Option<Child>,
}

impl Rig {
    /// `body` is the fake HUD's script; it gets the log path as `$LOG`.
    fn new(tag: &str, body: &str, grace_ms: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("hud-host-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("hud");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nLOG={}/log\necho \"start $*\" >> $LOG\n{body}\n",
                dir.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut bus = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("dbus-daemon is installed where the suites run");
        let mut address = String::new();
        std::io::BufReader::new(bus.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let address = address.trim().to_owned();
        // A context of its own: tests run on parallel threads.
        let context = MainContext::new();
        let daemon = context
            .with_thread_default(|| {
                context.block_on(gio::DBusConnection::for_address_future(
                    &address,
                    gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
                        | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
                    None,
                ))
            })
            .unwrap()
            .unwrap();
        let mut rig = Self {
            dir,
            address,
            _bus: Killer(bus),
            daemon,
            host: None,
        };
        rig.host = Some(rig.start_host(grace_ms));
        rig
    }

    fn start_host(&self, grace_ms: &str) -> Child {
        let child = Command::new(env!("CARGO_BIN_EXE_myna-hud-host"))
            .env("DBUS_SESSION_BUS_ADDRESS", &self.address)
            .env("MYNA_HUD_BINARY", self.dir.join("hud"))
            .env("MYNA_HUD_HOST_GRACE_MS", grace_ms)
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
    }

    fn call(&self, method: &str, flags: u32) {
        self.daemon
            .call_sync(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                method,
                Some(&if method == "RequestName" {
                    Variant::tuple_from_iter([NAME.to_variant(), flags.to_variant()])
                } else {
                    Variant::tuple_from_iter([NAME.to_variant()])
                }),
                None,
                gio::DBusCallFlags::NONE,
                5000,
                None::<&gio::Cancellable>,
            )
            .unwrap();
    }

    fn daemon_up(&self) {
        self.call("RequestName", 0);
    }

    fn daemon_down(&self) {
        self.call("ReleaseName", 0);
    }

    fn log(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn wait_for(&self, what: &str, done: impl Fn(&[String]) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if done(&self.log()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("timed out waiting for {what}; log: {:?}", self.log());
    }

    fn count(log: &[String], prefix: &str) -> usize {
        log.iter().filter(|line| line.starts_with(prefix)).count()
    }

    fn settle(&self) {
        std::thread::sleep(Duration::from_millis(700));
    }

    fn running(&self, marker: &str) -> bool {
        Command::new("pgrep")
            .args(["-f", marker])
            .stdout(Stdio::null())
            .status()
            .unwrap()
            .success()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        if let Some(mut host) = self.host.take() {
            let _ = host.kill();
            let _ = host.wait();
        }
        let _ = Command::new("pkill").args(["-f", &self.marker()]).status();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Rig {
    fn marker(&self) -> String {
        self.dir.join("hud").display().to_string()
    }
}

const WELL_BEHAVED: &str = "trap 'echo term >> $LOG; exit 0' TERM\nwhile :; do sleep 0.05; done";

#[test]
fn the_hud_runs_only_while_the_daemon_owns_the_name() {
    let rig = Rig::new("lifecycle", WELL_BEHAVED, "3000");
    rig.settle();
    assert!(rig.log().is_empty(), "no daemon, no HUD");

    rig.daemon_up();
    rig.wait_for("start", |log| log == ["start --host x11"]);
    rig.daemon_down();
    rig.wait_for("term", |log| log.iter().any(|l| l == "term"));
    rig.settle();
    assert_eq!(Rig::count(&rig.log(), "start"), 1);

    rig.daemon_up();
    rig.wait_for("start again", |log| Rig::count(log, "start") == 2);
}

#[test]
fn a_hud_that_crashes_is_started_again() {
    let body =
        "if [ ! -e $LOG.crashed ]; then touch $LOG.crashed; exit 1; fi\n".to_owned() + WELL_BEHAVED;
    let rig = Rig::new("crash", &body, "3000");
    rig.daemon_up();
    rig.wait_for("respawn", |log| Rig::count(log, "start") == 2);
    rig.settle();
    assert_eq!(Rig::count(&rig.log(), "start"), 2);
}

#[test]
fn exit_78_ends_it_for_the_session() {
    let rig = Rig::new("cannot-host", "exit 78", "3000");
    rig.daemon_up();
    rig.wait_for("start", |log| Rig::count(log, "start") == 1);
    rig.daemon_down();
    rig.daemon_up();
    rig.settle();
    rig.settle();
    assert_eq!(Rig::count(&rig.log(), "start"), 1);
}

#[test]
fn a_hud_that_ignores_term_is_killed_after_the_grace() {
    let rig = Rig::new(
        "stubborn",
        "trap '' TERM\nwhile :; do sleep 0.05; done",
        "200",
    );
    rig.daemon_up();
    rig.wait_for("start", |log| Rig::count(log, "start") == 1);
    assert!(rig.running(&rig.marker()));
    rig.daemon_down();
    let deadline = Instant::now() + Duration::from_secs(5);
    while rig.running(&rig.marker()) {
        assert!(Instant::now() < deadline, "the HUD outlived the grace");
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn stopping_the_supervisor_stops_the_hud() {
    let mut rig = Rig::new("shutdown", WELL_BEHAVED, "3000");
    rig.daemon_up();
    rig.wait_for("start", |log| Rig::count(log, "start") == 1);
    let mut host = rig.host.take().unwrap();
    // SAFETY: signalling our own child by pid.
    unsafe { libc::kill(host.id() as i32, libc::SIGTERM) };
    let status = host.wait().unwrap();
    assert!(status.success(), "{status:?}");
    rig.wait_for("term", |log| log.iter().any(|l| l == "term"));
    assert!(!rig.running(&rig.marker()));
}

#[test]
fn a_missing_hud_binary_is_logged_and_not_fatal() {
    let rig = Rig::new("missing", WELL_BEHAVED, "3000");
    let output = Command::new(env!("CARGO_BIN_EXE_myna-hud-host"))
        .env("DBUS_SESSION_BUS_ADDRESS", &rig.address)
        .env("MYNA_HUD_BINARY", rig.dir.join("nope"))
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("nope"));
}
