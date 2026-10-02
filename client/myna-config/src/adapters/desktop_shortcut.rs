//! The GNOME custom shortcut that toggles dictation. The same entry the snap's
//! `myna.install-shortcut` writes.

use gio::glib;
use gio::prelude::*;

use crate::shortcut::same_accelerator;

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

pub struct DesktopShortcut {
    source: gio::SettingsSchemaSource,
    backend: Option<gio::SettingsBackend>,
    list: gio::Settings,
    entry: gio::Settings,
}

/// A desktop shortcut already bound to the key being set.
#[derive(Clone, Debug)]
pub struct Conflict {
    /// What the key does now, as the desktop describes it.
    pub action: String,
    /// gsd-media-keys grabs a `-static` key once, at login, and keeps it
    /// until logout whatever the setting says, so it cannot be taken.
    pub reserved: bool,
    settings: gio::Settings,
    key: String,
    binding: String,
}

impl DesktopShortcut {
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

    /// The installed accelerator. An entry GNOME does not list binds nothing.
    pub fn binding(&self) -> Option<String> {
        let binding = self.entry.string("binding");
        (self.listed() && !binding.is_empty()).then(|| binding.to_string())
    }

    /// What the shortcut runs.
    pub fn command(&self) -> String {
        self.entry.string("command").to_string()
    }

    /// Bind `binding` to `command`, keeping every other custom shortcut.
    pub fn install(&self, name: &str, command: &str, binding: &str) -> Result<(), glib::BoolError> {
        self.entry.set_string("name", name)?;
        self.entry.set_string("command", command)?;
        self.entry.set_string("binding", binding)?;
        if !self.listed() {
            let mut paths: Vec<String> = self
                .list
                .strv(LIST_KEY)
                .iter()
                .map(|path| path.to_string())
                .collect();
            paths.push(PATH.to_owned());
            self.list.set_strv(LIST_KEY, paths)?;
        }
        Ok(())
    }

    /// Run `changed` whenever the binding may have changed, from here or from
    /// the desktop's keyboard settings.
    pub fn connect_changed(&self, changed: impl Fn() + Clone + 'static) {
        for settings in [&self.list, &self.entry] {
            let changed = changed.clone();
            settings.connect_changed(None, move |_, _| changed());
        }
    }

    /// The desktop shortcut, other than ours, that `binding` would clash with.
    pub fn conflict(&self, binding: &str) -> Option<Conflict> {
        self.desktop_conflict(binding)
            .or_else(|| self.custom_conflict(binding))
    }

    /// Take `conflict`'s key away from the action that holds it.
    pub fn release(&self, conflict: &Conflict) -> Result<(), glib::BoolError> {
        let settings = &conflict.settings;
        let key = conflict.key.as_str();
        if key == "binding" {
            return settings.set_string("binding", "");
        }
        let kept: Vec<String> = settings
            .strv(key)
            .iter()
            .filter(|held| !same_accelerator(held, &conflict.binding))
            .map(|held| held.to_string())
            .collect();
        settings.set_strv(key, kept)
    }

    fn desktop_conflict(&self, binding: &str) -> Option<Conflict> {
        KEYBINDING_SCHEMAS.iter().find_map(|id| {
            let schema = self.source.lookup(id, true)?;
            let settings = gio::Settings::new_full(&schema, self.backend.as_ref(), None);
            schema.list_keys().iter().find_map(|name| {
                let key = schema.key(name);
                if name == LIST_KEY || key.value_type().as_str() != "as" {
                    return None;
                }
                settings
                    .strv(name.as_str())
                    .iter()
                    .any(|held| same_accelerator(held, binding))
                    .then(|| Conflict {
                        action: key
                            .summary()
                            .map_or_else(|| name.to_string(), |summary| summary.to_string()),
                        reserved: name.ends_with("-static"),
                        settings: settings.clone(),
                        key: name.to_string(),
                        binding: binding.to_owned(),
                    })
            })
        })
    }

    fn custom_conflict(&self, binding: &str) -> Option<Conflict> {
        let schema = self.source.lookup(CUSTOM_KEYBINDING, true)?;
        self.list
            .strv(LIST_KEY)
            .iter()
            .filter(|path| *path != PATH)
            .find_map(|path| {
                let entry = gio::Settings::new_full(&schema, self.backend.as_ref(), Some(path));
                same_accelerator(&entry.string("binding"), binding).then(|| Conflict {
                    action: entry.string("name").to_string(),
                    reserved: false,
                    settings: entry.clone(),
                    key: "binding".to_owned(),
                    binding: binding.to_owned(),
                })
            })
    }

    fn listed(&self) -> bool {
        self.list.strv(LIST_KEY).iter().any(|path| path == PATH)
    }
}
