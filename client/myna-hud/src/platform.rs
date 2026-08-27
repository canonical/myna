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
//! The **accent is read from CSS**, not from the settings name: a widget
//! styled `color: @accent_bg_color` is asked for its computed colour
//! ([`probe_css_accent`]). That is the direct analogue of the extension's
//! `-st-accent-color`, and it beats every alternative — the named colour
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
//! a missing schema — or reading a missing key — **aborts the process**.
//! Every GSettings access below is guarded through
//! [`settings_for_schema_key`], which consults the schema source first.

use glib::translate::ToGlibPtr;
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;

use crate::accent::{fallback_palette, resolve_theme_accent_palette, AccentPalette};
use crate::motion::{reduced_motion, MotionReadings};
use crate::shader::Rgb;

/// `org.gnome.desktop.interface`, home of both `accent-color` and the
/// `enable-animations` fallback.
const INTERFACE_SCHEMA: &str = "org.gnome.desktop.interface";
const ACCENT_KEY: &str = "accent-color";
/// Watched because a Yaru accent variant is selected by theme name.
const GTK_THEME_KEY: &str = "gtk-theme";
const ANIMATIONS_KEY: &str = "enable-animations";

/// `GtkSettings`' reduced-motion property (GTK ≥ 4.22).
const GTK_REDUCED_MOTION_PROPERTY: &str = "gtk-interface-reduced-motion";
/// `GtkReducedMotion.no_preference` — the one value meaning "full motion".
const GTK_REDUCED_MOTION_NO_PREFERENCE: i32 = 0;
/// `AdwStyleManager`'s resolved accent — notified after the stylesheet is
/// updated, so the theme is already current when it fires.
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

/// The accent as the **theme** resolves it, read back from `widget`'s
/// computed CSS `color` (the widget must be styled `color:
/// @accent_bg_color` — see `style.css`'s `.myna-hud-ribbon`).
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
    Some(Rgb {
        r: color.red().clamp(0.0, 1.0) as f64,
        g: color.green().clamp(0.0, 1.0) as f64,
        b: color.blue().clamp(0.0, 1.0) as f64,
    })
}

/// The desktop's accent as libadwaita resolves it.
///
/// `adw_style_manager_get_accent_color_rgba()` (libadwaita ≥ 1.6, the
/// crate's floor) — a plain value read, needing no widget and no notion of
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
    Some(Rgb {
        r: rgba.red().clamp(0.0, 1.0) as f64,
        g: rgba.green().clamp(0.0, 1.0) as f64,
        b: rgba.blue().clamp(0.0, 1.0) as f64,
    })
}

/// The ribbon's palette for the current desktop, resolved from the theme
/// where possible and from the fixed table only as a last resort.
///
/// The untouched-default rule (Ubuntu orange) wins over platform resolution
/// in both paths — that decision lives in [`crate::accent`].
/// `accent_widget` is the widget carrying `color: @accent_bg_color`; pass
/// `None` where no styled widget is available yet.
///
/// Order: the style manager's accent, then the theme's `@accent_bg_color`,
/// then Ubuntu orange.
///
/// The style manager comes first because it is a plain value read —
/// `AdwStyleManager:accent-color-rgba`, equivalent to
/// `adw_style_manager_get_accent_color_rgba()` — with no dependence on a
/// widget being rooted or on when its style was last recomputed. It is also
/// complete on Ubuntu: the Yaru patches feed accent *variants* (selected by
/// theme name) into the same `accent-color` property, so a `Yaru-olive`
/// desktop reports olive here.
///
/// (The typed getter is `Since: 1.6`, and using it would mean enabling
/// `libadwaita/v1_6` — a compile-time floor the runtime matrix cannot take,
/// since the 24.04 workshop that builds this workspace has libadwaita 1.5.
/// Probing the property by name costs nothing and degrades to the CSS path
/// there instead.)
///
/// The theme is the fallback for exactly that case, and for a stylesheet
/// that defines its own `@accent_bg_color` independently of the accent
/// preference. Neither path needs to guess whether the user "chose"
/// anything, which is why the `accent-color` name table is gone.
pub fn probe_accent_palette(accent_widget: Option<&impl IsA<gtk::Widget>>) -> AccentPalette {
    if let Some(accent) =
        probe_platform_accent().or_else(|| accent_widget.and_then(probe_css_accent))
    {
        return resolve_theme_accent_palette(accent);
    }
    fallback_palette()
}

/// When a trigger fires, whether the theme's accent can be trusted to be
/// current *already*.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccentReadiness {
    /// The new styling is already installed, so the accent may be read
    /// straight away.
    ///
    /// Emitted for libadwaita's own notification, where the ordering is
    /// guaranteed by construction: `notify_accent_color_cb()` calls
    /// `update_stylesheet (self, UPDATE_ACCENT_COLOR)` — which reloads the
    /// provider holding `@accent_bg_color` — *before* notifying
    /// `accent-color`/`accent-color-rgba` (libadwaita
    /// `src/adw-style-manager.c`).
    Current,
    /// Something changed, but the styling may not have caught up: against a
    /// raw GSettings key we have no ordering guarantee versus libadwaita's
    /// own handler for that same key, and GTK recomputes styles lazily. The
    /// accent must be re-read at the next frame.
    NextFrame,
}

/// Call `on_change` whenever a preference that affects the HUD may have
/// changed, telling it whether the accent is readable yet.
///
/// The returned guard owns the subscriptions; dropping it disconnects
/// everything, so no callback can outlive the window.
pub fn watch_preferences<F: Fn(AccentReadiness) + 'static + Clone>(
    on_change: F,
) -> PreferenceWatch {
    let mut settings_handles = Vec::new();

    // Watched as a change TRIGGER only — the value is never read from here
    // (the theme is the source). Same for the theme name, which is how a
    // Yaru accent variant changes.
    for key in [ACCENT_KEY, GTK_THEME_KEY] {
        if let Some(settings) = settings_for_schema_key(INTERFACE_SCHEMA, key) {
            let cb = on_change.clone();
            settings.connect_changed(Some(key), move |_, _| cb(AccentReadiness::NextFrame));
            settings_handles.push(settings);
        }
    }
    if let Some(settings) = settings_for_schema_key(INTERFACE_SCHEMA, ANIMATIONS_KEY) {
        let cb = on_change.clone();
        settings.connect_changed(Some(ANIMATIONS_KEY), move |_, _| {
            cb(AccentReadiness::NextFrame)
        });
        settings_handles.push(settings);
    }

    let manager = adw::StyleManager::default();
    let adw_handle = {
        let cb = on_change.clone();
        // libadwaita reloads the accent provider before emitting this, so
        // the theme already reports the new colour here.
        Some(
            manager.connect_notify_local(Some(ADW_ACCENT_RGBA_PROPERTY), move |_, _| {
                cb(AccentReadiness::Current)
            }),
        )
    };

    let mut gtk_handle = None;
    let gtk_settings = gtk::Settings::default();
    if let Some(settings) = &gtk_settings {
        if settings
            .find_property(GTK_REDUCED_MOTION_PROPERTY)
            .is_some()
        {
            let cb = on_change.clone();
            gtk_handle = Some(
                settings.connect_notify_local(Some(GTK_REDUCED_MOTION_PROPERTY), move |_, _| {
                    cb(AccentReadiness::NextFrame)
                }),
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
