//! motion — PURE reduced-motion resolution rules (R26, data-model E2b,
//! FR-022a).
//!
//! The user's system-wide reduced-motion preference selects between the
//! flowing wave ribbon and its static/minimal-motion alternative. The
//! resolution order (the application layer probes and fills
//! [`MotionReadings`]):
//!
//! 1. **Primary**: `GtkSettings:gtk-interface-reduced-motion` (GTK ≥ 4.22;
//!    populated by GDK from the settings portal's
//!    `org.freedesktop.appearance reduced-motion`), looked up by *runtime
//!    GObject property name* — no compile-time version feature, because the
//!    runtime matrix spans the snap's GTK 4.18 (absent there → fallback),
//!    26.04 hosts (present) and the 24.04 workshop (absent).
//! 2. **Fallback**: the classic `org.gnome.desktop.interface`
//!    `enable-animations` GSettings key, inverted, schema/key-existence
//!    guarded (R19's original mechanism).
//! 3. **Default**: full motion when neither source is available.
//!
//! **Crash-on-start guard (E2b)**: the new
//! `org.gnome.desktop.a11y.interface reduced-motion` GSettings key is NEVER
//! read directly — it is new in gsettings-desktop-schemas and absent on
//! older systems, and an unguarded `Settings` construction/read against a
//! missing schema/key aborts the process. This is why the struct below has
//! exactly the two safe sources and nothing else.
//!
//! Live updates (the app layer re-resolves on `notify::` signals) and the
//! rendering choice itself are out of scope here — this module only decides
//! the boolean.

/// The raw readings from whichever sources the runtime probing found
/// (`None` = source absent on this stack). Filled by the application layer.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MotionReadings {
    /// `GtkSettings:gtk-interface-reduced-motion`, as a plain boolean
    /// (`Reduce` → `true`, `NoPreference` → `false`); `None` when the
    /// property does not exist (GTK < 4.22).
    pub gtk_reduced_motion: Option<bool>,
    /// `org.gnome.desktop.interface`'s `enable-animations` (NOT inverted
    /// here — the raw value); `None` when the schema/key is absent.
    pub enable_animations: Option<bool>,
}

/// Whether the user's system-wide reduced-motion preference is on: the
/// newer GtkSettings property decides when present, else the inverted
/// `enable-animations` key, else full motion.
pub fn reduced_motion(readings: &MotionReadings) -> bool {
    match readings.gtk_reduced_motion {
        Some(reduce) => reduce,
        None => !readings.enable_animations.unwrap_or(true),
    }
}
