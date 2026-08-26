//! accent — PURE accent-color resolution (feature 004, 2026-07-30
//! wave-ribbon redesign; research R18, mechanism amended R26; contract
//! extension.md X25). Resolves GNOME's desktop-wide accent-color preference
//! (`org.gnome.desktop.interface`'s `accent-color`) into the wave ribbon's
//! colour palette.
//!
//! Ported 1:1 from `extensions/myna-shell/accent.js`'s pure half. The live,
//! settings-backed half (schema/key-existence-guarded reads, `changed::`
//! subscriptions, the libadwaita style-manager probe) is application-layer
//! wiring; the reduced-motion resolution that used to live here moved to
//! [`crate::motion`] with E2b's two-safe-sources design.
//!
//! CRITICAL correctness rule (R18): the GSettings **user value**, not the
//! resolved value, is what lets "the user genuinely chose blue" and "the
//! user never touched this setting" (also `'blue'`, GNOME's factory
//! default) be told apart — both read identically any other way. The
//! caller therefore passes the *user value* (`None` = never written),
//! never the resolved setting.
//!
//! R26 adds the platform path: when the runtime libadwaita is ≥ 1.7, the
//! style manager's `accent-color-rgba` property resolves the accent
//! (including Ubuntu's Yaru tints, which the fixed table cannot name); a
//! genuine user choice then uses that color as `main` via
//! [`resolve_platform_accent_palette`]. The untouched-default rule still
//! wins over platform resolution.

use crate::shader::{hex_to_rgb, Rgb, RibbonPalette};

/// The 9-value libadwaita `Adw.AccentColor` palette (GNOME 47+), from
/// libadwaita's `_colors.scss` / `adw_accent_color_to_rgba`.
pub fn accent_hex(name: &str) -> Option<&'static str> {
    match name {
        "blue" => Some("#3584e4"),
        "teal" => Some("#2190a4"),
        "green" => Some("#3a944a"),
        "yellow" => Some("#c88800"),
        "orange" => Some("#ed5b00"),
        "red" => Some("#e62d42"),
        "pink" => Some("#d56199"),
        "purple" => Some("#9141ac"),
        "slate" => Some("#6f8396"),
        _ => None,
    }
}

/// Every accent name the table knows, for exhaustive iteration in tests.
pub const ACCENT_NAMES: [&str; 9] = [
    "blue", "teal", "green", "yellow", "orange", "red", "pink", "purple", "slate",
];

/// Fallback when the user has not actively chosen an accent colour
/// (untouched default, or the schema/key is unavailable on an older stack) —
/// the design decision doc's "default Ubuntu orange".
pub const UBUNTU_ORANGE: &str = "#E95420";

/// The design decision doc's specific override: when the ribbon's primary
/// colour is orange, the darker/complementary secondary tone is a fixed
/// Ubuntu-brand aubergine, not a generically-computed colour-wheel
/// complement (2026-07-30 /speckit-analyze finding U2 — this had been
/// dropped to a generic rule across the derived spec artifacts; reinstated
/// to match the source design brief).
pub const UBUNTU_AUBERGINE: &str = "#77216F";

/// The resolved ribbon palette (hex strings, the GJS shape; convert to the
/// shader's [`RibbonPalette`] with [`AccentPalette::as_ribbon_palette`]).
#[derive(Clone, Debug, PartialEq)]
pub struct AccentPalette {
    pub main: String,
    pub highlight: String,
    pub darker_complement: String,
    /// The "translucent secondary strand" is the same main colour at reduced
    /// alpha, applied by the renderer — not a separate hue.
    pub translucent_alpha: f64,
}

impl AccentPalette {
    /// The main colour as an 0..=1 [`Rgb`] (the shader's uniform space).
    pub fn main_rgb(&self) -> Rgb {
        hex_to_rgb(&self.main)
    }

    /// Convert to the shader module's palette type.
    pub fn as_ribbon_palette(&self) -> RibbonPalette {
        RibbonPalette {
            main: hex_to_rgb(&self.main),
            highlight: hex_to_rgb(&self.highlight),
            darker_complement: hex_to_rgb(&self.darker_complement),
            translucent_alpha: self.translucent_alpha,
        }
    }
}

fn clamp01(x: f64) -> f64 {
    x.clamp(0.0, 1.0)
}

/// Parse `#rrggbb` into 0-255 components; invalid input degrades to black
/// (this module's own rule — the painter's color parser degraded to white;
/// both are non-panicking by design).
fn hex_to_rgb255(hex: &str) -> (u32, u32, u32) {
    let t = hex.trim();
    let h = t.strip_prefix('#').unwrap_or(t);
    if h.len() != 6 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return (0, 0, 0);
    }
    let n = u32::from_str_radix(h, 16).unwrap_or(0);
    ((n >> 16) & 0xff, (n >> 8) & 0xff, n & 0xff)
}

fn rgb255_to_hex(r: u32, g: u32, b: u32) -> String {
    let c = |v: u32| format!("{:02x}", (clamp01(v as f64 / 255.0) * 255.0).round() as u32);
    format!("#{}{}{}", c(r), c(g), c(b))
}

fn rgb_to_hsl(r: u32, g: u32, b: u32) -> (f64, f64, f64) {
    let (rn, gn, bn) = (r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0);
    let max = rn.max(gn).max(bn);
    let min = rn.min(gn).min(bn);
    let l = (max + min) / 2.0;
    if (max - min).abs() < f64::EPSILON {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if (max - rn).abs() < f64::EPSILON {
        (gn - bn) / d + if gn < bn { 6.0 } else { 0.0 }
    } else if (max - gn).abs() < f64::EPSILON {
        (bn - rn) / d + 2.0
    } else {
        (rn - gn) / d + 4.0
    };
    (h / 6.0, s, l)
}

fn hue2rgb(p: f64, q: f64, t: f64) -> f64 {
    let t = if t < 0.0 {
        t + 1.0
    } else if t > 1.0 {
        t - 1.0
    } else {
        t
    };
    if t < 1.0 / 6.0 {
        return p + (q - p) * 6.0 * t;
    }
    if t < 1.0 / 2.0 {
        return q;
    }
    if t < 2.0 / 3.0 {
        return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
    }
    p
}

fn hsl_to_rgb255(h: f64, s: f64, l: f64) -> (u32, u32, u32) {
    if s == 0.0 {
        let v = (l * 255.0).round() as u32;
        return (v, v, v);
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let r = (hue2rgb(p, q, h + 1.0 / 3.0) * 255.0).round() as u32;
    let g = (hue2rgb(p, q, h) * 255.0).round() as u32;
    let b = (hue2rgb(p, q, h - 1.0 / 3.0) * 255.0).round() as u32;
    (r, g, b)
}

/// Lighten a hex colour toward white by `amount` in `[0,1]` (the highlight
/// tone).
fn lighten(hex: &str, amount: f64) -> String {
    let (r, g, b) = hex_to_rgb255(hex);
    rgb255_to_hex(
        (r as f64 + (255.0 - r as f64) * amount).round() as u32,
        (g as f64 + (255.0 - g as f64) * amount).round() as u32,
        (b as f64 + (255.0 - b as f64) * amount).round() as u32,
    )
}

/// True colour-wheel complement (hue rotated 180°), lightness pulled down
/// slightly.
fn complement(hex: &str) -> String {
    let (r, g, b) = hex_to_rgb255(hex);
    let (h, s, l) = rgb_to_hsl(r, g, b);
    let (r, g, b) = hsl_to_rgb255((h + 0.5) % 1.0, s, clamp01(l * 0.75));
    rgb255_to_hex(r, g, b)
}

/// Derive the ribbon's full palette from one resolved main colour.
///
/// Port of `accent.js`'s `derivePalette`.
pub fn derive_palette(main_hex: &str, is_orange: bool) -> AccentPalette {
    AccentPalette {
        main: main_hex.to_string(),
        highlight: lighten(main_hex, 0.55),
        darker_complement: if is_orange {
            UBUNTU_AUBERGINE.to_string()
        } else {
            complement(main_hex)
        },
        translucent_alpha: 0.35,
    }
}

fn ubuntu_orange_palette() -> AccentPalette {
    derive_palette(UBUNTU_ORANGE, true)
}

/// Resolve the ribbon's palette from the *result* of reading the
/// `accent-color` GSettings **user value** — a GNOME accent name string if
/// the user has genuinely set one, or `None` if they never have (including
/// sitting on the untouched factory default, itself `'blue'` — R18's core
/// distinction) or the schema/key is unavailable.
///
/// Never panics: an unrecognized name also falls back to Ubuntu orange.
///
/// Port of `accent.js`'s `resolveAccentPalette`.
pub fn resolve_accent_palette(user_value: Option<&str>) -> AccentPalette {
    let Some(name) = user_value else {
        return ubuntu_orange_palette();
    };
    match accent_hex(name) {
        Some(hex) => derive_palette(hex, name == "orange"),
        None => ubuntu_orange_palette(),
    }
}

/// The R26 platform path: a genuine user choice plus the style manager's
/// own resolved accent (`AdwStyleManager:accent-color-rgba`, libadwaita
/// ≥ 1.7 — picks up Ubuntu's Yaru tints, which the fixed table cannot
/// name). The user value still gates everything: `None` (untouched default)
/// is Ubuntu orange regardless of what the platform reports as its default.
///
/// Orangeness for the aubergine override is by name ("orange") — the one
/// case the design brief calls out.
pub fn resolve_platform_accent_palette(user_value: Option<&str>, platform: Rgb) -> AccentPalette {
    let Some(name) = user_value else {
        return ubuntu_orange_palette();
    };
    let is_orange = name == "orange";
    // The platform-resolved color becomes main (as hex, keeping the
    // palette's string shape), with the dependent tones derived from it.
    let main_hex = rgb255_to_hex(
        (platform.r * 255.0).round() as u32,
        (platform.g * 255.0).round() as u32,
        (platform.b * 255.0).round() as u32,
    );
    derive_palette(&main_hex, is_orange)
}
