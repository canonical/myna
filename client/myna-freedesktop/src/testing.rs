//! Stand-in desktop services on a private session bus, one `dbus-daemon` per
//! test. A stand-in answers on its own thread, because the backends call
//! synchronously; the test acts as another client through the same
//! connection.

use std::io::BufRead;

use gio::glib::{MainContext, MainLoop};
use gio::prelude::*;

/// A session bus that lives as long as the test.
pub struct Bus(std::process::Child, std::path::PathBuf);

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        let _ = std::fs::remove_file(&self.1);
    }
}

/// The session bus' policy with no service directories: a name nobody owns
/// stays unowned, rather than starting the host's real service.
const CONFIG: &str = "<busconfig>\
  <type>session</type>\
  <listen>unix:tmpdir=/tmp</listen>\
  <auth>EXTERNAL</auth>\
  <policy context='default'>\
    <allow send_destination='*' eavesdrop='true'/>\
    <allow eavesdrop='true'/>\
    <allow own='*'/>\
  </policy>\
</busconfig>";

pub fn bus() -> (Bus, String) {
    static BUSES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let config = std::env::temp_dir().join(format!(
        "myna-test-bus-{}-{}.conf",
        std::process::id(),
        BUSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::write(&config, CONFIG).unwrap();
    let mut child = std::process::Command::new("dbus-daemon")
        .arg(format!("--config-file={}", config.display()))
        .args(["--nofork", "--print-address"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("dbus-daemon is installed where the suites run");
    let mut address = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut address)
        .unwrap();
    (Bus(child, config), address.trim().to_owned())
}

pub fn connect_to(address: &str) -> gio::DBusConnection {
    MainContext::ref_thread_default()
        .block_on(gio::DBusConnection::for_address_future(
            address,
            gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
                | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
            None,
        ))
        .expect("connect to the private bus")
}

/// Own `name` on the bus at `address`, with the objects `register` puts on
/// the connection, until the bus goes.
pub fn serve_as(
    address: &str,
    name: &str,
    ready: std::sync::mpsc::Sender<()>,
    register: impl FnOnce(&gio::DBusConnection),
) {
    let context = MainContext::new();
    context
        .with_thread_default(|| {
            let connection = connect_to(address);
            register(&connection);
            let main_loop = MainLoop::new(Some(&context), false);
            connection.connect_closed({
                let main_loop = main_loop.clone();
                move |_, _, _| main_loop.quit()
            });
            connection
                .call_sync(
                    Some("org.freedesktop.DBus"),
                    "/org/freedesktop/DBus",
                    "org.freedesktop.DBus",
                    "RequestName",
                    Some(&(name, 0u32).to_variant()),
                    None,
                    gio::DBusCallFlags::NONE,
                    5_000,
                    gio::Cancellable::NONE,
                )
                .expect("own the name");
            ready.send(()).unwrap();
            main_loop.run();
        })
        .expect("own the server's context");
}

/// The client end of the stand-in `serve` runs on a fresh bus. The server
/// thread ends when the bus does.
pub fn connect_with(
    serve: impl FnOnce(&str, std::sync::mpsc::Sender<()>) + Send + 'static,
) -> (gio::DBusConnection, Bus, std::thread::JoinHandle<()>) {
    let (bus, address) = bus();
    let (ready, owned) = std::sync::mpsc::channel();
    let thread = std::thread::spawn({
        let address = address.clone();
        move || serve(&address, ready)
    });
    owned.recv().expect("the stand-in owns its name");
    (connect_to(&address), bus, thread)
}

/// Everything on one context per test, which the signals are delivered to.
pub fn on_own_context<T>(test: impl FnOnce() -> T) -> T {
    MainContext::new()
        .with_thread_default(test)
        .expect("own the test's context")
}

pub fn settle() {
    let context = MainContext::ref_thread_default();
    while context.iteration(false) {}
}
