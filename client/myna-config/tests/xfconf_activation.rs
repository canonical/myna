//! The Xfce activation backend against a stand-in xfconfd on a private
//! session bus of its own, one `dbus-daemon` per test: no real Xfce session.
//!
//! The stand-in answers on its own thread, because the backend calls
//! synchronously; the test acts as another xfconf client through the same
//! connection, the way xfce4-keyboard-settings would.

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use common::*;
use gio::prelude::*;
use myna_config::platform::xfce::activation::{XfconfActivation, CHANNEL};
use myna_config::platform::xfce::xfconf::Xfconf;
use myna_config::shortcut::TOGGLE_COMMAND;
use myna_platform::activation::{Accelerator, Action, Activation, ActivationError};

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
fn a_command_holding_a_key_is_named_by_its_desktop_entry() {
    on_own_context(|| {
        let apps = std::env::temp_dir().join(format!("xfconf-apps-{}", std::process::id()));
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::write(
            apps.join("thunar.desktop"),
            "[Desktop Entry]\nType=Application\nName=File Manager\nExec=thunar %U\n",
        )
        .unwrap();
        let mut desktop = desktop(true);
        desktop.activation = XfconfActivation::with_xfconf(Xfconf::with_connection(
            desktop.connection.clone(),
            CHANNEL,
        ))
        .with_applications(vec![apps.clone()]);
        desktop.put("/commands/custom/<Super>e", "thunar");
        desktop.put("/commands/custom/<Super>t", "unknown-tool --flag");
        let held = desktop.activation.conflicts(&key("<Super>e")).unwrap();
        assert_eq!(held[0].action, "File Manager");
        // Releasing still finds the entry by where it is held.
        assert_eq!(held[0].holder, "/commands/custom/<Super>e");
        let held = desktop.activation.conflicts(&key("<Super>t")).unwrap();
        assert_eq!(held[0].action, "unknown-tool --flag");
        std::fs::remove_dir_all(apps).ok();
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
