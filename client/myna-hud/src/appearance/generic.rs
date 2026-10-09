//! Appearance for desktops that are not GNOME, Xfce among them: what GTK
//! itself sees, with no GSettings.
//!
//! On Xfce, xfsettingsd publishes `/Net`, `/Xft` and `/Gtk` xfconf properties
//! as XSETTINGS and GTK maps them to `GtkSettings`: `Net/ThemeName` is
//! `gtk-theme-name`, `Net/EnableAnimations` is `gtk-enable-animations` (not
//! in xfsettingsd's defaults, so it holds only when someone sets it).
//! GNOME's schemas may be absent or stale there. xfsettingsd exports no
//! contrast or reduced-motion setting: high contrast is a theme whose name
//! says so, or whatever libadwaita derives.
//!
//! Reduced motion is on when either `gtk-interface-reduced-motion` (GTK 4.22)
//! or a disabled `gtk-enable-animations` says so. The accent is the theme's
//! `@accent_bg_color` from `accent_widget`; none when the theme has none.

use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;
use myna_platform::appearance::{Appearance, AppearanceReadings, Freshness};
use myna_platform::Subscription;

use super::probe::{
    is_high_contrast_theme, probe_css_accent, probe_gtk_enable_animations,
    probe_gtk_reduced_motion, probe_high_contrast, ADW_ACCENT_RGBA_PROPERTY,
    GTK_REDUCED_MOTION_PROPERTY,
};

pub struct GtkAppearance {
    accent_widget: glib::WeakRef<gtk::Widget>,
}

impl GtkAppearance {
    pub fn new(accent_widget: &impl IsA<gtk::Widget>) -> Self {
        Self {
            accent_widget: accent_widget.upcast_ref::<gtk::Widget>().downgrade(),
        }
    }
}

fn theme_is_high_contrast() -> bool {
    gtk::Settings::default()
        .and_then(|settings| settings.gtk_theme_name())
        .is_some_and(|name| is_high_contrast_theme(&name))
}

impl Appearance for GtkAppearance {
    fn read(&self) -> AppearanceReadings {
        AppearanceReadings {
            accent: self
                .accent_widget
                .upgrade()
                .as_ref()
                .and_then(probe_css_accent),
            reduced_motion: probe_gtk_reduced_motion().unwrap_or(false)
                || !probe_gtk_enable_animations().unwrap_or(true),
            high_contrast: probe_high_contrast() || theme_is_high_contrast(),
        }
    }

    fn watch(&self, changed: Box<dyn Fn(Freshness)>) -> Subscription {
        let changed: Rc<dyn Fn(Freshness)> = Rc::from(changed);
        let manager = adw::StyleManager::default();
        let accent_handle = {
            let cb = changed.clone();
            manager.connect_notify_local(Some(ADW_ACCENT_RGBA_PROPERTY), move |_, _| {
                cb(Freshness::Current)
            })
        };
        let contrast_handle = manager.find_property("high-contrast").map(|_| {
            let cb = changed.clone();
            manager
                .connect_notify_local(Some("high-contrast"), move |_, _| cb(Freshness::NextFrame))
        });

        let settings = gtk::Settings::default();
        let mut handles = Vec::new();
        if let Some(settings) = &settings {
            // A new theme restyles the accent lazily.
            let mut names = vec!["gtk-theme-name", "gtk-enable-animations"];
            if settings
                .find_property(GTK_REDUCED_MOTION_PROPERTY)
                .is_some()
            {
                names.push(GTK_REDUCED_MOTION_PROPERTY);
            }
            for name in names {
                let cb = changed.clone();
                handles.push(
                    settings.connect_notify_local(Some(name), move |_, _| cb(Freshness::NextFrame)),
                );
            }
        }

        Subscription::new(move || {
            manager.disconnect(accent_handle);
            if let Some(handle) = contrast_handle {
                manager.disconnect(handle);
            }
            if let Some(settings) = &settings {
                for handle in handles {
                    settings.disconnect(handle);
                }
            }
        })
    }
}
