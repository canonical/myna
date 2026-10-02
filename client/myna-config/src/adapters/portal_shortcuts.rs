//! Myna's entry in GNOME's store of portal shortcuts, and the portal backend's
//! call that moves a live grab to a new key.
//!
//! From GNOME 48 the portal hands `BindShortcuts` to GNOME Settings, which
//! keeps each grant in dconf under gnome-settings-daemon's
//! `global-shortcuts.application` schema and raises its dialog only for a
//! shortcut id with no stored entry. Changing a bound key in that dialog means
//! taking the entry out for the dialog's lifetime (`docs/onboarding.md`).
//! The portal has filed the snap's daemon under `myna_myna` and under `.`
//! (an empty app id), so both are searched.

use gio::glib::{self, Variant};
use gio::prelude::*;

/// The app id the portal files Myna's daemon under.
pub const APP_ID: &str = "myna_myna";
/// The id the daemon binds the dictation shortcut as.
pub const SHORTCUT_ID: &str = "dictate";

const SCHEMA: &str = "org.gnome.settings-daemon.global-shortcuts.application";
const APPLICATIONS_SCHEMA: &str = "org.gnome.settings-daemon.global-shortcuts";
/// GNOME Settings' service that raises the dialog and keeps the store.
pub const PROVIDER: &str = "org.gnome.Settings.GlobalShortcutsProvider";
const ROOT: &str = "/org/gnome/settings-daemon/global-shortcuts/";
const KEY: &str = "shortcuts";
const PORTAL_BACKEND: &str = "org.freedesktop.impl.portal.desktop.gnome";
const REBIND_PATH: &str = "/org/gnome/globalshortcuts";
const REBIND_INTERFACE: &str = "org.gnome.GlobalShortcutsRebind";
const REBIND_TIMEOUT_MS: i32 = 5_000;

/// GNOME's whole store: the list of app ids and each app's shortcuts.
pub struct Store {
    source: gio::SettingsSchemaSource,
    backend: Option<gio::SettingsBackend>,
    applications: gio::Settings,
}

impl Store {
    /// `None` without gnome-settings-daemon's schemas: a GNOME whose portal
    /// keeps no such store.
    pub fn open() -> Option<Self> {
        Self::open_with(gio::SettingsSchemaSource::default()?, None)
    }

    pub fn open_with(
        source: gio::SettingsSchemaSource,
        backend: Option<gio::SettingsBackend>,
    ) -> Option<Self> {
        source.lookup(SCHEMA, true)?;
        let schema = source.lookup(APPLICATIONS_SCHEMA, true)?;
        let applications = gio::Settings::new_full(&schema, backend.as_ref(), None::<&str>);
        Some(Self {
            source,
            backend,
            applications,
        })
    }

    pub fn app(&self, app_id: &str) -> PortalShortcuts {
        PortalShortcuts::open_with(&self.source, self.backend.as_ref(), app_id)
            .expect("the schema was found when the store opened")
    }

    /// The app ids Myna's grant may be filed under, [`APP_ID`] first.
    pub fn myna_apps(&self) -> Vec<String> {
        let listed = self.applications.strv("applications");
        std::iter::once(APP_ID.to_owned())
            .chain(
                listed
                    .iter()
                    .map(|id| id.as_str())
                    .filter(|id| *id != APP_ID && (*id == "." || id.contains("myna")))
                    .map(str::to_owned),
            )
            .collect()
    }
}

pub struct PortalShortcuts {
    settings: gio::Settings,
}

/// An entry taken out of the store, to put back unless a new one replaced it.
#[derive(Debug)]
pub struct Taken {
    id: String,
    entry: Variant,
    /// The entry's first key, as a GTK accelerator.
    pub accelerator: String,
}

impl PortalShortcuts {
    pub fn open_with(
        source: &gio::SettingsSchemaSource,
        backend: Option<&gio::SettingsBackend>,
        app_id: &str,
    ) -> Option<Self> {
        let schema = source.lookup(SCHEMA, true)?;
        let path = format!("{ROOT}{app_id}/");
        Some(Self {
            settings: gio::Settings::new_full(&schema, backend, Some(&path)),
        })
    }

    /// The stored shortcuts, as `RebindShortcuts` takes them.
    pub fn shortcuts(&self) -> Variant {
        self.settings.value(KEY)
    }

    /// The first key stored for `id`.
    pub fn accelerator(&self, id: &str) -> Option<String> {
        self.entries()
            .into_iter()
            .find(|(entry_id, _)| entry_id == id)
            .and_then(|(_, entry)| first_key(&entry))
    }

    /// Remove `id`'s entry and flush it to dconf, so the portal's dialog
    /// offers it again. `None` when no key is stored for it.
    pub fn take(&self, id: &str) -> Option<Taken> {
        let (entries, mut taken): (Vec<_>, Vec<_>) = self
            .entries()
            .into_iter()
            .partition(|(entry_id, _)| entry_id != id);
        let (_, entry) = taken.pop()?;
        let accelerator = first_key(&entry)?;
        self.store(entries.into_iter().map(|(_, entry)| entry))?;
        Some(Taken {
            id: id.to_owned(),
            entry,
            accelerator,
        })
    }

    /// Put `taken` back, unless an entry for its id was stored meanwhile.
    pub fn put_back(&self, taken: Taken) {
        let entries = self.entries();
        if entries.iter().any(|(id, _)| *id == taken.id) {
            return;
        }
        let restored = entries
            .into_iter()
            .map(|(_, entry)| entry)
            .chain(std::iter::once(taken.entry));
        if self.store(restored).is_none() {
            glib::g_message!(
                crate::LOG_DOMAIN,
                "shortcut: could not put back {}",
                taken.id
            );
        }
    }

    /// Each stored `(sa{sv})` with its id.
    fn entries(&self) -> Vec<(String, Variant)> {
        self.shortcuts()
            .iter()
            .filter_map(|entry| Some((entry.try_child_value(0)?.get::<String>()?, entry)))
            .collect()
    }

    fn store(&self, entries: impl Iterator<Item = Variant>) -> Option<()> {
        let value = Variant::array_from_iter_with_type(self.shortcuts().type_().element(), entries);
        self.settings.set_value(KEY, &value).ok()?;
        gio::Settings::sync();
        Some(())
    }
}

/// The first key of a stored `(sa{sv})`.
fn first_key(entry: &Variant) -> Option<String> {
    let options = glib::VariantDict::new(Some(&entry.try_child_value(1)?));
    let keys = options.lookup_value("shortcuts", Some(glib::VariantTy::STRING_ARRAY))?;
    keys.get::<Vec<String>>()?
        .into_iter()
        .find(|key| !key.is_empty())
}

/// Whether GNOME Settings' provider runs or can be started: without it the
/// portal answers binds itself and the store above means nothing.
pub async fn provider_present(connection: &gio::DBusConnection) -> bool {
    for method in ["ListNames", "ListActivatableNames"] {
        let names = connection
            .call_future(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                method,
                None,
                Some(glib::VariantTy::new("(as)").expect("a valid type")),
                gio::DBusCallFlags::NONE,
                REBIND_TIMEOUT_MS,
            )
            .await
            .ok()
            .and_then(|reply| reply.get::<(Vec<String>,)>());
        if names.is_some_and(|(names,)| names.iter().any(|name| name == PROVIDER)) {
            return true;
        }
    }
    false
}

/// Ask the portal backend to move the live grab of `app_id`'s session to the
/// keys in `shortcuts`; GNOME Settings does the same after an edit.
pub async fn rebind(
    connection: &gio::DBusConnection,
    app_id: &str,
    shortcuts: Variant,
) -> Result<(), glib::Error> {
    // A peer-to-peer connection has no bus to route by name.
    let destination = connection
        .flags()
        .contains(gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION)
        .then_some(PORTAL_BACKEND);
    connection
        .call_future(
            destination,
            REBIND_PATH,
            REBIND_INTERFACE,
            "RebindShortcuts",
            Some(&Variant::tuple_from_iter([app_id.to_variant(), shortcuts])),
            None,
            gio::DBusCallFlags::NONE,
            REBIND_TIMEOUT_MS,
        )
        .await
        .map(drop)
}
