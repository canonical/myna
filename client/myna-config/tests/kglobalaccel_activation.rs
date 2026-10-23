//! The KDE activation backend against a stand-in kglobalaccel on a private
//! session bus, one `dbus-daemon` per test, and an applications directory of
//! its own: no Plasma session. The test acts as Plasma's Shortcuts settings
//! through the same connection.

mod common;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use common::kglobalaccel::State;
use common::*;
use gio::glib::{Variant, VariantTy};
use gio::prelude::*;
use myna_config::platform::kde::activation::KglobalaccelActivation;
use myna_config::shortcut::TOGGLE_COMMAND;
use myna_platform::activation::{Accelerator, Action, Activation, ActivationError};

const LEGACY: &str = "/snap/bin/myna.toggle";
const SUPER_J: i32 = 0x1000_0000 | 0x4a;
const SUPER_K: i32 = 0x1000_0000 | 0x4b;
const ALT_F4: i32 = 0x0800_0000 | 0x0100_0033;

fn key(text: &str) -> Accelerator {
    Accelerator::parse(text).unwrap()
}

fn action(command: &str) -> Action {
    Action {
        name: "Dictation".into(),
        command: command.into(),
    }
}

struct Desktop {
    activation: KglobalaccelActivation,
    connection: gio::DBusConnection,
    state: Arc<Mutex<State>>,
    apps: PathBuf,
    _bus: Bus,
}

impl Drop for Desktop {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.apps).ok();
    }
}

fn desktop() -> Desktop {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let (connection, bus, _, state) = kglobalaccel::connect();
    let apps = std::env::temp_dir().join(format!(
        "kglobalaccel-apps-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    Desktop {
        activation: KglobalaccelActivation::new(connection.clone(), vec![apps.clone()]),
        connection,
        state,
        apps,
        _bus: bus,
    }
}

impl Desktop {
    fn call(&self, method: &str, parameters: Variant) {
        self.connection
            .call_sync(
                Some("org.kde.kglobalaccel"),
                "/kglobalaccel",
                "org.kde.KGlobalAccel",
                method,
                Some(&parameters),
                None,
                gio::DBusCallFlags::NONE,
                2_000,
                gio::Cancellable::NONE,
            )
            .unwrap();
    }

    fn set_keys(&self, component: &str, action: &str, keys: &[i32]) {
        let sequences = Variant::array_from_iter_with_type(
            VariantTy::new("(ai)").unwrap(),
            keys.iter()
                .map(|key| Variant::tuple_from_iter([[*key, 0, 0, 0].to_variant()])),
        );
        self.call(
            "setForeignShortcutKeys",
            Variant::tuple_from_iter([vec![component, action, "", ""].to_variant(), sequences]),
        );
    }

    /// Another shortcut, as an application registers it.
    fn hold(&self, component: &str, action: &str, name: &str, keys: &[i32]) {
        self.call(
            "doRegister",
            (vec![component, action, name, name],).to_variant(),
        );
        self.set_keys(component, action, keys);
    }

    /// A command shortcut made in Plasma's Shortcuts settings.
    fn add_command(&self, id: &str, command: &str, keys: &[i32]) {
        std::fs::create_dir_all(&self.apps).unwrap();
        std::fs::write(
            self.apps.join(id),
            format!(
                "[Desktop Entry]\nType=Application\nName=Mine\nExec={command}\n\
                 X-KDE-GlobalAccel-CommandShortcut=true\n"
            ),
        )
        .unwrap();
        self.call("doRegister", (vec![id, "", "Mine", ""],).to_variant());
        self.call("unregister", (id, "").to_variant());
        self.set_keys(id, "_launch", keys);
    }

    fn launch(&self, component: &str) -> common::kglobalaccel::Action {
        self.state
            .lock()
            .unwrap()
            .action(component, "_launch")
            .cloned()
            .expect("registered")
    }

    /// Myna's entries in the applications directory.
    fn entries(&self) -> Vec<(String, String)> {
        let Ok(dir) = std::fs::read_dir(&self.apps) else {
            return Vec::new();
        };
        let mut found: Vec<_> = dir
            .flatten()
            .map(|entry| {
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    std::fs::read_to_string(entry.path()).unwrap(),
                )
            })
            .collect();
        found.sort();
        found
    }
}

#[test]
fn a_bound_key_is_a_command_shortcut_kglobalaccel_grabs() {
    on_own_context(|| {
        let desktop = desktop();
        desktop
            .activation
            .bind(&key("<Super>j"), &action(TOGGLE_COMMAND))
            .unwrap();
        let entries = desktop.entries();
        assert_eq!(entries.len(), 1);
        let (id, text) = &entries[0];
        assert!(id.starts_with("com.canonical.Myna.Toggle-"), "{id}");
        for line in [
            "Type=Application",
            "Name=Dictation",
            &format!("Exec={TOGGLE_COMMAND}"),
            "NoDisplay=true",
            "X-KDE-GlobalAccel-CommandShortcut=true",
        ] {
            assert!(text.lines().any(|l| l == line), "{line} in {text}");
        }
        let launch = desktop.launch(id);
        assert_eq!(launch.keys, [SUPER_J]);
        assert!(launch.present, "the key would never fire");
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
fn a_new_entry_is_registered_once_kglobalaccel_has_seen_it() {
    on_own_context(|| {
        let desktop = desktop();
        desktop.state.lock().unwrap().unseen = 3;
        desktop
            .activation
            .bind(&key("<Super>j"), &action(TOGGLE_COMMAND))
            .unwrap();
        let id = desktop.entries().remove(0).0;
        assert_eq!(desktop.launch(&id).keys, [SUPER_J]);
        assert!(desktop.launch(&id).present);
    });
}

#[test]
fn an_entry_kglobalaccel_never_sees_is_unavailable_in_bounded_time() {
    on_own_context(|| {
        let desktop = desktop();
        desktop.state.lock().unwrap().unseen = u32::MAX;
        let started = std::time::Instant::now();
        let bound = desktop
            .activation
            .bind(&key("<Super>j"), &action(TOGGLE_COMMAND));
        assert!(
            matches!(bound, Err(ActivationError::Unavailable(_))),
            "{bound:?}"
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(4));
    });
}

#[test]
fn a_command_shortcut_made_in_plasmas_settings_is_mynas() {
    on_own_context(|| {
        let desktop = desktop();
        desktop.add_command("net.local.myna.toggle.desktop", LEGACY, &[SUPER_K]);
        assert_eq!(
            desktop.activation.binding().unwrap().unwrap().as_str(),
            "<Super>k"
        );
        assert_eq!(
            desktop.activation.command().unwrap().as_deref(),
            Some(LEGACY)
        );
        assert!(desktop
            .activation
            .conflicts(&key("<Super>k"))
            .unwrap()
            .is_empty());
        // Binding Myna's own key takes the place of the hand-made one.
        desktop
            .activation
            .bind(&key("<Super>k"), &action(TOGGLE_COMMAND))
            .unwrap();
        assert!(desktop
            .launch("net.local.myna.toggle.desktop")
            .keys
            .is_empty());
        let entries = desktop.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(desktop.launch(&entries[0].0).keys, [SUPER_K]);
    });
}

#[test]
fn a_new_command_is_a_new_entry_because_kglobalaccel_keeps_the_old_one() {
    on_own_context(|| {
        let desktop = desktop();
        desktop
            .activation
            .bind(&key("<Super>j"), &action(LEGACY))
            .unwrap();
        let old = desktop.entries().remove(0).0;
        desktop
            .activation
            .bind(&key("<Super>j"), &action(TOGGLE_COMMAND))
            .unwrap();
        let entries = desktop.entries();
        assert_eq!(entries.len(), 1);
        assert_ne!(entries[0].0, old);
        assert!(desktop.launch(&old).keys.is_empty());
        assert_eq!(desktop.launch(&entries[0].0).keys, [SUPER_J]);
        // The same command again rewrites nothing kglobalaccel has loaded.
        desktop
            .activation
            .bind(&key("<Super>k"), &action(TOGGLE_COMMAND))
            .unwrap();
        assert_eq!(desktop.entries()[0].0, entries[0].0);
        assert_eq!(desktop.launch(&entries[0].0).keys, [SUPER_K]);
    });
}

#[test]
fn a_key_another_shortcut_holds_is_refused_until_released() {
    on_own_context(|| {
        let desktop = desktop();
        desktop.hold("kwin", "Window Close", "Close Window", &[ALT_F4, SUPER_K]);
        let bound = desktop
            .activation
            .bind(&key("<Alt>F4"), &action(TOGGLE_COMMAND));
        assert!(
            matches!(bound, Err(ActivationError::Refused(_))),
            "{bound:?}"
        );
        let held = desktop.activation.conflicts(&key("<Mod1>f4")).unwrap();
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].action, "Close Window");
        assert!(!held[0].reserved);
        desktop.activation.release(&held[0]).unwrap();
        // Only that key: KWin keeps the action's other one.
        assert_eq!(
            desktop
                .state
                .lock()
                .unwrap()
                .action("kwin", "Window Close")
                .unwrap()
                .keys,
            [SUPER_K]
        );
        desktop
            .activation
            .bind(&key("<Alt>F4"), &action(TOGGLE_COMMAND))
            .unwrap();
    });
}

#[test]
fn an_application_holding_a_key_is_named_by_its_component() {
    on_own_context(|| {
        let desktop = desktop();
        desktop.hold("org.kde.konsole.desktop", "_launch", "Konsole", &[SUPER_J]);
        let held = desktop.activation.conflicts(&key("<Super>j")).unwrap();
        assert_eq!(held[0].action, "Konsole");
    });
}

#[test]
fn a_key_qt_cannot_name_is_refused_and_held_by_nothing() {
    on_own_context(|| {
        let desktop = desktop();
        let bound = desktop
            .activation
            .bind(&key("<Hyper>j"), &action(TOGGLE_COMMAND));
        assert!(
            matches!(bound, Err(ActivationError::Refused(_))),
            "{bound:?}"
        );
        assert!(desktop.entries().is_empty());
        assert!(desktop
            .activation
            .conflicts(&key("<Hyper>j"))
            .unwrap()
            .is_empty());
    });
}

#[test]
fn clearing_frees_the_key_and_removes_the_entry() {
    on_own_context(|| {
        let desktop = desktop();
        desktop
            .activation
            .bind(&key("<Super>j"), &action(TOGGLE_COMMAND))
            .unwrap();
        let id = desktop.entries().remove(0).0;
        desktop.activation.clear().unwrap();
        assert!(desktop.launch(&id).keys.is_empty());
        assert!(desktop.entries().is_empty());
        assert_eq!(desktop.activation.binding().unwrap(), None);
    });
}

#[test]
fn the_watch_hears_an_entry_removed_in_plasmas_settings() {
    on_own_context(|| {
        let desktop = desktop();
        desktop
            .activation
            .bind(&key("<Super>j"), &action(TOGGLE_COMMAND))
            .unwrap();
        let heard = Rc::new(RefCell::new(0));
        let _watch = desktop.activation.watch(Box::new({
            let heard = Rc::clone(&heard);
            move || *heard.borrow_mut() += 1
        }));
        settle();
        let id = desktop.entries().remove(0).0;
        std::fs::remove_file(desktop.apps.join(id)).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while *heard.borrow() == 0 && std::time::Instant::now() < deadline {
            gio::glib::MainContext::ref_thread_default().iteration(true);
        }
        assert!(*heard.borrow() > 0);
        assert_eq!(desktop.activation.binding().unwrap(), None);
    });
}

#[test]
fn a_session_without_kglobalaccel_is_unavailable() {
    on_own_context(|| {
        let (_bus, address) = bus();
        let activation = KglobalaccelActivation::new(connect_to(&address), Vec::new());
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

    struct Kde {
        desktop: Option<Desktop>,
    }

    impl Kde {
        fn desktop(&self) -> &Desktop {
            self.desktop.as_ref().expect("set up")
        }
    }

    impl Fixture for Kde {
        fn setup(&mut self) -> Box<dyn Activation> {
            let desktop = desktop();
            let activation =
                KglobalaccelActivation::new(desktop.connection.clone(), vec![desktop.apps.clone()]);
            self.desktop = Some(desktop);
            Box::new(activation)
        }

        fn hold(&mut self, accelerator: &Accelerator, reserved: bool) -> bool {
            if reserved {
                return false;
            }
            let code = myna_config::platform::kde::keys::to_qt(accelerator).unwrap();
            self.desktop()
                .hold("kwin", &format!("held {code}"), "Something", &[code]);
            true
        }

        fn commands(&self) -> [&'static str; 2] {
            [TOGGLE_COMMAND, LEGACY]
        }

        /// The user sets Myna's key in Plasma's Shortcuts settings.
        fn change_outside(&mut self, binding: Option<&Accelerator>) {
            let desktop = self.desktop();
            let keys: Vec<i32> = binding
                .map(|b| myna_config::platform::kde::keys::to_qt(b).unwrap())
                .into_iter()
                .collect();
            for (id, _) in desktop.entries() {
                desktop.set_keys(&id, "_launch", &keys);
            }
        }

        fn settle(&mut self) {
            super::settle();
        }
    }

    #[test]
    fn kde_passes_the_activation_suite_but_has_no_reserved_keys() {
        on_own_context(|| {
            let report = run(&mut Kde { desktop: None });
            assert_eq!(report.not_applicable, ["reserved_keys_are_not_released"]);
            assert_eq!(report.passed.len(), 8);
        });
    }
}
