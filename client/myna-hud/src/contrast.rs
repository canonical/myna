//! WCAG contrast checks over the pill's own stylesheet (FR-013, SC-005).
//!
//! This lives beside `style.css` rather than in `myna-desktop` because the
//! previous home outlived its subject: the check was written against the
//! GNOME Shell extension's `stylesheet.css`, and when the HUD became this
//! standalone renderer that file went away. The constants stayed, so the
//! gate kept passing while guarding colours nothing drew any more.
//!
//! `include_str!` is what stops that recurring. The stylesheet is compiled
//! in, and [`tests::every_checked_colour_is_still_declared_in_the_stylesheet`]
//! asserts each literal below still appears in it — so a colour that moves
//! fails the build at the moment it diverges, not merely if it later drops
//! under a threshold. Deleting the file is a compile error.
//!
//! Two things here are deliberately not checked, because checking them
//! would mean asserting a value this crate does not choose:
//!
//! - The severity and accent colours resolve from libadwaita variables
//!   (`--warning-bg-color`, `--error-bg-color`, `--accent-bg-color`) and
//!   differ per theme and per accent preference. Severity is never carried
//!   by colour alone (FR-009) — the label names the state in words — so the
//!   contrast obligation on them is the platform's, not ours.
//! - `color-mix(... , black)` backgrounds derive from those same variables.

/// The shipped stylesheet, compiled in so these checks cannot outlive it.
/// Test-only: the drift guard is the sole reader, and `pill.rs` loads the
/// same file through its own `include_str!` for the real thing.
#[cfg(test)]
const STYLE_CSS: &str = include_str!("style.css");

/// An RGBA colour with 0.0–1.0 channels (sRGB, non-premultiplied alpha).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

impl Rgba {
    pub const fn new(r: f64, g: f64, b: f64, a: f64) -> Self {
        Self { r, g, b, a }
    }

    /// A `#rrggbb` literal, opaque.
    pub const fn hex(r: u8, g: u8, b: u8) -> Self {
        Self::new(r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0, 1.0)
    }

    /// Alpha-composite `self` over an opaque `background`, returning the
    /// resulting opaque colour.
    pub fn over(&self, background: Rgba) -> Rgba {
        Rgba {
            r: self.r * self.a + background.r * (1.0 - self.a),
            g: self.g * self.a + background.g * (1.0 - self.a),
            b: self.b * self.a + background.b * (1.0 - self.a),
            a: 1.0,
        }
    }

    /// WCAG relative luminance (sRGB → linear, then the standard weights).
    pub fn relative_luminance(&self) -> f64 {
        fn channel(c: f64) -> f64 {
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * channel(self.r) + 0.7152 * channel(self.g) + 0.0722 * channel(self.b)
    }
}

pub const WHITE: Rgba = Rgba::new(1.0, 1.0, 1.0, 1.0);
pub const BLACK: Rgba = Rgba::new(0.0, 0.0, 0.0, 1.0);

/// The WCAG contrast ratio between two opaque colours (always ≥ 1.0).
pub fn contrast_ratio(a: Rgba, b: Rgba) -> f64 {
    let (l1, l2) = (a.relative_luminance(), b.relative_luminance());
    let (lighter, darker) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
    (lighter + 0.05) / (darker + 0.05)
}

/// The worst-case ratio of `foreground` over `background` when `background`
/// may itself be translucent over an unknown desktop surface — composited
/// over both white and black, taking the lower result. The pill floats over
/// whatever the user has on screen, so the wallpaper is not ours to assume.
pub fn worst_case_contrast(foreground: Rgba, background: Rgba) -> f64 {
    let bg_over_white = background.over(WHITE);
    let bg_over_black = background.over(BLACK);
    let ratio_on_white = contrast_ratio(foreground.over(bg_over_white), bg_over_white);
    let ratio_on_black = contrast_ratio(foreground.over(bg_over_black), bg_over_black);
    ratio_on_white.min(ratio_on_black)
}

/// The colour pairs `style.css` declares, each paired with the literal it is
/// transcribed from so the drift guard can find it.
pub mod declared {
    use super::Rgba;

    /// `.myna-hud-pill { background-color: #2f2f2f; }` — gnome-shell's
    /// `$osd_bg_color`. Opaque, so the worst case is the colour itself.
    pub const PILL_BACKGROUND: Rgba = Rgba::hex(0x2f, 0x2f, 0x2f);
    pub const PILL_BACKGROUND_CSS: &str = "background-color: #2f2f2f;";

    /// `.myna-hud-label { color: #ffffff; }` — text, so the 4.5:1 threshold.
    pub const LABEL_TEXT: Rgba = Rgba::hex(0xff, 0xff, 0xff);
    pub const LABEL_TEXT_CSS: &str = "color: #ffffff;";

    /// `.myna-hud-icon { color: #ffffff; }` — a meaningful non-text element
    /// (the mic glyph), so the 3:1 threshold.
    pub const ICON: Rgba = Rgba::hex(0xff, 0xff, 0xff);

    /// `.myna-hud-dismiss { color: rgba(255, 255, 255, 0.75); }` — the
    /// dismiss affordance at rest, dimmer than the label and still a
    /// meaningful non-text element.
    pub const DISMISS: Rgba = Rgba::new(1.0, 1.0, 1.0, 0.75);
    pub const DISMISS_CSS: &str = "color: rgba(255, 255, 255, 0.75);";

    /// `.myna-hud-pill.myna-hud-high-contrast { background-color: rgba(0, 0, 0, 0.95); }`
    pub const HIGH_CONTRAST_BACKGROUND: Rgba = Rgba::new(0.0, 0.0, 0.0, 0.95);
    pub const HIGH_CONTRAST_BACKGROUND_CSS: &str = "background-color: rgba(0, 0, 0, 0.95);";
}

#[cfg(test)]
mod tests {
    use super::declared::*;
    use super::*;

    #[test]
    fn identical_colours_have_a_ratio_of_one() {
        assert!((contrast_ratio(WHITE, WHITE) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn black_on_white_is_maximal() {
        assert!((contrast_ratio(BLACK, WHITE) - 21.0).abs() < 1e-6);
    }

    /// The guard that the old gate lacked: if a colour below is edited in
    /// `style.css` without being edited here, this fails immediately —
    /// rather than the pair silently describing a colour nothing renders.
    #[test]
    fn every_checked_colour_is_still_declared_in_the_stylesheet() {
        for (what, literal) in [
            ("pill background", PILL_BACKGROUND_CSS),
            ("label text", LABEL_TEXT_CSS),
            ("dismiss button", DISMISS_CSS),
            ("high-contrast background", HIGH_CONTRAST_BACKGROUND_CSS),
        ] {
            assert!(
                STYLE_CSS.contains(literal),
                "{what}: style.css no longer declares `{literal}` — the contrast \
                 check above is describing a colour the HUD does not draw. \
                 Re-read the stylesheet and update `declared`."
            );
        }
    }

    #[test]
    fn label_text_on_the_pill_meets_the_text_threshold() {
        let ratio = worst_case_contrast(LABEL_TEXT, PILL_BACKGROUND);
        assert!(
            ratio >= 4.5,
            "label text contrast {ratio} is below the 4.5:1 text threshold (FR-013)"
        );
    }

    #[test]
    fn the_icon_on_the_pill_meets_the_non_text_threshold() {
        let ratio = worst_case_contrast(ICON, PILL_BACKGROUND);
        assert!(
            ratio >= 3.0,
            "icon contrast {ratio} is below the 3:1 non-text threshold (FR-013)"
        );
    }

    #[test]
    fn the_dismiss_affordance_meets_the_non_text_threshold() {
        let ratio = worst_case_contrast(DISMISS, PILL_BACKGROUND);
        assert!(
            ratio >= 3.0,
            "dismiss contrast {ratio} is below the 3:1 non-text threshold (FR-013)"
        );
    }

    /// High contrast must not be a downgrade: the mode exists to help, so
    /// it is checked against the stricter text threshold too.
    #[test]
    fn the_high_contrast_pill_keeps_its_label_legible() {
        let ratio = worst_case_contrast(LABEL_TEXT, HIGH_CONTRAST_BACKGROUND);
        assert!(
            ratio >= 4.5,
            "high-contrast label contrast {ratio} is below the 4.5:1 threshold (FR-013)"
        );
    }
}
