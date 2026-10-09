//! GNOME's appearance: GSettings and libadwaita, as the HUD has always read
//! them.
//!
//! The accent is the style manager's, then the theme's `@accent_bg_color`
//! from `accent_widget`. Reduced motion is `gtk-interface-reduced-motion`
//! where GTK has it, else the inverted `enable-animations` key. Every GSettings
//! access is schema-guarded (E2b).

use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;
use myna_platform::appearance::{Appearance, AppearanceReadings, Freshness};
use myna_platform::Subscription;

use super::probe::{
    probe_css_accent, probe_enable_animations, probe_gtk_reduced_motion, probe_high_contrast,
    probe_platform_accent, settings_for_schema_key, ACCENT_KEY, ADW_ACCENT_RGBA_PROPERTY,
    ANIMATIONS_KEY, GTK_REDUCED_MOTION_PROPERTY, GTK_THEME_KEY, INTERFACE_SCHEMA,
};
use crate::motion::{reduced_motion, MotionReadings};

pub struct GnomeAppearance {
    accent_widget: glib::WeakRef<gtk::Widget>,
}

impl GnomeAppearance {
    pub fn new(accent_widget: &impl IsA<gtk::Widget>) -> Self {
        Self {
            accent_widget: accent_widget.upcast_ref::<gtk::Widget>().downgrade(),
        }
    }
}

impl Appearance for GnomeAppearance {
    fn read(&self) -> AppearanceReadings {
        AppearanceReadings {
            accent: probe_platform_accent().or_else(|| {
                self.accent_widget
                    .upgrade()
                    .as_ref()
                    .and_then(probe_css_accent)
            }),
            reduced_motion: reduced_motion(&MotionReadings {
                gtk_reduced_motion: probe_gtk_reduced_motion(),
                enable_animations: probe_enable_animations(),
            }),
            high_contrast: probe_high_contrast(),
        }
    }

    fn watch(&self, changed: Box<dyn Fn(Freshness)>) -> Subscription {
        let changed: Rc<dyn Fn(Freshness)> = Rc::from(changed);
        let mut settings_handles = Vec::new();

        // Watched as a change TRIGGER only; the value is never read from here
        // (the theme is the source). Same for the theme name, which is how a
        // Yaru accent variant changes.
        for key in [ACCENT_KEY, GTK_THEME_KEY, ANIMATIONS_KEY] {
            if let Some(settings) = settings_for_schema_key(INTERFACE_SCHEMA, key) {
                let cb = changed.clone();
                settings.connect_changed(Some(key), move |_, _| cb(Freshness::NextFrame));
                settings_handles.push(settings);
            }
        }

        let manager = adw::StyleManager::default();
        // libadwaita reloads the accent provider before emitting this, so the
        // theme already reports the new colour here.
        let accent_handle = {
            let cb = changed.clone();
            manager.connect_notify_local(Some(ADW_ACCENT_RGBA_PROPERTY), move |_, _| {
                cb(Freshness::Current)
            })
        };

        let gtk_settings = gtk::Settings::default();
        let mut gtk_handles = Vec::new();
        if let Some(settings) = &gtk_settings {
            if settings
                .find_property(GTK_REDUCED_MOTION_PROPERTY)
                .is_some()
            {
                let cb = changed.clone();
                gtk_handles.push(
                    settings
                        .connect_notify_local(Some(GTK_REDUCED_MOTION_PROPERTY), move |_, _| {
                            cb(Freshness::NextFrame)
                        }),
                );
            }
        }

        // Adw tracks high contrast, itself following
        // GtkSettings:gtk-interface-contrast where that exists.
        let contrast_handle = manager.find_property("high-contrast").map(|_| {
            let cb = changed.clone();
            manager
                .connect_notify_local(Some("high-contrast"), move |_, _| cb(Freshness::NextFrame))
        });

        Subscription::new(move || {
            manager.disconnect(accent_handle);
            if let Some(handle) = contrast_handle {
                manager.disconnect(handle);
            }
            if let Some(settings) = &gtk_settings {
                for handle in gtk_handles {
                    settings.disconnect(handle);
                }
            }
            drop(settings_handles);
        })
    }
}
