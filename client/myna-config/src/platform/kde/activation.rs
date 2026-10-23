//! The dictation shortcut as a kglobalaccel command shortcut, the kind
//! Plasma's Shortcuts settings make under "Add Command".
//!
//! A command shortcut is a desktop entry with
//! `X-KDE-GlobalAccel-CommandShortcut=true`; kglobalaccel (inside KWin on
//! Plasma 6) launches it on the `_launch` action of the component named by the
//! entry's file name. Myna's binding is the component whose entry runs a
//! command Myna recognises as its own ([`crate::shortcut::is_toggle_command`]),
//! so a shortcut the user made by hand counts.
//!
//! kglobalaccel keeps the entry it loaded until the session ends, so an edited
//! `Exec` would never run: Myna's file name is derived from its command, and a
//! new command is a new entry. It refuses a key another shortcut holds, and
//! aborts KWin on a key sequence of fewer than four ints, so every key is sent
//! as four.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gio::glib::{self, Variant};
use gio::prelude::*;
use myna_platform::activation::{Accelerator, Action, Activation, ActivationError, Conflict};
use myna_platform::Subscription;

use super::keys;
use crate::platform::applications::{self, Entry};
use crate::platform::dbus::{self, unavailable};
use crate::shortcut::is_toggle_command;

const SERVICE: &str = "org.kde.kglobalaccel";
const PATH: &str = "/kglobalaccel";
const INTERFACE: &str = "org.kde.KGlobalAccel";
const LAUNCH: &str = "_launch";
const PREFIX: &str = "com.canonical.Myna.Toggle-";
/// Twice KSycoca's 1.5 s between looks at the disk.
const REGISTER_PATIENCE: Duration = Duration::from_secs(3);
const REGISTER_POLL: Duration = Duration::from_millis(100);

pub struct KglobalaccelActivation {
    connection: gio::DBusConnection,
    /// Where entries are found, the user's own first: Myna writes there.
    applications: Vec<PathBuf>,
}

/// One of Myna's command shortcuts: its component and the keys it holds.
struct Ours {
    component: String,
    keys: Vec<i32>,
}

impl KglobalaccelActivation {
    /// The session bus's kglobalaccel.
    pub fn open() -> Result<Self, ActivationError> {
        Ok(Self::new(
            dbus::session()?,
            applications::application_dirs(),
        ))
    }

    pub fn new(connection: gio::DBusConnection, applications: Vec<PathBuf>) -> Self {
        Self {
            connection,
            applications,
        }
    }

    fn call(
        &self,
        method: &str,
        parameters: Variant,
        reply: &str,
    ) -> Result<Variant, ActivationError> {
        dbus::call(
            &self.connection,
            SERVICE,
            PATH,
            INTERFACE,
            method,
            parameters,
            reply,
        )
        .map_err(|error| unavailable(method, &error))
    }

    fn entry(&self, component: &str) -> Option<Entry> {
        component
            .ends_with(".desktop")
            .then(|| applications::entry(component, &self.applications))?
    }

    fn is_ours(&self, component: &str) -> bool {
        self.entry(component)
            .is_some_and(|entry| is_toggle_command(&unescape(&entry.exec)))
    }

    /// Every component whose entry runs Myna's toggle, bound or not.
    fn ours(&self) -> Result<Vec<Ours>, ActivationError> {
        let reply = self.call("allMainComponents", ().to_variant(), "(aas)")?;
        let mut found = Vec::new();
        for id in reply.child_value(0).iter() {
            let Some(component) = id.child_value(0).str().map(str::to_owned) else {
                continue;
            };
            if self.is_ours(&component) {
                let keys = self.keys(&component, LAUNCH)?;
                found.push(Ours { component, keys });
            }
        }
        Ok(found)
    }

    /// The first key of each sequence `action` holds.
    fn keys(&self, component: &str, action: &str) -> Result<Vec<i32>, ActivationError> {
        let reply = self.call("shortcutKeys", action_id(component, action, ""), "(a(ai))")?;
        Ok(reply
            .child_value(0)
            .iter()
            .filter_map(|sequence| {
                sequence
                    .child_value(0)
                    .fixed_array::<i32>()
                    .ok()?
                    .first()
                    .copied()
            })
            .filter(|key| *key != 0)
            .collect())
    }

    fn set_keys(&self, component: &str, action: &str, keys: &[i32]) -> Result<(), ActivationError> {
        let sequences: Vec<Variant> = keys
            .iter()
            .map(|key| Variant::tuple_from_iter([[*key, 0, 0, 0].to_variant()]))
            .collect();
        let parameters = Variant::tuple_from_iter([
            action_id(component, action, "").child_value(0),
            Variant::array_from_iter_with_type(glib::VariantTy::new("(ai)").unwrap(), sequences),
        ]);
        self.call("setForeignShortcutKeys", parameters, "()")
            .map(drop)
    }

    fn bound(&self) -> Result<Option<Ours>, ActivationError> {
        Ok(self.ours()?.into_iter().find(|ours| !ours.keys.is_empty()))
    }

    /// Free the component's keys, then remove its entry where the user's own
    /// settings could: kglobalaccel drops a component whose entry is gone.
    fn retire(&self, component: &str) -> Result<(), ActivationError> {
        self.set_keys(component, LAUNCH, &[])?;
        let user = self.applications.first().map(|dir| dir.join(component));
        match user {
            Some(path) if path.is_file() => std::fs::remove_file(&path)
                .map_err(|error| ActivationError::Refused(format!("{}: {error}", path.display()))),
            _ => Ok(()),
        }
    }

    fn write_entry(&self, component: &str, action: &Action) -> Result<(), ActivationError> {
        let dir = self
            .applications
            .first()
            .ok_or_else(|| ActivationError::Unavailable("no applications directory".into()))?;
        let file = glib::KeyFile::new();
        let group = "Desktop Entry";
        file.set_string(group, "Type", "Application");
        file.set_string(group, "Name", &action.name);
        file.set_string(group, "Exec", &action.command.replace('%', "%%"));
        file.set_boolean(group, "NoDisplay", true);
        file.set_boolean(group, "StartupNotify", false);
        file.set_boolean(group, "X-KDE-GlobalAccel-CommandShortcut", true);
        std::fs::create_dir_all(dir)
            .and_then(|()| std::fs::write(dir.join(component), file.to_data().as_bytes()))
            .map_err(|error| ActivationError::Refused(format!("{}: {error}", dir.display())))
    }
}

impl KglobalaccelActivation {
    /// Give `component` a grabbed `_launch`. Registering `_launch` directly
    /// would shadow the one the entry gives a new component, and only that
    /// one is grabbed: Plasma's Shortcuts settings register a placeholder and
    /// drop it, so do the same. kglobalaccel finds an entry through KSycoca,
    /// which looks at the disk at most every 1.5 s, so a new one may take
    /// that long to be found.
    fn register(&self, component: &str, name: &str) -> Result<(), ActivationError> {
        let deadline = Instant::now() + REGISTER_PATIENCE;
        loop {
            self.call(
                "doRegister",
                Variant::tuple_from_iter([[component, "", name, ""].to_variant()]),
                "()",
            )?;
            if self.has_component(component)? {
                break;
            }
            if Instant::now() >= deadline {
                return Err(ActivationError::Unavailable(format!(
                    "kglobalaccel did not find {component}"
                )));
            }
            std::thread::sleep(REGISTER_POLL);
        }
        self.call("unregister", (component, "").to_variant(), "(b)")
            .map(drop)
    }

    fn has_component(&self, component: &str) -> Result<bool, ActivationError> {
        match dbus::call(
            &self.connection,
            SERVICE,
            PATH,
            INTERFACE,
            "getComponent",
            (component,).to_variant(),
            "(o)",
        ) {
            Ok(_) => Ok(true),
            Err(error)
                if gio::DBusError::remote_error(&error).as_deref()
                    == Some("org.kde.kglobalaccel.NoSuchComponent") =>
            {
                Ok(false)
            }
            Err(error) => Err(unavailable("getComponent", &error)),
        }
    }
}

impl Activation for KglobalaccelActivation {
    fn binding(&self) -> Result<Option<Accelerator>, ActivationError> {
        let Some(ours) = self.bound()? else {
            return Ok(None);
        };
        keys::from_qt(ours.keys[0]).map(Some).ok_or_else(|| {
            ActivationError::Unavailable(format!("Myna is bound to Qt key {:#x}", ours.keys[0]))
        })
    }

    fn command(&self) -> Result<Option<String>, ActivationError> {
        Ok(self
            .bound()?
            .and_then(|ours| self.entry(&ours.component))
            .map(|entry| unescape(&entry.exec)))
    }

    /// Myna's other entries go first: kglobalaccel would refuse a key one of
    /// them still holds.
    fn bind(&self, accelerator: &Accelerator, action: &Action) -> Result<(), ActivationError> {
        let key = keys::to_qt(accelerator).ok_or_else(|| {
            ActivationError::Refused(format!("Plasma has no key for {accelerator}"))
        })?;
        let component = component_for(&action.command);
        for stale in self.ours()? {
            if stale.component != component {
                self.retire(&stale.component)?;
            }
        }
        self.write_entry(&component, action)?;
        self.register(&component, &action.name)?;
        self.set_keys(&component, LAUNCH, &[key])?;
        if self.keys(&component, LAUNCH)? != [key] {
            return Err(ActivationError::Refused(format!(
                "kglobalaccel did not take {accelerator}"
            )));
        }
        Ok(())
    }

    fn clear(&self) -> Result<(), ActivationError> {
        for ours in self.ours()? {
            self.retire(&ours.component)?;
        }
        Ok(())
    }

    /// Nothing is reserved: kglobalaccel gives up any key, KWin's included.
    fn conflicts(&self, accelerator: &Accelerator) -> Result<Vec<Conflict>, ActivationError> {
        let Some(key) = keys::to_qt(accelerator) else {
            return Ok(Vec::new());
        };
        let parameters = Variant::tuple_from_iter([
            Variant::tuple_from_iter([[key, 0, 0, 0].to_variant()]),
            Variant::tuple_from_iter([0_i32.to_variant()]),
        ]);
        let reply = self.call("globalShortcutsByKey", parameters, "(a(ssssssaiai))")?;
        let mut found = Vec::new();
        for info in reply.child_value(0).iter() {
            let field = |i| info.child_value(i).str().unwrap_or_default().to_owned();
            let (action, action_name, component, component_name) =
                (field(0), field(1), field(2), field(3));
            if self.is_ours(&component) {
                continue;
            }
            found.push(Conflict {
                action: if action == LAUNCH {
                    component_name
                } else {
                    action_name
                },
                reserved: false,
                holder: format!("{component}\t{action}\t{key}"),
            });
        }
        Ok(found)
    }

    fn release(&self, conflict: &Conflict) -> Result<(), ActivationError> {
        let mut fields = conflict.holder.split('\t');
        let (Some(component), Some(action), Some(Ok(key))) = (
            fields.next(),
            fields.next(),
            fields.next().map(str::parse::<i32>),
        ) else {
            return Err(ActivationError::Refused(format!(
                "{:?} is not a kglobalaccel shortcut",
                conflict.holder
            )));
        };
        let mut keys = self.keys(component, action)?;
        keys.retain(|held| *held != key);
        self.set_keys(component, action, &keys)
    }

    /// kglobalaccel tells of key changes made through it; an entry removed in
    /// Plasma's settings changes only the applications directory.
    fn watch(&self, changed: Box<dyn Fn()>) -> Subscription {
        let changed: std::rc::Rc<dyn Fn()> = changed.into();
        let signal = self.connection.subscribe_to_signal(
            None,
            Some(INTERFACE),
            Some("yourShortcutsChanged"),
            Some(PATH),
            None,
            gio::DBusSignalFlags::NONE,
            {
                let changed = changed.clone();
                move |_| changed()
            },
        );
        let monitor = self.applications.first().and_then(|dir| {
            let monitor = gio::File::for_path(dir)
                .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
                .ok()?;
            monitor.connect_changed(move |_, file, _, _| {
                if file
                    .basename()
                    .is_some_and(|name| name.extension().is_some_and(|ext| ext == "desktop"))
                {
                    changed();
                }
            });
            Some(monitor)
        });
        Subscription::new(move || {
            drop(signal);
            if let Some(monitor) = monitor {
                monitor.cancel();
            }
        })
    }
}

/// kglobalaccel's four-part action id: component, action, and their names.
fn action_id(component: &str, action: &str, name: &str) -> Variant {
    Variant::tuple_from_iter([[component, action, name, name].to_variant()])
}

/// The desktop file ID of the entry that runs `command`: one per command, so
/// an entry kglobalaccel loaded never needs its `Exec` changed.
fn component_for(command: &str) -> String {
    // FNV-1a: stable across builds, unlike std's hasher.
    let hash = command.bytes().fold(0x811c_9dc5_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    });
    format!("{PREFIX}{hash:08x}.desktop")
}

/// An `Exec` as the command it runs: `%%` is a literal `%`.
fn unescape(exec: &str) -> String {
    exec.replace("%%", "%")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_command_has_an_entry_of_its_own() {
        let toggle = component_for(crate::shortcut::TOGGLE_COMMAND);
        assert_eq!(toggle, component_for(crate::shortcut::TOGGLE_COMMAND));
        assert!(toggle.starts_with(PREFIX) && toggle.ends_with(".desktop"));
        assert_ne!(toggle, component_for("/snap/bin/myna.toggle"));
        assert_eq!(component_for(""), format!("{PREFIX}811c9dc5.desktop"));
    }
}
