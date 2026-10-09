//! Appearance: the desktop preferences Myna's surfaces follow.
//!
//! A backend reports what the desktop says; what to draw from it (the
//! palette, the fallback accent, the animation policy) stays with the caller.

use crate::Subscription;

/// An sRGB colour, components in `[0, 1]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgb {
    pub r: f64,
    pub g: f64,
    pub b: f64,
}

impl Rgb {
    /// Clamp each component into gamut: themes may resolve a colour outside
    /// it.
    pub fn clamped(r: f64, g: f64, b: f64) -> Self {
        let clamp = |c: f64| if c.is_nan() { 0.0 } else { c.clamp(0.0, 1.0) };
        Self {
            r: clamp(r),
            g: clamp(g),
            b: clamp(b),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AppearanceReadings {
    /// The desktop's accent; `None` where it names none.
    pub accent: Option<Rgb>,
    pub reduced_motion: bool,
    pub high_contrast: bool,
    /// The desktop's theme is a dark one.
    pub prefers_dark: bool,
}

/// Whether readings taken inside a `watch` callback already reflect the
/// change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Freshness {
    /// The desktop applied the change before notifying; read now.
    Current,
    /// The styling may lag the notification; read at the next frame.
    NextFrame,
}

/// The desktop's appearance preferences.
///
/// Calls and callbacks happen on the caller's main loop.
pub trait Appearance {
    fn read(&self) -> AppearanceReadings;

    /// Call `changed` whenever any reading may have changed.
    fn watch(&self, changed: Box<dyn Fn(Freshness)>) -> Subscription;
}

/// Whether a GTK theme name is one of the high-contrast ones, the way Xfce
/// offers high contrast: by theme name, which xfsettingsd exports as
/// `Net/ThemeName`.
pub fn is_high_contrast_theme(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.contains("highcontrast") || name.contains("high-contrast")
}

/// Whether a GTK theme name is a dark variant: a `dark` or `darker` word
/// (`Adwaita-dark`, `Greybird-dark`, `Arc-Darker`, `Adwaita:dark`) or the
/// high-contrast inverse.
pub fn is_dark_theme(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.contains("highcontrastinverse")
        || name
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|word| matches!(word, "dark" | "darker"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn dark_themes_are_recognised_by_name() {
        for name in [
            "Adwaita-dark",
            "Greybird-dark",
            "Arc-Darker",
            "Adwaita:dark",
            "Materia-dark-compact",
            "Yaru-blue-dark",
            "HighContrastInverse",
            "dark",
        ] {
            assert!(is_dark_theme(name), "{name}");
        }
        for name in [
            "Adwaita",
            "Greybird",
            "HighContrast",
            "Yaru",
            "Darkroom",
            "",
        ] {
            assert!(!is_dark_theme(name), "{name}");
        }
    }

    #[test]
    fn high_contrast_themes_are_recognised_by_name() {
        for name in [
            "HighContrast",
            "HighContrastInverse",
            "Yaru-highcontrast",
            "high-contrast",
        ] {
            assert!(is_high_contrast_theme(name), "{name}");
        }
        for name in ["Adwaita", "Greybird", "Yaru-dark", ""] {
            assert!(!is_high_contrast_theme(name), "{name}");
        }
    }

    use super::*;

    #[test]
    fn colours_are_clamped_into_gamut() {
        assert_eq!(
            Rgb::clamped(-0.29, 0.5, 1.2),
            Rgb {
                r: 0.0,
                g: 0.5,
                b: 1.0
            }
        );
        assert_eq!(Rgb::clamped(f64::NAN, 0.0, 1.0).r, 0.0);
    }

    #[test]
    fn a_desktop_that_says_nothing_reads_as_defaults() {
        assert_eq!(
            AppearanceReadings::default(),
            AppearanceReadings {
                accent: None,
                reduced_motion: false,
                high_contrast: false,
                prefers_dark: false,
            }
        );
    }
}
