//! Appearance for desktops without GNOME's schemas, Xfce among them: what GTK
//! itself sees.
//!
//! Xfce's xfsettingsd publishes xfconf as XSETTINGS, which GTK maps to
//! `GtkSettings`: `gtk-enable-animations` (`Net/EnableAnimations`, unset in
//! xfsettingsd's defaults) and `gtk-theme-name` (`Net/ThemeName`). It exports
//! no contrast setting, so a theme whose name says high contrast counts, as
//! does whatever libadwaita derives.

use gtk::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;
use myna_platform::appearance::{
    is_high_contrast_theme, Appearance, AppearanceReadings, Freshness,
};
use myna_platform::Subscription;

pub struct GtkAppearance;

/// `GtkSettings:gtk-enable-animations`, true where there are no settings.
pub(super) fn animations_enabled() -> bool {
    gtk::Settings::default().is_none_or(|settings| settings.is_gtk_enable_animations())
}

impl Appearance for GtkAppearance {
    fn read(&self) -> AppearanceReadings {
        let theme_contrast = gtk::Settings::default()
            .and_then(|settings| settings.gtk_theme_name())
            .is_some_and(|name| is_high_contrast_theme(&name));
        AppearanceReadings {
            accent: None,
            reduced_motion: !animations_enabled(),
            high_contrast: adw::StyleManager::default().is_high_contrast() || theme_contrast,
        }
    }

    fn watch(&self, changed: Box<dyn Fn(Freshness)>) -> Subscription {
        let changed: std::rc::Rc<dyn Fn(Freshness)> = changed.into();
        let manager = adw::StyleManager::default();
        let contrast = {
            let changed = changed.clone();
            manager.connect_high_contrast_notify(move |_| changed(Freshness::Current))
        };
        let settings = gtk::Settings::default();
        let handles: Vec<_> = settings
            .iter()
            .flat_map(|settings| {
                let animations = changed.clone();
                let theme = changed.clone();
                [
                    settings.connect_gtk_enable_animations_notify(move |_| {
                        animations(Freshness::Current)
                    }),
                    settings.connect_gtk_theme_name_notify(move |_| theme(Freshness::Current)),
                ]
            })
            .collect();
        Subscription::new(move || {
            manager.disconnect(contrast);
            if let Some(settings) = settings {
                for handle in handles {
                    settings.disconnect(handle);
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::gnome::appearance::GnomeAppearance;
    use gio::prelude::*;
    use myna_platform::conformance::appearance::{run, Fixture};

    fn settle() {
        let context = gtk::glib::MainContext::default();
        for _ in 0..20 {
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    struct Rig {
        gnome: bool,
    }

    impl Fixture for Rig {
        fn setup(&mut self) -> Box<dyn Appearance> {
            let settings = gtk::Settings::default().expect("display");
            settings.set_gtk_enable_animations(true);
            settings.set_gtk_theme_name(Some("Adwaita"));
            settle();
            if self.gnome {
                Box::new(GnomeAppearance)
            } else {
                Box::new(GtkAppearance)
            }
        }

        fn set_reduced_motion(&mut self, on: bool) -> bool {
            gtk::Settings::default()
                .expect("display")
                .set_gtk_enable_animations(!on);
            true
        }

        fn set_high_contrast(&mut self, on: bool) -> bool {
            if self.gnome {
                // Only a throwaway backend may be written: the user's is dconf.
                let backend = gio::SettingsBackend::default();
                if backend.type_().name() != "GMemorySettingsBackend" {
                    return false;
                }
                return gio::SettingsSchemaSource::default()
                    .and_then(|source| source.lookup("org.gnome.desktop.a11y.interface", true))
                    .is_some_and(|_| {
                        gio::Settings::new("org.gnome.desktop.a11y.interface")
                            .set_boolean("high-contrast", on)
                            .is_ok()
                    });
            }
            gtk::Settings::default()
                .expect("display")
                .set_gtk_theme_name(Some(if on { "HighContrast" } else { "Adwaita" }));
            true
        }

        fn settle(&mut self) {
            settle();
        }
    }

    #[test]
    fn the_gtk_backend_conforms() {
        crate::ui::on_gtk_thread(|| {
            gtk::init().expect("display");
            adw::init().expect("libadwaita");
            let report = run(&mut Rig { gnome: false });
            assert!(report.not_applicable.is_empty(), "{report:?}");
        });
    }

    #[test]
    fn the_gnome_backend_conforms() {
        crate::ui::on_gtk_thread(|| {
            gtk::init().expect("display");
            adw::init().expect("libadwaita");
            let report = run(&mut Rig { gnome: true });
            assert!(
                report
                    .not_applicable
                    .iter()
                    .all(|check| *check == "high_contrast_is_read_back"),
                "{report:?}"
            );
        });
    }
}
