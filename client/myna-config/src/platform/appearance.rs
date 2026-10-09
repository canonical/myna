//! Appearance for desktops without GNOME's schemas, Xfce among them: what GTK
//! itself sees.
//!
//! Xfce's xfsettingsd publishes xfconf as XSETTINGS, which GTK maps to
//! `GtkSettings`: `gtk-enable-animations` (`Net/EnableAnimations`, unset in
//! xfsettingsd's defaults) and `gtk-theme-name` (`Net/ThemeName`). It exports
//! no contrast setting, so a theme whose name says high contrast counts, as
//! does whatever libadwaita derives. Likewise a theme named `-dark` is the
//! dark preference, which libadwaita cannot see: the caller feeds it in.
//!
//! libadwaita replaces `gtk-theme-name` with its own at start-up and ignores
//! the desktop's from then on, so the theme is the name GTK held before that
//! ([`remember_startup_theme`]) or, on Xfce, what xfsettingsd publishes in
//! xfconf, which also says when it changes.

use gtk::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;
use myna_platform::appearance::{
    is_dark_theme, is_high_contrast_theme, Appearance, AppearanceReadings, Freshness,
};
use myna_platform::Subscription;

use super::xfce::xfconf::Xfconf;

/// What GTK, and xfconf where the desktop has it, say.
#[derive(Default)]
pub struct GtkAppearance {
    xfconf: Option<Xfconf>,
}

impl GtkAppearance {
    /// Read the theme from xfsettingsd's `xsettings` channel.
    pub fn with_xfconf(xfconf: Xfconf) -> Self {
        Self {
            xfconf: Some(xfconf),
        }
    }

    /// The desktop's GTK theme name.
    fn theme(&self) -> Option<String> {
        let published = self
            .xfconf
            .as_ref()
            .and_then(|xfconf| xfconf.get(XFCONF_THEME).ok().flatten())
            .and_then(|value| value.str().map(str::to_owned));
        published.or_else(|| {
            STARTUP_THEME
                .get()
                .cloned()
                .flatten()
                .or_else(current_theme)
        })
    }
}

const XFCONF_THEME: &str = "/Net/ThemeName";

/// The GTK theme name before libadwaita replaces it.
static STARTUP_THEME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

fn current_theme() -> Option<String> {
    gtk::Settings::default()
        .and_then(|settings| settings.gtk_theme_name())
        .map(|name| name.to_string())
}

/// Keep the theme GTK starts with. Call after `gtk::init`, before libadwaita
/// starts.
pub fn remember_startup_theme() {
    STARTUP_THEME.get_or_init(current_theme);
}

/// libadwaita's own high-contrast reading, once it runs.
fn adw_high_contrast() -> bool {
    adw::is_initialized() && adw::StyleManager::default().is_high_contrast()
}

/// `GtkSettings:gtk-enable-animations`, true where there are no settings.
pub(super) fn animations_enabled() -> bool {
    gtk::Settings::default().is_none_or(|settings| settings.is_gtk_enable_animations())
}

impl Appearance for GtkAppearance {
    fn read(&self) -> AppearanceReadings {
        let theme = self.theme();
        let theme_contrast = theme.as_deref().is_some_and(is_high_contrast_theme);
        AppearanceReadings {
            prefers_dark: theme.as_deref().is_some_and(is_dark_theme),
            accent: None,
            reduced_motion: !animations_enabled(),
            high_contrast: adw_high_contrast() || theme_contrast,
        }
    }

    fn watch(&self, changed: Box<dyn Fn(Freshness)>) -> Subscription {
        let changed: std::rc::Rc<dyn Fn(Freshness)> = changed.into();
        let contrast = adw::is_initialized().then(|| {
            let manager = adw::StyleManager::default();
            let changed = changed.clone();
            let handle = manager.connect_high_contrast_notify(move |_| changed(Freshness::Current));
            (manager, handle)
        });
        let theme_watch = self.xfconf.as_ref().map(|xfconf| {
            let changed = changed.clone();
            xfconf.watch(move |property| {
                if property == XFCONF_THEME {
                    changed(Freshness::Current);
                }
            })
        });
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
            drop(theme_watch);
            if let Some((manager, handle)) = contrast {
                manager.disconnect(handle);
            }
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
                Box::new(GtkAppearance::default())
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

        fn set_prefers_dark(&mut self, on: bool) -> bool {
            if self.gnome {
                return false;
            }
            gtk::Settings::default()
                .expect("display")
                .set_gtk_theme_name(Some(if on { "Adwaita-dark" } else { "Adwaita" }));
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
                report.not_applicable.iter().all(|check| matches!(
                    *check,
                    "high_contrast_is_read_back" | "dark_is_read_back"
                )),
                "{report:?}"
            );
        });
    }
}
