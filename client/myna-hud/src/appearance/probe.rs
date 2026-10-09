//! probe - the live sources behind the HUD's [`Appearance`](myna_platform::appearance::Appearance) backends: the
//! **accent colour** (R18/R26), **reduced motion** (E2b, FR-022a) and high
//! contrast. The rules live in [`crate::accent`] and [`crate::motion`]; this
//! module only reads the sources.
//!
//! Only *host* preferences are read here. Myna's own settings are not: the
//! HUD is told what to draw by the publisher (`HudStyle`), because this
//! process runs on the desktop's GSettings backend while the rest of the
//! client runs on the snap's keyfile store, and a reader on the wrong side of
//! that line silently returns the schema default. See
//! `myna_desktop::dbus::hud_style`.
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
//! The **accent is read from CSS**, not from the settings name: a widget
//! styled `color: @accent_bg_color` is asked for its computed colour
//! ([`probe_css_accent`]). That is the direct analogue of the extension's
//! `-st-accent-color`, and it beats every alternative - the named colour
//! has existed since libadwaita 1.0, so it needs no version probing at all
//! and covers the whole runtime matrix; it resolves Ubuntu's Yaru tints
//! (including `wartybrown`, which upstream has no enum member for)
//! automatically; and it was measured identical to
//! `AdwStyleManager:accent-color-rgba` for every accent.
//!
//! `AdwStyleManager:accent-color-rgba` remains as a fallback for stacks
//! where the CSS lookup yields nothing, read as a **boxed `gdk::RGBA`** and
//! deliberately never as the `AdwAccentColor` enum: Yaru adds
//! `ADW_ACCENT_COLOR_BROWN = ADW_ACCENT_COLOR_SLATE + 100`, outside
//! upstream's enumeration, so mapping the enum would abort or mis-name a
//! colour the RGBA reports exactly.
//!
//! ## Crash guard (E2b)
//!
//! `org.gnome.desktop.a11y.interface reduced-motion` is NEVER read. It is
//! new in gsettings-desktop-schemas, and constructing a `gio::Settings` for
//! a missing schema - or reading a missing key - **aborts the process**.
//! Every GSettings access below is guarded through
//! [`settings_for_schema_key`], which consults the schema source first.

use glib::translate::ToGlibPtr;
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;

use myna_platform::appearance::Rgb;

/// `org.gnome.desktop.interface`, home of both `accent-color` and the
/// `enable-animations` fallback.
pub(super) const INTERFACE_SCHEMA: &str = "org.gnome.desktop.interface";
pub(super) const ACCENT_KEY: &str = "accent-color";
/// Watched because a Yaru accent variant is selected by theme name.
pub(super) const GTK_THEME_KEY: &str = "gtk-theme";
pub(super) const ANIMATIONS_KEY: &str = "enable-animations";

/// `GtkSettings`' reduced-motion property (GTK ≥ 4.22).
pub(super) const GTK_REDUCED_MOTION_PROPERTY: &str = "gtk-interface-reduced-motion";
/// `GtkReducedMotion.no_preference` - the one value meaning "full motion".
const GTK_REDUCED_MOTION_NO_PREFERENCE: i32 = 0;
/// `AdwStyleManager`'s resolved accent - notified after the stylesheet is
/// updated, so the theme is already current when it fires.
pub(super) const ADW_ACCENT_RGBA_PROPERTY: &str = "accent-color-rgba";

/// Build a [`gio::Settings`] for `schema` only if the schema **and** `key`
/// both exist on this system; otherwise `None`.
///
/// This is the guard that keeps a missing schema/key from aborting the
/// process (E2b) - the reason the HUD may never touch a GSettings key
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
/// (`no_preference = 0`, `reduce = 1`) - reading it as a `bool` fails and
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
/// - [`crate::motion::reduced_motion`] owns that), schema/key guarded.
pub fn probe_enable_animations() -> Option<bool> {
    let settings = settings_for_schema_key(INTERFACE_SCHEMA, ANIMATIONS_KEY)?;
    Some(settings.boolean(ANIMATIONS_KEY))
}

/// `GtkSettings:gtk-enable-animations`, which GTK fills from XSETTINGS
/// (`Net/EnableAnimations`) on X11 and from the desktop's settings elsewhere.
pub fn probe_gtk_enable_animations() -> Option<bool> {
    Some(gtk::Settings::default()?.is_gtk_enable_animations())
}

/// Whether the desktop requests a higher-contrast UI (FR-022).
///
/// `Adw.StyleManager:high-contrast` - a plain bool libadwaita exposes (and
/// itself derives from `GtkSettings:gtk-interface-contrast` where available,
/// i.e. `gtk-interface-contrast` is just the GTK plumbing Adw builds on).
/// Looked up by runtime property name so the same binary works on older
/// libadwaita; `false` when the property is missing.
pub fn probe_high_contrast() -> bool {
    let manager = adw::StyleManager::default();
    let Some(property) = manager
        .find_property("high-contrast")
        .map(|p| p.name().to_string())
    else {
        return false;
    };
    manager
        .property_value(&property)
        .get::<bool>()
        .unwrap_or(false)
}

/// The accent as the **theme** resolves it, read back from `widget`'s
/// computed CSS `color` (the widget must be styled `color:
/// @accent_bg_color` - see `style.css`'s `.myna-hud-ribbon`).
///
/// This is the primary source: no version probing, no name table, and
/// correct on Yaru by construction. The widget must have a computed style
/// (i.e. be in a rooted hierarchy) for this to mean anything.
///
/// Components are clamped to `[0, 1]`: GTK's standalone accent variants can
/// legitimately fall outside sRGB (`@accent_color` was measured at
/// `b = -0.29`), and while `@accent_bg_color` is in gamut, the shader's
/// uniform space is not the place to discover otherwise.
pub fn probe_css_accent(widget: &impl IsA<gtk::Widget>) -> Option<Rgb> {
    let color = widget.as_ref().color();
    // A fully transparent colour means "no accent resolved" rather than a
    // real black; treat it as absent so the fallbacks get their turn.
    if color.alpha() <= 0.0 {
        return None;
    }
    Some(Rgb::clamped(
        color.red() as f64,
        color.green() as f64,
        color.blue() as f64,
    ))
}

/// The desktop's accent as libadwaita resolves it.
///
/// `adw_style_manager_get_accent_color_rgba()` (libadwaita ≥ 1.6, the
/// crate's floor) - a plain value read, needing no widget and no notion of
/// when a style was last recomputed. On Ubuntu it is also complete: the
/// Yaru patches feed accent *variants*, which are selected by theme name
/// rather than by the `accent-color` key, into this same property, so a
/// `Yaru-olive` desktop reports olive here.
///
/// Read as a `gdk::RGBA`, never as `AdwAccentColor`: Yaru adds
/// `ADW_ACCENT_COLOR_BROWN = ADW_ACCENT_COLOR_SLATE + 100`, outside
/// upstream's enumeration, which the Rust enum cannot represent.
pub fn probe_platform_accent() -> Option<Rgb> {
    let rgba = adw::StyleManager::default().accent_color_rgba();
    Some(Rgb::clamped(
        rgba.red() as f64,
        rgba.green() as f64,
        rgba.blue() as f64,
    ))
}
