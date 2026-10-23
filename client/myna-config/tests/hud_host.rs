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

const IFACE_XML: &str = "<node><interface name='com.canonical.Myna.Dictation'>\
    <method name='RegisterClient'><arg type='u' direction='out'/></method>\
    <method name='UnregisterClient'><arg type='u' direction='out'/></method>\
    </interface></node>";

/// The daemon's side of the bus: owns the name on request, and answers
/// `RegisterClient`/`UnregisterClient` by logging "register <sender>" into the
/// fake HUD's log, so the order of the two is observable. It lives on a thread
/// with a main context of its own, because tests run on parallel threads.
struct Daemon {
    commands: std::sync::mpsc::Sender<(&'static str, std::sync::mpsc::Sender<()>)>,
    context: MainContext,
}

impl Daemon {
    fn start(address: &str, log: PathBuf) -> Self {
        let (commands, inbox) =
            std::sync::mpsc::channel::<(&'static str, std::sync::mpsc::Sender<()>)>();
        let context = MainContext::new();
        let address = address.to_owned();
        let owned = context.clone();
        let (ready_tx, ready) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            owned
                .with_thread_default(|| {
                    let conn = owned
                        .block_on(gio::DBusConnection::for_address_future(
                            &address,
                            gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
                                | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
                            None,
                        ))
                        .unwrap();
                    let info = gio::DBusNodeInfo::for_xml(IFACE_XML).unwrap();
                    let _registration = conn
                        .register_object(
                            "/com/canonical/Myna/Dictation",
                            &info
                                .lookup_interface("com.canonical.Myna.Dictation")
                                .unwrap(),
                        )
                        .method_call(move |_, sender, _, _, method, _, invocation| {
                            use std::io::Write;
                            let verb = if method == "RegisterClient" {
                                "register"
                            } else {
                                "unregister"
                            };
                            let mut file = std::fs::OpenOptions::new()
                                .create(true)
                                .append(true)
                                .open(&log)
                                .unwrap();
                            writeln!(file, "{verb} {}", sender.unwrap_or("")).unwrap();
                            invocation.return_value(Some(&(1u32,).to_variant()));
                        })
                        .build()
                        .unwrap();
                    ready_tx.send(()).unwrap();
                    loop {
                        owned.iteration(true);
                        match inbox.try_recv() {
                            Ok((method, done)) => {
                                conn.call_sync(
                                    Some("org.freedesktop.DBus"),
                                    "/org/freedesktop/DBus",
                                    "org.freedesktop.DBus",
                                    method,
                                    Some(&if method == "RequestName" {
                                        Variant::tuple_from_iter([
                                            NAME.to_variant(),
                                            0u32.to_variant(),
                                        ])
                                    } else {
                                        Variant::tuple_from_iter([NAME.to_variant()])
                                    }),
                                    None,
                                    gio::DBusCallFlags::NONE,
                                    5000,
                                    None::<&gio::Cancellable>,
                                )
                                .unwrap();
                                let _ = done.send(());
                            }
                            Err(std::sync::mpsc::TryRecvError::Empty) => {}
                            Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                        }
                    }
                })
                .unwrap();
        });
        ready.recv().unwrap();
        Self { commands, context }
    }

    fn call(&self, method: &'static str) {
        let (done, finished) = std::sync::mpsc::channel();
        self.commands.send((method, done)).unwrap();
        self.context.wakeup();
        finished.recv().unwrap();
    }
}

struct Rig {
    dir: PathBuf,
    address: String,
    _bus: Killer,
    daemon: Daemon,
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
        let daemon = Daemon::start(&address, dir.join("log"));
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
        self.start_host_on(grace_ms, &[])
    }

    /// The host as a session with only the display variables `vars` starts it.
    fn start_host_on(&self, grace_ms: &str, vars: &[(&str, &str)]) -> Child {
        let mut command = Command::new(env!("CARGO_BIN_EXE_myna-hud-host"));
        for name in ["DISPLAY", "WAYLAND_DISPLAY", "XDG_SESSION_TYPE"] {
            command.env_remove(name);
        }
        let child = command
            .envs(vars.iter().copied())
            .env("DBUS_SESSION_BUS_ADDRESS", &self.address)
            .env("MYNA_HUD_BINARY", self.dir.join("hud"))
            .env("MYNA_HUD_HOST_GRACE_MS", grace_ms)
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
    }

    fn daemon_up(&self) {
        self.daemon.call("RequestName");
    }

    fn daemon_down(&self) {
        self.daemon.call("ReleaseName");
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

    /// Wait for the host to leave on its own, its HUD stopped first.
    fn ends_with_its_hud(&self, mut host: Child) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = host.try_wait().unwrap() {
                break status;
            }
            if Instant::now() > deadline {
                let _ = host.kill();
                panic!("the host outlived its display");
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        assert!(status.success(), "{status:?}");
        self.wait_for("term", |log| log.iter().any(|l| l == "term"));
        assert!(!self.running(&self.marker()));
    }

    /// Replace the host started without a display by one started with `vars`.
    fn restart_host_on(&mut self, vars: &[(&str, &str)]) -> Child {
        let mut idle = self.host.take().unwrap();
        let _ = idle.kill();
        let _ = idle.wait();
        self.start_host_on("3000", vars)
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
    rig.wait_for("start", |log| Rig::count(log, "start") == 1);
    rig.daemon_down();
    rig.wait_for("term", |log| log.iter().any(|l| l == "term"));
    rig.settle();
    assert_eq!(Rig::count(&rig.log(), "start"), 1);

    rig.daemon_up();
    rig.wait_for("start again", |log| Rig::count(log, "start") == 2);
}

/// The HUD takes seconds to register itself, and until something is
/// registered the daemon shows toasts: the host's registration must be on the
/// daemon's books before the HUD even starts.
#[test]
fn the_host_registers_with_the_daemon_before_it_starts_the_hud() {
    let rig = Rig::new("register", WELL_BEHAVED, "3000");
    rig.daemon_up();
    rig.wait_for("start", |log| Rig::count(log, "start") == 1);
    let log = rig.log();
    assert!(log[0].starts_with("register :1."), "{log:?}");
    assert_eq!(log[1], "start --host x11", "{log:?}");
    assert_eq!(Rig::count(&log, "register"), 1, "{log:?}");
}

#[test]
fn a_daemon_that_comes_back_is_registered_with_again() {
    let rig = Rig::new("reregister", WELL_BEHAVED, "3000");
    rig.daemon_up();
    rig.wait_for("start", |log| Rig::count(log, "start") == 1);
    rig.daemon_down();
    rig.wait_for("term", |log| log.iter().any(|l| l == "term"));
    rig.daemon_up();
    rig.wait_for("second start", |log| Rig::count(log, "start") == 2);
    let log = rig.log();
    assert_eq!(Rig::count(&log, "register"), 2, "{log:?}");
    assert_eq!(Rig::count(&log, "unregister"), 0, "{log:?}");
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
    // It withdrew its pill on the first 78, and the second daemon never
    // got a registration at all.
    let log = rig.log();
    assert_eq!(Rig::count(&log, "register"), 1, "{log:?}");
    assert_eq!(Rig::count(&log, "unregister"), 1, "{log:?}");
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
    rig.wait_for("unregister", |log| Rig::count(log, "unregister") == 1);
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

/// An X server on a free display, until dropped.
struct Xvfb(Child, String);

impl Xvfb {
    fn start() -> Option<Self> {
        let mut child = Command::new("Xvfb")
            .args([
                "-displayfd",
                "1",
                "-nolisten",
                "tcp",
                "-screen",
                "0",
                "64x64x8",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut number = String::new();
        std::io::BufReader::new(child.stdout.take()?)
            .read_line(&mut number)
            .ok()?;
        Some(Self(child, format!(":{}", number.trim())))
    }

    fn stop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for Xvfb {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A logout leaves the user manager, and so the session bus, running: the
/// display going away is what says the session is over.
#[test]
fn the_host_and_its_hud_end_with_the_display() {
    let Some(mut xvfb) = Xvfb::start() else {
        eprintln!("skipped: no Xvfb to stand in for the session's display");
        return;
    };
    let mut rig = Rig::new("display", WELL_BEHAVED, "3000");
    let host = rig.restart_host_on(&[("DISPLAY", &xvfb.1)]);
    rig.daemon_up();
    rig.wait_for("start", |log| Rig::count(log, "start") == 1);

    xvfb.stop();
    rig.ends_with_its_hud(host);
}

/// On Wayland the compositor's socket is the session's display: the HUD is
/// a layer surface on it, and the host ends when the compositor goes. Only
/// the socket matters to the host, so a listener stands in for KWin.
#[test]
fn on_wayland_the_hud_is_a_layer_surface_and_ends_with_the_compositor() {
    let mut rig = Rig::new("wayland", WELL_BEHAVED, "3000");
    let socket = rig.dir.join("wayland-test");
    let compositor = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    // Xwayland's DISPLAY is in a Plasma session too; Wayland wins.
    let host = rig.restart_host_on(&[
        ("WAYLAND_DISPLAY", socket.to_str().unwrap()),
        ("DISPLAY", ":nowhere"),
    ]);
    compositor.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let client = loop {
        if let Ok((client, _)) = compositor.accept() {
            break client;
        }
        assert!(
            Instant::now() < deadline,
            "the host never connected to the compositor"
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    rig.daemon_up();
    rig.wait_for("start", |log| Rig::count(log, "start") == 1);
    assert!(
        rig.log().contains(&"start --host layer-shell".to_owned()),
        "{:?}",
        rig.log()
    );

    drop((client, compositor));
    rig.ends_with_its_hud(host);
}

#[test]
fn a_host_with_no_display_runs_on() {
    let rig = Rig::new("no-display", WELL_BEHAVED, "3000");
    rig.settle();
    assert!(rig.host.is_some());
    rig.daemon_up();
    rig.wait_for("start", |log| Rig::count(log, "start") == 1);
}
