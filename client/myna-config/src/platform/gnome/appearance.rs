//! GNOME's appearance: the GTK settings, libadwaita and the accessibility
//! schema's `high-contrast`, which Adw does not always mirror.

use gio::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;
use myna_platform::appearance::{Appearance, AppearanceReadings, Freshness};
use myna_platform::Subscription;

use crate::platform::appearance::animations_enabled;

const A11Y_INTERFACE_SCHEMA: &str = "org.gnome.desktop.a11y.interface";
const HIGH_CONTRAST_KEY: &str = "high-contrast";

pub struct GnomeAppearance;

/// The GSettings object for `schema`, or none when the schema or `key` is not
/// installed. `gio::Settings::new` aborts the process on a missing schema, and
/// desktops such as Xubuntu do not ship GNOME's accessibility schema.
fn settings_for_schema_key(schema: &str, key: &str) -> Option<gio::Settings> {
    let source = gio::SettingsSchemaSource::default()?;
    let schema_obj = source.lookup(schema, true)?;
    if !schema_obj.has_key(key) {
        return None;
    }
    Some(gio::Settings::new(schema))
}

/// Whether GNOME's high-contrast preference is on. Without the schema there is
/// no such preference, so it reads as off.
fn high_contrast_preference() -> bool {
    settings_for_schema_key(A11Y_INTERFACE_SCHEMA, HIGH_CONTRAST_KEY)
        .is_some_and(|settings| settings.boolean(HIGH_CONTRAST_KEY))
}

impl Appearance for GnomeAppearance {
    fn read(&self) -> AppearanceReadings {
        AppearanceReadings {
            accent: None,
            reduced_motion: !animations_enabled(),
            high_contrast: adw::StyleManager::default().is_high_contrast()
                || high_contrast_preference(),
            // The portal drives libadwaita here, so its answer is the desktop's.
            prefers_dark: adw::StyleManager::default().is_dark(),
        }
    }

    fn watch(&self, changed: Box<dyn Fn(Freshness)>) -> Subscription {
        let changed: std::rc::Rc<dyn Fn(Freshness)> = changed.into();
        let manager = adw::StyleManager::default();
        let contrast = {
            let changed = changed.clone();
            manager.connect_high_contrast_notify(move |_| changed(Freshness::Current))
        };
        let dark = {
            let changed = changed.clone();
            manager.connect_dark_notify(move |_| changed(Freshness::Current))
        };
        let settings = gtk::Settings::default();
        let animations = settings.as_ref().map(|settings| {
            let changed = changed.clone();
            settings.connect_gtk_enable_animations_notify(move |_| changed(Freshness::Current))
        });
        let preference = settings_for_schema_key(A11Y_INTERFACE_SCHEMA, HIGH_CONTRAST_KEY);
        let preference_handle = preference.as_ref().map(|settings| {
            let changed = changed.clone();
            settings.connect_changed(Some(HIGH_CONTRAST_KEY), move |_, _| {
                changed(Freshness::Current)
            })
        });
        Subscription::new(move || {
            manager.disconnect(contrast);
            manager.disconnect(dark);
            if let (Some(settings), Some(handle)) = (&settings, animations) {
                settings.disconnect(handle);
            }
            if let (Some(settings), Some(handle)) = (&preference, preference_handle) {
                settings.disconnect(handle);
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_schema_reads_as_no_settings() {
        assert!(settings_for_schema_key("org.example.NoSuchSchema", "high-contrast").is_none());
    }
}
