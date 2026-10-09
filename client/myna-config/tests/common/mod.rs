//! A stand-in xfconfd on a private session bus, shared by the Xfce backends'
//! tests. The stand-in answers on its own thread, because the backends call
//! synchronously; the test acts as another xfconf client through the same
//! connection.
#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::BufRead;
use std::rc::Rc;

use gio::glib::{MainContext, MainLoop, Variant, VariantTy};
use gio::prelude::*;

const XML: &str = "<node>\
  <interface name='org.xfce.Xfconf'>\
    <method name='GetProperty'>\
      <arg type='s' name='channel' direction='in'/>\
      <arg type='s' name='property' direction='in'/>\
      <arg type='v' name='value' direction='out'/>\
    </method>\
    <method name='SetProperty'>\
      <arg type='s' name='channel' direction='in'/>\
      <arg type='s' name='property' direction='in'/>\
      <arg type='v' name='value' direction='in'/>\
    </method>\
    <method name='GetAllProperties'>\
      <arg type='s' name='channel' direction='in'/>\
      <arg type='s' name='property_base' direction='in'/>\
      <arg type='a{sv}' name='properties' direction='out'/>\
    </method>\
    <method name='ResetProperty'>\
      <arg type='s' name='channel' direction='in'/>\
      <arg type='s' name='property' direction='in'/>\
      <arg type='b' name='recursive' direction='in'/>\
    </method>\
  </interface>\
</node>";
const NOT_FOUND: &str = "org.xfce.Xfconf.Error.PropertyNotFound";

type Store = Rc<RefCell<BTreeMap<String, Variant>>>;

/// Serve xfconf's methods on the bus at `address` until it goes.
pub fn serve(address: &str, ready: std::sync::mpsc::Sender<()>) {
    let context = MainContext::new();
    context
        .with_thread_default(|| {
            let connection = connect_to(address);
            let store = Store::default();
            let interface = gio::DBusNodeInfo::for_xml(XML)
                .unwrap()
                .lookup_interface("org.xfce.Xfconf")
                .unwrap();
            let emitter = connection.clone();
            connection
                .register_object("/org/xfce/Xfconf", &interface)
                .method_call(move |_, _, _, _, method, parameters, invocation| {
                    let channel = parameters
                        .child_value(0)
                        .str()
                        .unwrap_or_default()
                        .to_owned();
                    let property = parameters
                        .child_value(1)
                        .str()
                        .unwrap_or_default()
                        .to_owned();
                    let mut store = store.borrow_mut();
                    let emit = |name: &str, body: Variant| {
                        emitter
                            .emit_signal(
                                None,
                                "/org/xfce/Xfconf",
                                "org.xfce.Xfconf",
                                name,
                                Some(&body),
                            )
                            .unwrap();
                    };
                    match method {
                        "GetProperty" => match store.get(&property) {
                            Some(value) => {
                                invocation.return_value(Some(&Variant::tuple_from_iter([
                                    Variant::from_variant(value),
                                ])))
                            }
                            None => invocation.return_dbus_error(NOT_FOUND, "no such property"),
                        },
                        "SetProperty" => {
                            let value = parameters.child_value(2).as_variant().unwrap();
                            store.insert(property.clone(), value.clone());
                            emit(
                                "PropertyChanged",
                                Variant::tuple_from_iter([
                                    channel.to_variant(),
                                    property.to_variant(),
                                    Variant::from_variant(&value),
                                ]),
                            );
                            invocation.return_value(None);
                        }
                        "GetAllProperties" => {
                            let below = format!("{property}/");
                            let root = property.is_empty() || property == "/";
                            let found: Vec<Variant> = store
                                .iter()
                                .filter(|(path, _)| {
                                    root || **path == property || path.starts_with(&below)
                                })
                                .map(|(path, value)| {
                                    Variant::from_dict_entry(
                                        &path.to_variant(),
                                        &Variant::from_variant(value),
                                    )
                                })
                                .collect();
                            if found.is_empty() && !root {
                                invocation.return_dbus_error(NOT_FOUND, "no such property");
                            } else {
                                let array = Variant::array_from_iter_with_type(
                                    VariantTy::new("{sv}").unwrap(),
                                    found,
                                );
                                invocation.return_value(Some(&Variant::tuple_from_iter([array])));
                            }
                        }
                        "ResetProperty" => {
                            let recursive = parameters.child_value(2).get::<bool>().unwrap();
                            let below = format!("{property}/");
                            let gone: Vec<String> = store
                                .keys()
                                .filter(|path| {
                                    **path == property || (recursive && path.starts_with(&below))
                                })
                                .cloned()
                                .collect();
                            if gone.is_empty() {
                                invocation.return_dbus_error(NOT_FOUND, "no such property");
                                return;
                            }
                            for path in gone {
                                store.remove(&path);
                                emit(
                                    "PropertyRemoved",
                                    Variant::tuple_from_iter([
                                        channel.to_variant(),
                                        path.to_variant(),
                                    ]),
                                );
                            }
                            invocation.return_value(None);
                        }
                        other => unreachable!("{other}"),
                    }
                })
                .build()
                .expect("register the stand-in xfconfd");
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
                    Some(&("org.xfce.Xfconf", 0u32).to_variant()),
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

/// A session bus that lives as long as the test.
pub struct Bus(std::process::Child);

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn bus() -> (Bus, String) {
    let mut child = std::process::Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("dbus-daemon is installed where the suites run");
    let mut address = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut address)
        .unwrap();
    (Bus(child), address.trim().to_owned())
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

/// The client end of a stand-in xfconfd on a fresh bus. The server thread
/// ends when the bus does.
pub fn connect() -> (gio::DBusConnection, Bus, std::thread::JoinHandle<()>) {
    let (bus, address) = bus();
    let (ready, owned) = std::sync::mpsc::channel();
    let thread = std::thread::spawn({
        let address = address.clone();
        move || serve(&address, ready)
    });
    owned.recv().expect("the stand-in owns org.xfce.Xfconf");
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
