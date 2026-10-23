//! The HUD's GTK appearance backend against the conformance suite, on a real GTK
//! under a display (`ui-check`). Gated like the other GTK suites, since a
//! hermetic run has no display.

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;
use myna_hud::appearance::generic::GtkAppearance;
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
    /// Kept alive for the backend's accent probe.
    probe: gtk::Label,
}

impl Rig {
    fn new() -> Self {
        Self {
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
        settle();
    }
}

impl Fixture for Rig {
    fn setup(&mut self) -> Box<dyn Appearance> {
        self.reset();
        Box::new(GtkAppearance::new(&self.probe))
    }

    fn set_reduced_motion(&mut self, on: bool) -> bool {
        let settings = Self::settings();
        settings.set_gtk_enable_animations(!on);
        true
    }

    fn set_high_contrast(&mut self, on: bool) -> bool {
        Self::settings().set_gtk_theme_name(Some(if on { "HighContrast" } else { "Adwaita" }));
        true
    }

    fn set_prefers_dark(&mut self, on: bool) -> bool {
        Self::settings().set_gtk_theme_name(Some(if on { "Adwaita-dark" } else { "Adwaita" }));
        true
    }

    fn settle(&mut self) {
        settle();
    }
}

#[test]
fn the_gtk_backend_conforms_when_enabled() {
    if std::env::var_os("MYNA_HUD_GTK_TESTS").is_none() {
        eprintln!("skipped: set MYNA_HUD_GTK_TESTS=1 under Xvfb");
        return;
    }
    gtk::test_synced(|| {
        gtk::init().expect("display");
        adw::init().expect("libadwaita");
        let report = run(&mut Rig::new());
        assert!(
            report.not_applicable.is_empty(),
            "skipped checks: {report:?}"
        );
    });
}
