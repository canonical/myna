//! Settings follows a daemon that restarts: a change the new daemon signals
//! a moment after it takes the name is read, not lost to a subscription
//! that has not yet learned the owner. The signal is the standard
//! `PropertiesChanged`, not an interface of the daemon's own.

use std::cell::{Cell, RefCell};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::time::Duration;

use gio::glib::variant::ToVariant;
use gtk4::glib::{self, MainContext};
use myna_config::adapters::dictation_daemon::SessionBusDaemon;
use myna_config::ports::DictationDaemon;

const NAME: &str = "com.canonical.Myna.Dictation";
const PATH: &str = "/com/canonical/Myna/Dictation";
const XML: &str = r#"<node><interface name="com.canonical.Myna.Dictation">
<property name="State" type="s" access="read"/>
<property name="InstalledSteps" type="s" access="read"/>
</interface></node>"#;
const EMPTY: &str = r#"{"models":[],"steps":[]}"#;
const FULL: &str = r#"{"models":[{"model":"tidy"}],"steps":[]}"#;

struct Bus {
    child: Child,
    address: String,
}

impl Bus {
    fn start() -> Option<Self> {
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address"])
            .stdout(Stdio::piped())
            .spawn()
            .ok()?;
        let mut address = String::new();
        BufReader::new(child.stdout.take()?)
            .read_line(&mut address)
            .ok()?;
        Some(Self {
            child,
            address: address.trim().to_owned(),
        })
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn connect(address: &str) -> gio::DBusConnection {
    MainContext::ref_thread_default()
        .block_on(gio::DBusConnection::for_address_future(
            address,
            gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
                | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
            None,
        ))
        .unwrap()
}

/// A daemon that owns the name and serves `steps` as it changes.
struct Served {
    connection: gio::DBusConnection,
    steps: Rc<RefCell<String>>,
}

impl Served {
    fn start(address: &str, steps: &str) -> Self {
        let connection = connect(address);
        let current = Rc::new(RefCell::new(steps.to_owned()));
        let node = gio::DBusNodeInfo::for_xml(XML).unwrap();
        let interface = node.lookup_interface(NAME).unwrap();
        let seen = current.clone();
        connection
            .register_object(PATH, &interface)
            .property(move |_, _, _, _, name| match name {
                "State" => "idle".to_variant(),
                _ => seen.borrow().to_variant(),
            })
            .build()
            .unwrap();
        MainContext::ref_thread_default()
            .block_on(connection.call_future(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "RequestName",
                Some(&(NAME, 0u32).to_variant()),
                None,
                gio::DBusCallFlags::NONE,
                2_000,
            ))
            .unwrap();
        Self {
            connection,
            steps: current,
        }
    }

    fn publish(&self, steps: &str) {
        self.steps.replace(steps.to_owned());
        let changed = [("InstalledSteps", steps.to_variant())]
            .into_iter()
            .collect::<std::collections::HashMap<_, _>>();
        self.connection
            .emit_signal(
                None,
                PATH,
                "org.freedesktop.DBus.Properties",
                "PropertiesChanged",
                Some(&(NAME, changed, Vec::<String>::new()).to_variant()),
            )
            .unwrap();
    }
}

fn spin(ms: u64) {
    let ctx = MainContext::ref_thread_default();
    let end = std::time::Instant::now() + Duration::from_millis(ms);
    while std::time::Instant::now() < end {
        ctx.iteration(false);
        std::thread::sleep(Duration::from_micros(200));
    }
}

#[test]
fn a_restarted_daemon_s_first_values_are_read() {
    MainContext::new().with_thread_default(restarts).unwrap();
}

fn restarts() {
    let Some(bus) = Bus::start() else {
        eprintln!("no dbus-daemon, skipped");
        return;
    };
    let reader = Rc::new(SessionBusDaemon::on_bus(&bus.address));
    let latest = Rc::new(RefCell::new(None::<String>));
    let calls = Rc::new(Cell::new(0u32));
    let on_change: Rc<dyn Fn()> = {
        let (reader, latest, calls) = (reader.clone(), latest.clone(), calls.clone());
        Rc::new(move || {
            calls.set(calls.get() + 1);
            let (reader, latest) = (reader.clone(), latest.clone());
            glib::spawn_future_local(async move {
                let value = reader.installed_steps().await.ok().flatten();
                latest.replace(value);
            });
        })
    };
    let watcher = reader.clone();
    glib::spawn_future_local(async move { watcher.watch(on_change).await });
    spin(200);
    let mut stale = Vec::new();
    for round in 0..12u64 {
        let old = Served::start(&bus.address, EMPTY);
        spin(20);
        MainContext::ref_thread_default()
            .block_on(old.connection.close_future())
            .ok();
        drop(old);
        let delay = 1 + round % 5;
        let fresh = Served::start(&bus.address, EMPTY);
        if delay > 0 {
            spin(delay);
        }
        fresh.publish(FULL);
        spin(150);
        if latest.borrow().as_deref() != Some(FULL) {
            stale.push((round, delay, latest.borrow().clone(), calls.get()));
        }
        MainContext::ref_thread_default()
            .block_on(fresh.connection.close_future())
            .ok();
        spin(50);
    }
    assert!(stale.is_empty(), "stale after a restart: {stale:?}");
}
