//! accent — PURE accent-color resolution (feature 004, 2026-07-30
//! wave-ribbon redesign; research R18, mechanism amended R26; contract
//! extension.md X25). Resolves GNOME's desktop-wide accent-color preference
//! (`org.gnome.desktop.interface`'s `accent-color`) into the wave ribbon's
//! colour palette.
//!
//! The settings-backed half (schema/key-existence-guarded reads, `changed::`
//! subscriptions, the libadwaita style-manager probe) is application-layer
//! wiring; the reduced-motion resolution lives in [`crate::motion`] with
//! E2b's two-safe-sources design.
//!
//! Resolution order (the application layer probes; see
//! [`crate::platform::probe_accent_palette`]):
//!
//! 1. **The theme** — `@accent_bg_color` read back from a styled widget
//!    ([`resolve_theme_accent_palette`]). Direct, needs no version probing,
//!    and correct for Yaru's tints and variants by construction. This is
//!    the analogue of the extension's `-st-accent-color`.
//! 2. `AdwStyleManager:accent-color-rgba` (libadwaita ≥ 1.7) — measured
//!    identical to (1) for every accent.
//! 3. [`fallback_palette`] (Ubuntu orange), only if the desktop reports no
//!    accent whatsoever.
//!
//! **Removed (R18, 2026-08-26)**: the `accent-color` *name* table and the
//! GSettings read that fed it. Both existed to map a settings name onto a
//! colour ourselves, and the theme already answers that question — better,
//! since it also covers Yaru variants and `wartybrown`, which has no
//! upstream enum member. The associated "untouched default is not a
//! choice" rule went with it: its premise (that an untouched Ubuntu
//! desktop reads `'blue'` while looking orange) was false —
//! `ubuntu-settings` ships a gschema override setting
//! `accent-color = 'orange'` — and the rule mis-tinted Yaru accent
//! variants back to plain orange.
//!
use crate::shader::{hex_to_rgb, Rgb, RibbonPalette};

/// The last-resort colour, used only when the desktop reports no accent at
/// all — the design decision doc's "default Ubuntu orange".
pub const UBUNTU_ORANGE: &str = "#E95420";

/// The design decision doc's specific override: when the ribbon's primary
/// colour is orange, the darker/complementary secondary tone is a fixed
/// Ubuntu-brand aubergine, not a generically-computed colour-wheel
/// complement (2026-07-30 /speckit-analyze finding U2 — this had been
/// dropped to a generic rule across the derived spec artifacts; reinstated
/// to match the source design brief).
pub const UBUNTU_AUBERGINE: &str = "#77216F";

/// The resolved ribbon palette (hex strings; convert to the shader's
/// [`RibbonPalette`] with [`AccentPalette::as_ribbon_palette`]).
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
/// (this module's own rule; non-panicking by design).
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

/// The palette used when the desktop reports no accent at all.
pub fn fallback_palette() -> AccentPalette {
    derive_palette(UBUNTU_ORANGE, true)
}

/// The two hexes that count as "orange" for the aubergine override: Yaru's
/// (= [`UBUNTU_ORANGE`]) and upstream libadwaita's.
///
/// Orangeness used to be decided by the settings *name*; resolving the
/// accent from the theme means there is no name to consult, so it is
/// decided by the colour itself.
pub fn is_orange_hex(hex: &str) -> bool {
    let hex = hex.to_ascii_lowercase();
    hex == UBUNTU_ORANGE.to_ascii_lowercase() || hex == "#ed5b00"
}

/// Resolve the palette from the accent **the theme itself reports**
/// (`@accent_bg_color`, read back through
/// [`crate::platform::probe_css_accent`]).
///
/// This is the primary path, and it needs no notion of "did the user
/// choose": the theme reports what the desktop is actually using — Ubuntu
/// orange on an untouched Ubuntu, blue on untouched stock GNOME, a Yaru
/// variant where one is in use, and a deliberate choice where one was
/// made.
pub fn resolve_theme_accent_palette(accent: Rgb) -> AccentPalette {
    let main_hex = rgb255_to_hex(
        (accent.r * 255.0).round() as u32,
        (accent.g * 255.0).round() as u32,
        (accent.b * 255.0).round() as u32,
    );
    let is_orange = is_orange_hex(&main_hex);
    derive_palette(&main_hex, is_orange)
}
