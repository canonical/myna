//! The Xfce activation backend against a stand-in xfconfd on a private
//! session bus of its own, one `dbus-daemon` per test: no real Xfce session.
//!
//! The stand-in answers on its own thread, because the backend calls
//! synchronously; the test acts as another xfconf client through the same
//! connection, the way xfce4-keyboard-settings would.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::BufRead;
use std::rc::Rc;

use gio::glib::{MainContext, MainLoop, Variant, VariantTy};
use gio::prelude::*;
use myna_config::platform::xfce::activation::{XfconfActivation, CHANNEL};
use myna_config::platform::xfce::xfconf::Xfconf;
use myna_config::shortcut::TOGGLE_COMMAND;
use myna_platform::activation::{Accelerator, Action, Activation, ActivationError};

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
fn serve(address: &str, ready: std::sync::mpsc::Sender<()>) {
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
struct Bus(std::process::Child);

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn bus() -> (Bus, String) {
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

fn connect_to(address: &str) -> gio::DBusConnection {
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
fn connect() -> (gio::DBusConnection, Bus, std::thread::JoinHandle<()>) {
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
fn on_own_context<T>(test: impl FnOnce() -> T) -> T {
    MainContext::new()
        .with_thread_default(test)
        .expect("own the test's context")
}

fn settle() {
    let context = MainContext::ref_thread_default();
    while context.iteration(false) {}
}

fn key(text: &str) -> Accelerator {
    Accelerator::parse(text).unwrap()
}

fn action(command: &str) -> Action {
    Action {
        name: "Dictation".into(),
        command: command.into(),
    }
}

/// A desktop whose xfsettingsd has made the shortcuts its own.
struct Desktop {
    other: Xfconf,
    activation: XfconfActivation,
    connection: gio::DBusConnection,
    _bus: Bus,
}

fn desktop(customised: bool) -> Desktop {
    let (connection, bus, _) = connect();
    let other = Xfconf::with_connection(connection.clone(), CHANNEL);
    if customised {
        for provider in ["/commands", "/xfwm4"] {
            other
                .set(&format!("{provider}/custom/override"), &true.to_variant())
                .unwrap();
        }
    }
    Desktop {
        activation: XfconfActivation::with_xfconf(Xfconf::with_connection(
            connection.clone(),
            CHANNEL,
        )),
        other,
        connection,
        _bus: bus,
    }
}

impl Desktop {
    fn put(&self, property: &str, value: &str) {
        self.other.set(property, &value.to_variant()).unwrap();
    }

    fn string(&self, property: &str) -> Option<String> {
        self.other
            .get(property)
            .unwrap()
            .and_then(|value| value.str().map(str::to_owned))
    }
}

#[test]
fn a_bound_key_is_a_custom_command_xfsettingsd_runs() {
    on_own_context(|| {
        let desktop = desktop(true);
        desktop
            .activation
            .bind(&key("<Super>j"), &action(TOGGLE_COMMAND))
            .unwrap();
        assert_eq!(
            desktop.string("/commands/custom/<Super>j").as_deref(),
            Some(TOGGLE_COMMAND)
        );
        assert_eq!(
            desktop.activation.binding().unwrap().unwrap().as_str(),
            "<Super>j"
        );
        assert_eq!(
            desktop.activation.command().unwrap().as_deref(),
            Some(TOGGLE_COMMAND)
        );
    });
}

#[test]
fn myna_is_found_by_its_command_not_its_key() {
    on_own_context(|| {
        let desktop = desktop(true);
        desktop.put("/commands/custom/<Super>t", "xfce4-terminal");
        assert_eq!(desktop.activation.binding().unwrap(), None);
        for ours in [
            "/snap/bin/myna.toggle",
            "myna-desktop --toggle",
            TOGGLE_COMMAND,
        ] {
            desktop.put("/commands/custom/<Primary><Alt>d", ours);
            assert_eq!(
                desktop.activation.binding().unwrap().unwrap().as_str(),
                "<Primary><Alt>d",
                "{ours}"
            );
            assert_eq!(desktop.activation.command().unwrap().as_deref(), Some(ours));
        }
        assert!(desktop
            .activation
            .binding()
            .unwrap()
            .unwrap()
            .same_keys("<Control><Alt>d"));
    });
}

#[test]
fn rebinding_drops_the_old_entry_and_its_startup_notification() {
    on_own_context(|| {
        let desktop = desktop(true);
        desktop.put("/commands/custom/<Super>j", "/snap/bin/myna.toggle");
        desktop
            .other
            .set(
                "/commands/custom/<Super>j/startup-notify",
                &true.to_variant(),
            )
            .unwrap();
        desktop
            .activation
            .bind(&key("<Super>k"), &action(TOGGLE_COMMAND))
            .unwrap();
        assert_eq!(desktop.string("/commands/custom/<Super>j"), None);
        assert_eq!(
            desktop
                .other
                .get("/commands/custom/<Super>j/startup-notify")
                .unwrap(),
            None
        );
        assert_eq!(
            desktop.string("/commands/custom/<Super>k").as_deref(),
            Some(TOGGLE_COMMAND)
        );
    });
}

#[test]
fn rebinding_the_same_key_updates_its_command_in_place() {
    on_own_context(|| {
        let desktop = desktop(true);
        desktop.put("/commands/custom/<Super>j", "/snap/bin/myna.toggle");
        desktop
            .activation
            .bind(&key("<Super>j"), &action(TOGGLE_COMMAND))
            .unwrap();
        assert_eq!(
            desktop.string("/commands/custom/<Super>j").as_deref(),
            Some(TOGGLE_COMMAND)
        );
    });
}

#[test]
fn xfsettingsd_ignores_a_custom_tree_it_has_not_adopted() {
    on_own_context(|| {
        let desktop = desktop(false);
        desktop.put("/commands/default/<Super>e", "thunar");
        let bound = desktop
            .activation
            .bind(&key("<Super>j"), &action(TOGGLE_COMMAND));
        assert!(
            matches!(bound, Err(ActivationError::Unavailable(_))),
            "{bound:?}"
        );
        assert_eq!(desktop.string("/commands/custom/<Super>j"), None);
        // The defaults are what is live, so they are what holds a key.
        let held = desktop.activation.conflicts(&key("<Super>e")).unwrap();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].action, "thunar");
        let released = desktop.activation.release(&held[0]);
        assert!(
            matches!(released, Err(ActivationError::Refused(_))),
            "{released:?}"
        );
        assert_eq!(
            desktop.string("/commands/default/<Super>e").as_deref(),
            Some("thunar")
        );
    });
}

#[test]
fn other_commands_and_window_manager_keys_hold_a_key() {
    on_own_context(|| {
        let desktop = desktop(true);
        desktop.put(
            "/commands/custom/<Primary><Alt>t",
            "exo-open --launch TerminalEmulator",
        );
        desktop.put("/xfwm4/custom/<Alt>F4", "close_window_key");
        desktop.put("/commands/custom/<Super>e/startup-notify", "not a key");
        let terminal = desktop
            .activation
            .conflicts(&key("<Control><Alt>t"))
            .unwrap();
        assert_eq!(terminal.len(), 1);
        assert_eq!(terminal[0].action, "exo-open --launch TerminalEmulator");
        assert!(!terminal[0].reserved);
        let close = desktop.activation.conflicts(&key("<Mod1>f4")).unwrap();
        assert_eq!(close.len(), 1);
        assert_eq!(close[0].action, "close window");
        assert!(desktop
            .activation
            .conflicts(&key("<Super>e"))
            .unwrap()
            .is_empty());
    });
}

#[test]
fn releasing_resets_the_holder_and_what_hangs_off_it() {
    on_own_context(|| {
        let desktop = desktop(true);
        desktop.put("/xfwm4/custom/<Alt>F4", "close_window_key");
        desktop.put("/commands/custom/<Super>t", "xfce4-terminal");
        desktop
            .other
            .set(
                "/commands/custom/<Super>t/startup-notify",
                &true.to_variant(),
            )
            .unwrap();
        let held = desktop
            .activation
            .conflicts(&key("<Super>t"))
            .unwrap()
            .remove(0);
        desktop.activation.release(&held).unwrap();
        assert_eq!(desktop.string("/commands/custom/<Super>t"), None);
        assert_eq!(
            desktop
                .other
                .get("/commands/custom/<Super>t/startup-notify")
                .unwrap(),
            None
        );
        assert_eq!(
            desktop.string("/xfwm4/custom/<Alt>F4").as_deref(),
            Some("close_window_key")
        );
    });
}

#[test]
fn the_watch_hears_this_channels_command_changes_only() {
    on_own_context(|| {
        let desktop = desktop(true);
        let heard = Rc::new(RefCell::new(Vec::new()));
        let subscription = desktop.activation.watch(Box::new({
            let heard = Rc::clone(&heard);
            move || heard.borrow_mut().push(())
        }));
        settle();
        desktop.put("/xfwm4/custom/<Alt>F4", "close_window_key");
        settle();
        assert!(
            heard.borrow().is_empty(),
            "the window manager's keys are not Myna's"
        );
        desktop.put("/commands/custom/<Super>j", "/snap/bin/myna.toggle");
        settle();
        assert_eq!(heard.borrow().len(), 1);
        desktop
            .other
            .reset("/commands/custom/<Super>j", true)
            .unwrap();
        settle();
        assert_eq!(heard.borrow().len(), 2);
        drop(subscription);
        desktop.put("/commands/custom/<Super>j", "/snap/bin/myna.toggle");
        settle();
        assert_eq!(heard.borrow().len(), 2);
    });
}

#[test]
fn a_daemon_that_has_gone_is_unavailable_not_empty() {
    on_own_context(|| {
        let (connection, bus, server) = connect();
        let activation =
            XfconfActivation::with_xfconf(Xfconf::with_connection(connection, CHANNEL));
        assert_eq!(activation.binding().unwrap(), None);
        drop(bus);
        server.join().unwrap();
        assert!(matches!(
            activation.binding(),
            Err(ActivationError::Unavailable(_))
        ));
    });
}

#[test]
fn a_session_without_xfconfd_is_unavailable() {
    on_own_context(|| {
        let (_bus, address) = bus();
        let activation =
            XfconfActivation::with_xfconf(Xfconf::with_connection(connect_to(&address), CHANNEL));
        for result in [
            activation.binding().map(drop),
            activation.bind(&key("<Super>j"), &action(TOGGLE_COMMAND)),
        ] {
            assert!(
                matches!(result, Err(ActivationError::Unavailable(_))),
                "{result:?}"
            );
        }
    });
}

mod conformance {
    use super::*;
    use myna_platform::conformance::activation::{run, Fixture};

    struct Xfce {
        desktop: Option<Desktop>,
    }

    impl Xfce {
        fn desktop(&self) -> &Desktop {
            self.desktop.as_ref().expect("set up")
        }
    }

    impl Fixture for Xfce {
        fn setup(&mut self) -> Box<dyn Activation> {
            let desktop = desktop(true);
            let activation = XfconfActivation::with_xfconf(Xfconf::with_connection(
                desktop.connection.clone(),
                CHANNEL,
            ));
            self.desktop = Some(desktop);
            Box::new(activation)
        }

        fn hold(&mut self, accelerator: &Accelerator, reserved: bool) -> bool {
            if reserved {
                return false;
            }
            let (provider, what) = if accelerator.as_str().ends_with('t') {
                ("commands", "xfce4-terminal")
            } else {
                ("xfwm4", "close_window_key")
            };
            self.desktop()
                .put(&format!("/{provider}/custom/{accelerator}"), what);
            true
        }

        fn commands(&self) -> [&'static str; 2] {
            [TOGGLE_COMMAND, "/snap/bin/myna.toggle"]
        }

        fn change_outside(&mut self, binding: Option<&Accelerator>) {
            let desktop = self.desktop();
            for (property, value) in desktop.other.all("/commands/custom").unwrap() {
                if value
                    .str()
                    .is_some_and(myna_config::shortcut::is_toggle_command)
                {
                    desktop.other.reset(&property, true).unwrap();
                }
            }
            if let Some(binding) = binding {
                desktop.put(&format!("/commands/custom/{binding}"), TOGGLE_COMMAND);
            }
        }

        fn settle(&mut self) {
            super::settle();
        }
    }

    #[test]
    fn xfce_passes_the_activation_suite_but_has_no_reserved_keys() {
        on_own_context(|| {
            let report = run(&mut Xfce { desktop: None });
            assert_eq!(report.not_applicable, ["reserved_keys_are_not_released"]);
            assert_eq!(report.passed.len(), 8);
        });
    }
}
