//! platform — runtime probing for the two desktop preferences the HUD
//! honours: the **accent colour** (R18/R26) and **reduced motion** (E2b,
//! FR-022a). The rules live in [`crate::accent`] and [`crate::motion`];
//! this module only reads the live sources and feeds them.
//!
//! ## No compile-time version features
//!
//! Both newer sources are looked up by *runtime GObject property name*
//! rather than a `gtk4/v4_22` or `libadwaita/v1_7` cargo feature, because a
//! single binary must serve a runtime matrix that spans the 24.04 workshop
//! (GTK 4.14 / libadwaita 1.5), the snap's gnome-46-2404 SDK (GTK 4.18 /
//! Ubuntu-patched libadwaita 1.7) and 26.04 hosts (GTK 4.22 / libadwaita
//! 1.9). A compile-time feature would either raise the floor or forfeit the
//! newer source; `find_property` costs nothing and degrades exactly.
//!
//! `AdwStyleManager:accent-color-rgba` is read as a **boxed `gdk::RGBA`**,
//! deliberately never as the `AdwAccentColor` enum: Ubuntu's Yaru patches
//! add accent values outside upstream's enumeration, and mapping an unknown
//! enum member would panic or mis-name a colour the RGBA reports exactly.
//!
//! ## Crash guard (E2b)
//!
//! `org.gnome.desktop.a11y.interface reduced-motion` is NEVER read. It is
//! new in gsettings-desktop-schemas, and constructing a `gio::Settings` for
//! a missing schema — or reading a missing key — **aborts the process**.
//! Every GSettings access below is guarded through
//! [`settings_for_schema_key`], which consults the schema source first.

use glib::translate::ToGlibPtr;
use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;

use crate::accent::{resolve_accent_palette, resolve_platform_accent_palette, AccentPalette};
use crate::motion::{reduced_motion, MotionReadings};
use crate::shader::Rgb;

/// `org.gnome.desktop.interface`, home of both `accent-color` and the
/// `enable-animations` fallback.
const INTERFACE_SCHEMA: &str = "org.gnome.desktop.interface";
const ACCENT_KEY: &str = "accent-color";
const ANIMATIONS_KEY: &str = "enable-animations";

/// `GtkSettings`' reduced-motion property (GTK ≥ 4.22).
const GTK_REDUCED_MOTION_PROPERTY: &str = "gtk-interface-reduced-motion";
/// `GtkReducedMotion.no_preference` — the one value meaning "full motion".
const GTK_REDUCED_MOTION_NO_PREFERENCE: i32 = 0;
/// `AdwStyleManager`'s resolved accent (libadwaita ≥ 1.7).
const ADW_ACCENT_RGBA_PROPERTY: &str = "accent-color-rgba";

/// Build a [`gio::Settings`] for `schema` only if the schema **and** `key`
/// both exist on this system; otherwise `None`.
///
/// This is the guard that keeps a missing schema/key from aborting the
/// process (E2b) — the reason the HUD may never touch a GSettings key
/// without asking first.
pub fn settings_for_schema_key(schema: &str, key: &str) -> Option<gtk::gio::Settings> {
    let source = gtk::gio::SettingsSchemaSource::default()?;
    let schema_obj = source.lookup(schema, true)?;
    if !schema_obj.has_key(key) {
        return None;
    }
    Some(gtk::gio::Settings::new(schema))
}

/// Read `GtkSettings:gtk-interface-reduced-motion` if this GTK has it.
///
/// Returns `None` on GTK < 4.22, where [`crate::motion`] falls back to the
/// inverted `enable-animations`.
///
/// The property is a **`GtkReducedMotion` enum**, not a boolean
/// (`no_preference = 0`, `reduce = 1`) — reading it as a `bool` fails and
/// looks exactly like "the property is absent", silently forfeiting the
/// primary source on precisely the systems that have it. It is read through
/// `g_value_get_enum` rather than a bound Rust enum type both because the
/// binding lacks one without a `v4_22` feature and because that tolerates
/// additive values: anything other than `no_preference` counts as reduced
/// motion, so a future stronger level errs toward less animation.
pub fn probe_gtk_reduced_motion() -> Option<bool> {
    let settings = gtk::Settings::default()?;
    let property = settings
        .find_property(GTK_REDUCED_MOTION_PROPERTY)
        .map(|p| p.name().to_string())?;
    decode_reduced_motion(&settings.property_value(&property))
}

/// Decode whatever `gtk-interface-reduced-motion` holds into the boolean
/// [`crate::motion`] expects. Split out from the probe so the enum handling
/// is testable without a display.
pub fn decode_reduced_motion(value: &glib::Value) -> Option<bool> {
    if let Ok(flag) = value.get::<bool>() {
        return Some(flag);
    }
    if value.type_().is_a(glib::Type::ENUM) {
        // SAFETY: the GValue is known to hold an enum.
        let raw = unsafe { glib::gobject_ffi::g_value_get_enum(value.to_glib_none().0) };
        return Some(raw != GTK_REDUCED_MOTION_NO_PREFERENCE);
    }
    None
}

/// Read `org.gnome.desktop.interface enable-animations` (raw, NOT inverted
/// — [`crate::motion::reduced_motion`] owns that), schema/key guarded.
pub fn probe_enable_animations() -> Option<bool> {
    let settings = settings_for_schema_key(INTERFACE_SCHEMA, ANIMATIONS_KEY)?;
    Some(settings.boolean(ANIMATIONS_KEY))
}

/// The live reduced-motion preference, resolved through both safe sources.
pub fn probe_reduced_motion() -> bool {
    reduced_motion(&MotionReadings {
        gtk_reduced_motion: probe_gtk_reduced_motion(),
        enable_animations: probe_enable_animations(),
    })
}

/// The accent's **user value** — `None` when the user never wrote the key
/// (R18's critical distinction: an untouched default and a deliberate
/// `'blue'` read identically any other way, and only the former may be
/// re-tinted to Ubuntu orange).
pub fn probe_accent_user_value() -> Option<String> {
    let settings = settings_for_schema_key(INTERFACE_SCHEMA, ACCENT_KEY)?;
    let user_value = settings.user_value(ACCENT_KEY)?;
    user_value.get::<String>()
}

/// The platform-resolved accent from libadwaita ≥ 1.7's style manager, read
/// as a boxed RGBA so Ubuntu's Yaru accents (outside upstream's enum) come
/// through exactly. `None` on older libadwaita.
pub fn probe_platform_accent() -> Option<Rgb> {
    let manager = adw::StyleManager::default();
    let property = manager
        .find_property(ADW_ACCENT_RGBA_PROPERTY)
        .map(|p| p.name().to_string())?;
    let rgba = manager.property_value(&property).get::<gdk::RGBA>().ok()?;
    Some(Rgb {
        r: rgba.red() as f64,
        g: rgba.green() as f64,
        b: rgba.blue() as f64,
    })
}

/// The ribbon's palette for the current desktop: the fixed table when
/// libadwaita cannot resolve the accent, the platform colour when it can.
///
/// The untouched-default rule (Ubuntu orange) wins over platform resolution
/// in both paths — that decision lives in [`crate::accent`].
pub fn probe_accent_palette() -> AccentPalette {
    let user_value = probe_accent_user_value();
    match probe_platform_accent() {
        Some(platform) => resolve_platform_accent_palette(user_value.as_deref(), platform),
        None => resolve_accent_palette(user_value.as_deref()),
    }
}

/// Call `on_change` whenever either preference may have changed: the accent
/// key, the style manager's resolved accent, the animations key, and the
/// GtkSettings property when present.
///
/// The returned guard owns the subscriptions; dropping it disconnects
/// everything, so no callback can outlive the window.
pub fn watch_preferences<F: Fn() + 'static + Clone>(on_change: F) -> PreferenceWatch {
    let mut settings_handles = Vec::new();

    if let Some(settings) = settings_for_schema_key(INTERFACE_SCHEMA, ACCENT_KEY) {
        let cb = on_change.clone();
        settings.connect_changed(Some(ACCENT_KEY), move |_, _| cb());
        settings_handles.push(settings);
    }
    if let Some(settings) = settings_for_schema_key(INTERFACE_SCHEMA, ANIMATIONS_KEY) {
        let cb = on_change.clone();
        settings.connect_changed(Some(ANIMATIONS_KEY), move |_, _| cb());
        settings_handles.push(settings);
    }

    let manager = adw::StyleManager::default();
    let mut adw_handle = None;
    if manager.find_property(ADW_ACCENT_RGBA_PROPERTY).is_some() {
        let cb = on_change.clone();
        adw_handle =
            Some(manager.connect_notify_local(Some(ADW_ACCENT_RGBA_PROPERTY), move |_, _| cb()));
    }

    let mut gtk_handle = None;
    let gtk_settings = gtk::Settings::default();
    if let Some(settings) = &gtk_settings {
        if settings
            .find_property(GTK_REDUCED_MOTION_PROPERTY)
            .is_some()
        {
            let cb = on_change.clone();
            gtk_handle = Some(
                settings.connect_notify_local(Some(GTK_REDUCED_MOTION_PROPERTY), move |_, _| cb()),
            );
        }
    }

    PreferenceWatch {
        _settings: settings_handles,
        manager,
        adw_handle,
        gtk_settings,
        gtk_handle,
    }
}

/// Owns the preference subscriptions; disconnects them on drop.
pub struct PreferenceWatch {
    _settings: Vec<gtk::gio::Settings>,
    manager: adw::StyleManager,
    adw_handle: Option<glib::SignalHandlerId>,
    gtk_settings: Option<gtk::Settings>,
    gtk_handle: Option<glib::SignalHandlerId>,
}

impl Drop for PreferenceWatch {
    fn drop(&mut self) {
        if let Some(handle) = self.adw_handle.take() {
            self.manager.disconnect(handle);
        }
        if let (Some(settings), Some(handle)) = (&self.gtk_settings, self.gtk_handle.take()) {
            settings.disconnect(handle);
        }
        // The gio::Settings objects drop with their handlers attached; the
        // objects themselves are owned here and released now.
    }
}
