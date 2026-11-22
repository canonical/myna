//! The daemon adapter over a real (private) D-Bus: `State` and
//! `InstalledSteps` read through `GetAll`, `RescanModels` called, and a
//! missing daemon answered as not running.

use std::cell::{Cell, RefCell};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;

use gio::glib::variant::ToVariant;
use gtk4::glib::MainContext;
use myna_config::adapters::dictation_daemon::SessionBusDaemon;
use myna_config::ports::DictationDaemon;

const NAME: &str = "com.canonical.Myna.Dictation";
const PATH: &str = "/com/canonical/Myna/Dictation";
const XML: &str = r#"<node><interface name="com.canonical.Myna.Dictation">
<method name="RescanModels"/>
<property name="State" type="s" access="read"/>
<property name="InstalledSteps" type="s" access="read"/>
</interface></node>"#;

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

fn serve(address: &str, state: &str, steps: &str, rescans: Rc<Cell<u32>>) -> gio::DBusConnection {
    let ctx = MainContext::ref_thread_default();
    let connection = ctx
        .block_on(gio::DBusConnection::for_address_future(
            address,
            gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
                | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
            None,
        ))
        .unwrap();
    let node = gio::DBusNodeInfo::for_xml(XML).unwrap();
    let interface = node.lookup_interface(NAME).unwrap();
    let (state, steps) = (state.to_owned(), steps.to_owned());
    connection
        .register_object(PATH, &interface)
        .method_call(move |_, _, _, _, _, _, invocation| {
            rescans.set(rescans.get() + 1);
            if rescans.get() > 1 {
                invocation.return_error(gio::DBusError::Failed, "cannot rescan");
            } else {
                invocation.return_value(None);
            }
        })
        .property(move |_, _, _, _, name| match name {
            "State" => state.to_variant(),
            _ => steps.to_variant(),
        })
        .build()
        .unwrap();
    ctx.block_on(connection.call_future(
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
    connection
}

#[test]
fn the_adapter_reads_state_and_models_and_asks_for_a_rescan() {
    MainContext::new()
        .with_thread_default(reads_state_and_models)
        .unwrap();
}

fn reads_state_and_models() {
    let Some(bus) = Bus::start() else {
        eprintln!("no dbus-daemon, skipped");
        return;
    };
    let rescans = Rc::new(Cell::new(0));
    let _server = serve(
        &bus.address,
        "recording",
        r#"{"models":[{"model":"bergamot-en-fr"}],"steps":[]}"#,
        rescans.clone(),
    );
    let daemon = SessionBusDaemon::on_bus(&bus.address);
    let seen = RefCell::new(None);
    // The server's replies need the main context turning while the client waits.
    MainContext::ref_thread_default().block_on(async {
        let state = daemon.state().await;
        let offered = daemon.offered_models().await;
        let rescan = daemon.rescan().await;
        seen.replace(Some((state, offered, rescan)));
    });
    let (state, offered, rescan) = seen.take().unwrap();
    assert_eq!(state, Ok(Some("recording".to_owned())));
    assert_eq!(offered, Ok(Some(vec!["bergamot-en-fr".to_owned()])));
    assert_eq!(rescan, Ok(()));
    assert_eq!(rescans.get(), 1);
    let failed = MainContext::ref_thread_default().block_on(daemon.rescan());
    assert!(
        failed.as_ref().is_err_and(|e| e.contains("cannot rescan")),
        "a refusal is an error, not a missing daemon: {failed:?}"
    );
}

#[test]
fn no_daemon_on_the_bus_is_not_running_rather_than_an_error() {
    MainContext::new().with_thread_default(no_daemon).unwrap();
}

fn no_daemon() {
    let Some(bus) = Bus::start() else {
        eprintln!("no dbus-daemon, skipped");
        return;
    };
    let daemon = SessionBusDaemon::on_bus(&bus.address);
    let (state, offered, rescan) = MainContext::ref_thread_default().block_on(async {
        (
            daemon.state().await,
            daemon.offered_models().await,
            daemon.rescan().await,
        )
    });
    assert_eq!(state, Ok(None));
    assert_eq!(offered, Ok(None));
    assert_eq!(rescan, Ok(()));
}
