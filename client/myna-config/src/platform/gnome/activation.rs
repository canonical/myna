//! The GNOME custom shortcut that toggles dictation. The same entry the snap's
//! `myna.install-shortcut` writes.

use gio::glib;
use gio::prelude::*;
use myna_platform::activation::{Accelerator, Action, Activation, ActivationError, Conflict};
use myna_platform::Subscription;

const MEDIA_KEYS: &str = "org.gnome.settings-daemon.plugins.media-keys";
const CUSTOM_KEYBINDING: &str = "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding";
const LIST_KEY: &str = "custom-keybindings";
const PATH: &str = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/myna/";
/// Where the desktop keeps the shortcuts it handles itself, as GNOME
/// Settings' Keyboard panel lists them.
const KEYBINDING_SCHEMAS: [&str; 5] = [
    "org.gnome.desktop.wm.keybindings",
    "org.gnome.mutter.keybindings",
    "org.gnome.mutter.wayland.keybindings",
    "org.gnome.shell.keybindings",
    MEDIA_KEYS,
];
/// Stands for "no relocatable path" in a conflict's holder.
const NO_PATH: &str = "-";

pub struct GnomeActivation {
    source: gio::SettingsSchemaSource,
    backend: Option<gio::SettingsBackend>,
    list: gio::Settings,
    entry: gio::Settings,
}

impl GnomeActivation {
    /// `None` off GNOME, where there is no media-keys schema.
    pub fn open() -> Option<Self> {
        Self::open_with(&gio::SettingsSchemaSource::default()?, None)
    }

    pub fn open_with(
        source: &gio::SettingsSchemaSource,
        backend: Option<&gio::SettingsBackend>,
    ) -> Option<Self> {
        let list = source.lookup(MEDIA_KEYS, true)?;
        let entry = source.lookup(CUSTOM_KEYBINDING, true)?;
        Some(Self {
            source: source.clone(),
            backend: backend.cloned(),
            list: gio::Settings::new_full(&list, backend, None),
            entry: gio::Settings::new_full(&entry, backend, Some(PATH)),
        })
    }

    fn listed(&self) -> bool {
        self.list.strv(LIST_KEY).iter().any(|path| path == PATH)
    }

    fn set_list(&self, paths: Vec<String>) -> Result<(), ActivationError> {
        self.list.set_strv(LIST_KEY, paths).map_err(refused)
    }

    fn desktop_conflicts(&self, accelerator: &Accelerator) -> Vec<Conflict> {
        let mut found = Vec::new();
        for id in KEYBINDING_SCHEMAS {
            let Some(schema) = self.source.lookup(id, true) else {
                continue;
            };
            let settings = gio::Settings::new_full(&schema, self.backend.as_ref(), None);
            for name in schema.list_keys() {
                let key = schema.key(&name);
                if name == LIST_KEY || key.value_type().as_str() != "as" {
                    continue;
                }
                let Some(held) = settings
                    .strv(name.as_str())
                    .iter()
                    .find(|held| accelerator.same_keys(held))
                    .map(|held| held.to_string())
                else {
                    continue;
                };
                found.push(Conflict {
                    action: key
                        .summary()
                        .map_or_else(|| name.to_string(), |summary| summary.to_string()),
                    // gsd-media-keys grabs a `-static` key once, at login,
                    // and keeps it until logout whatever the setting says.
                    reserved: name.ends_with("-static"),
                    holder: holder(id, NO_PATH, &name, &held),
                });
            }
        }
        found
    }

    fn custom_conflicts(&self, accelerator: &Accelerator) -> Vec<Conflict> {
        let Some(schema) = self.source.lookup(CUSTOM_KEYBINDING, true) else {
            return Vec::new();
        };
        self.list
            .strv(LIST_KEY)
            .iter()
            .filter(|path| *path != PATH)
            .filter_map(|path| {
                let entry = gio::Settings::new_full(&schema, self.backend.as_ref(), Some(path));
                let held = entry.string("binding");
                accelerator.same_keys(&held).then(|| Conflict {
                    action: entry.string("name").to_string(),
                    reserved: false,
                    holder: holder(CUSTOM_KEYBINDING, path, "binding", &held),
                })
            })
            .collect()
    }
}

fn refused(error: glib::BoolError) -> ActivationError {
    ActivationError::Refused(error.to_string())
}

fn holder(schema: &str, path: &str, key: &str, held: &str) -> String {
    format!("{schema} {path} {key} {held}")
}

impl Activation for GnomeActivation {
    /// An entry GNOME does not list binds nothing.
    fn binding(&self) -> Result<Option<Accelerator>, ActivationError> {
        let binding = self.entry.string("binding");
        if !self.listed() || binding.is_empty() {
            return Ok(None);
        }
        Accelerator::parse(&binding).map(Some)
    }

    fn command(&self) -> Result<Option<String>, ActivationError> {
        Ok(self
            .listed()
            .then(|| self.entry.string("command").to_string())
            .filter(|command| !command.is_empty()))
    }

    /// Keeps every other custom shortcut.
    fn bind(&self, accelerator: &Accelerator, action: &Action) -> Result<(), ActivationError> {
        self.entry
            .set_string("name", &action.name)
            .map_err(refused)?;
        self.entry
            .set_string("command", &action.command)
            .map_err(refused)?;
        self.entry
            .set_string("binding", accelerator.as_str())
            .map_err(refused)?;
        if !self.listed() {
            let mut paths: Vec<String> = self
                .list
                .strv(LIST_KEY)
                .iter()
                .map(|path| path.to_string())
                .collect();
            paths.push(PATH.to_owned());
            self.set_list(paths)?;
        }
        Ok(())
    }

    fn clear(&self) -> Result<(), ActivationError> {
        if self.listed() {
            let kept = self
                .list
                .strv(LIST_KEY)
                .iter()
                .filter(|path| *path != PATH)
                .map(|path| path.to_string())
                .collect();
            self.set_list(kept)?;
        }
        for key in ["name", "command", "binding"] {
            self.entry.reset(key);
        }
        Ok(())
    }

    /// The desktop's own shortcuts first, then other custom ones.
    fn conflicts(&self, accelerator: &Accelerator) -> Result<Vec<Conflict>, ActivationError> {
        let mut found = self.desktop_conflicts(accelerator);
        found.extend(self.custom_conflicts(accelerator));
        Ok(found)
    }

    fn release(&self, conflict: &Conflict) -> Result<(), ActivationError> {
        if conflict.reserved {
            return Err(ActivationError::Reserved(conflict.action.clone()));
        }
        let mut parts = conflict.holder.splitn(4, ' ');
        let (Some(schema), Some(path), Some(key), Some(held)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(ActivationError::Refused(format!(
                "not a holder this backend made: {:?}",
                conflict.holder
            )));
        };
        let schema = self
            .source
            .lookup(schema, true)
            .ok_or_else(|| ActivationError::Unavailable(format!("no schema {schema}")))?;
        let path = (path != NO_PATH).then_some(path);
        let settings = gio::Settings::new_full(&schema, self.backend.as_ref(), path);
        if key == "binding" {
            return settings.set_string("binding", "").map_err(refused);
        }
        let held = Accelerator::parse(held)?;
        let kept: Vec<String> = settings
            .strv(key)
            .iter()
            .filter(|other| !held.same_keys(other))
            .map(|other| other.to_string())
            .collect();
        settings.set_strv(key, kept).map_err(refused)
    }

    fn watch(&self, changed: Box<dyn Fn()>) -> Subscription {
        let changed: std::rc::Rc<dyn Fn()> = changed.into();
        let handlers: Vec<_> = [&self.list, &self.entry]
            .into_iter()
            .map(|settings| {
                let changed = changed.clone();
                (
                    settings.clone(),
                    settings.connect_changed(None, move |_, _| changed()),
                )
            })
            .collect();
        Subscription::new(move || {
            for (settings, handler) in handlers {
                settings.disconnect(handler);
            }
        })
    }
}
