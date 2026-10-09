//! The HUD's appearance backends against the conformance suite, on a real GTK
//! under a display (`ui-check`). Gated like the other GTK suites, since a
//! hermetic run has no display.

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;
use myna_hud::appearance::generic::GtkAppearance;
use myna_hud::appearance::gnome::GnomeAppearance;
use myna_platform::appearance::Appearance;
use myna_platform::conformance::appearance::{run, Fixture};

const GTK_REDUCED_MOTION: &str = "gtk-interface-reduced-motion";

fn settle() {
    let context = glib::MainContext::default();
    for _ in 0..20 {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// `GtkReducedMotion` is an enum the bindings do not type.
fn set_gtk_enum(settings: &gtk::Settings, name: &str, value: i32) {
    use glib::translate::ToGlibPtrMut;
    let property = settings.find_property(name).expect("property");
    let mut gvalue = glib::Value::from_type(property.value_type());
    // SAFETY: the value was just built with the property's enum type.
    unsafe { glib::gobject_ffi::g_value_set_enum(gvalue.to_glib_none_mut().0, value) };
    settings.set_property_from_value(name, &gvalue);
}

struct Rig {
    gnome: bool,
    /// Kept alive for the backend's accent probe.
    probe: gtk::Label,
}

impl Rig {
    fn new(gnome: bool) -> Self {
        Self {
            gnome,
            probe: gtk::Label::new(None),
        }
    }

    fn settings() -> gtk::Settings {
        gtk::Settings::default().expect("display")
    }

    fn reset(&self) {
        let settings = Self::settings();
        settings.set_gtk_enable_animations(true);
        settings.set_gtk_theme_name(Some("Adwaita"));
        if settings.find_property(GTK_REDUCED_MOTION).is_some() {
            set_gtk_enum(&settings, GTK_REDUCED_MOTION, 0);
        }
        if let Some(interface) = myna_hud::appearance::probe::settings_for_schema_key(
            "org.gnome.desktop.interface",
            "enable-animations",
        ) {
            interface.set_boolean("enable-animations", true).ok();
        }
        settle();
    }
}

impl Fixture for Rig {
    fn setup(&mut self) -> Box<dyn Appearance> {
        self.reset();
        if self.gnome {
            Box::new(GnomeAppearance::new(&self.probe))
        } else {
            Box::new(GtkAppearance::new(&self.probe))
        }
    }

    fn set_reduced_motion(&mut self, on: bool) -> bool {
        let settings = Self::settings();
        if self.gnome {
            // GNOME's own sources: the GTK property where there is one, else
            // the GSettings key.
            if settings.find_property(GTK_REDUCED_MOTION).is_some() {
                set_gtk_enum(&settings, GTK_REDUCED_MOTION, on as i32);
                return true;
            }
            return myna_hud::appearance::probe::settings_for_schema_key(
                "org.gnome.desktop.interface",
                "enable-animations",
            )
            .is_some_and(|interface| interface.set_boolean("enable-animations", !on).is_ok());
        }
        settings.set_gtk_enable_animations(!on);
        true
    }

    fn set_high_contrast(&mut self, on: bool) -> bool {
        if self.gnome {
            // libadwaita derives it from the portal; nothing to set here.
            return false;
        }
        Self::settings().set_gtk_theme_name(Some(if on { "HighContrast" } else { "Adwaita" }));
        true
    }

    fn settle(&mut self) {
        settle();
    }
}

#[test]
fn both_backends_conform_when_enabled() {
    if std::env::var_os("MYNA_HUD_GTK_TESTS").is_none() {
        eprintln!("skipped: set MYNA_HUD_GTK_TESTS=1 under Xvfb");
        return;
    }
    // The desktop's GSettings stay untouched.
    std::env::set_var("GSETTINGS_BACKEND", "memory");
    gtk::test_synced(|| {
        gtk::init().expect("display");
        adw::init().expect("libadwaita");
        let gnome = run(&mut Rig::new(true));
        assert!(
            gnome
                .not_applicable
                .iter()
                .all(|check| *check == "high_contrast_is_read_back"),
            "GNOME skipped more than contrast: {gnome:?}"
        );
        let generic = run(&mut Rig::new(false));
        assert!(
            generic.not_applicable.is_empty(),
            "generic skipped checks: {generic:?}"
        );
    });
}
