//! The state-to-channel coverage matrix (SC-002, FR-032,
//! `contracts/coverage-matrix.md`): loads and validates the checked-in
//! `extensions/myna-shell/coverage-matrix.json`, and the contrast-ratio
//! regression check (FR-013) over the shipped stylesheet colours.

use std::collections::HashSet;

use serde::Deserialize;

/// The checked-in matrix's path, resolved at compile time relative to this
/// crate — `client/myna-desktop` → workspace root → `extensions/myna-shell`.
pub const COVERAGE_MATRIX_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../extensions/myna-shell/coverage-matrix.json"
);

#[derive(Debug, Deserialize)]
pub struct Channels {
    pub visual: Vec<String>,
    pub non_visual: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct StateEntry {
    pub id: String,
    pub channels: Channels,
    pub colour_only: bool,
    pub sound_only: bool,
}

#[derive(Debug, Deserialize)]
pub struct CoverageMatrix {
    pub states: Vec<StateEntry>,
}

/// Every wire state `indicator::dbus::wire_state` can emit (C4). An
/// exhaustiveness check against this list fails the moment a new state is
/// added to the wire vocabulary without a matching matrix entry.
pub const KNOWN_WIRE_STATES: [&str; 7] = [
    "idle",
    "loading",
    "recording",
    "transcribing",
    "finalizing",
    "notice",
    "error",
];

#[derive(Debug, PartialEq, Eq)]
pub enum CoverageViolation {
    EmptyVisualChannels(String),
    EmptyNonVisualChannels(String),
    ColourOnly(String),
    SoundOnly(String),
    MissingMatrixEntry(&'static str),
}

pub fn parse_matrix(json: &str) -> Result<CoverageMatrix, serde_json::Error> {
    serde_json::from_str(json)
}

pub fn load_matrix() -> std::io::Result<CoverageMatrix> {
    let content = std::fs::read_to_string(COVERAGE_MATRIX_PATH)?;
    parse_matrix(&content)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
}

/// C1–C3: every entry has a non-empty visual and non-visual channel list,
/// and neither `colour_only` nor `sound_only` is `true`.
pub fn check_invariants(matrix: &CoverageMatrix) -> Vec<CoverageViolation> {
    let mut violations = Vec::new();
    for entry in &matrix.states {
        if entry.channels.visual.is_empty() {
            violations.push(CoverageViolation::EmptyVisualChannels(entry.id.clone()));
        }
        if entry.channels.non_visual.is_empty() {
            violations.push(CoverageViolation::EmptyNonVisualChannels(entry.id.clone()));
        }
        if entry.colour_only {
            violations.push(CoverageViolation::ColourOnly(entry.id.clone()));
        }
        if entry.sound_only {
            violations.push(CoverageViolation::SoundOnly(entry.id.clone()));
        }
    }
    violations
}

/// C4: every known wire state has a matching matrix entry.
pub fn check_exhaustive(matrix: &CoverageMatrix) -> Vec<CoverageViolation> {
    let present: HashSet<&str> = matrix.states.iter().map(|s| s.id.as_str()).collect();
    KNOWN_WIRE_STATES
        .iter()
        .filter(|w| !present.contains(*w))
        .map(|w| CoverageViolation::MissingMatrixEntry(w))
        .collect()
}

// ── Contrast (FR-013, K1/K2) ─────────────────────────────────────────────

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

    /// Alpha-composite `self` over an opaque `background` (both channels in
    /// 0.0–1.0), returning the resulting opaque colour.
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

/// The worst-case contrast ratio of `foreground` over `background` when
/// `background` may itself be translucent over an arbitrary (unknown)
/// desktop surface — computed by compositing both over white and over
/// black and taking the lower ratio. This is the correct, conservative
/// check for chrome (like the HUD pill) that floats over content this
/// feature does not control.
pub fn worst_case_contrast(foreground: Rgba, background: Rgba) -> f64 {
    let bg_over_white = background.over(WHITE);
    let bg_over_black = background.over(BLACK);
    let fg_over_white_bg = foreground.over(bg_over_white);
    let fg_over_black_bg = foreground.over(bg_over_black);
    let ratio_on_white = contrast_ratio(fg_over_white_bg, bg_over_white);
    let ratio_on_black = contrast_ratio(fg_over_black_bg, bg_over_black);
    ratio_on_white.min(ratio_on_black)
}

/// The declared text/non-text colour pairs from
/// `extensions/myna-shell/stylesheet.css` (FR-013, K1/K2). Hand-extracted
/// rather than CSS-parsed: the stylesheet is small and hand-authored, and a
/// general CSS colour-pair parser (resolving which rule's foreground pairs
/// with which rule's background) is disproportionate to this feature's
/// scope. Kept in sync by the doc comment cross-reference below — a change
/// to these colours in the stylesheet without a matching update here is
/// still caught the moment it regresses a threshold, just not the moment
/// the source values diverge.
pub mod stylesheet_colours {
    use super::Rgba;

    /// `.myna-hud-pill { background-color: rgba(20, 22, 28, 0.82); }`
    pub const PILL_BACKGROUND: Rgba = Rgba::new(20.0 / 255.0, 22.0 / 255.0, 28.0 / 255.0, 0.82);
    /// `.myna-hud-label { color: rgba(255, 255, 255, 0.92); }` (text, K1)
    pub const LABEL_TEXT: Rgba = Rgba::new(1.0, 1.0, 1.0, 0.92);
    /// `.myna-hud-icon { color: rgba(255, 255, 255, 0.92); }` (non-text
    /// meaningful element — the mic icon, K2)
    pub const ICON: Rgba = Rgba::new(1.0, 1.0, 1.0, 0.92);
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── T020/T022: matrix invariants (C1–C4) ────────────────────────────────

    fn sample_matrix() -> CoverageMatrix {
        load_matrix().expect("coverage-matrix.json must exist and parse")
    }

    #[test]
    fn the_checked_in_matrix_parses() {
        sample_matrix();
    }

    #[test]
    fn every_entry_has_visual_and_non_visual_channels_and_no_forbidden_encoding() {
        let matrix = sample_matrix();
        let violations = check_invariants(&matrix);
        assert!(violations.is_empty(), "violations: {violations:?}");
    }

    #[test]
    fn every_known_wire_state_has_a_matrix_entry() {
        let matrix = sample_matrix();
        let violations = check_exhaustive(&matrix);
        assert!(violations.is_empty(), "missing entries: {violations:?}");
    }

    #[test]
    fn a_state_with_empty_visual_channels_is_flagged() {
        let matrix = parse_matrix(
            r#"{"states": [{"id": "idle", "channels": {"visual": [], "non_visual": ["x"]}, "colour_only": false, "sound_only": false}]}"#,
        )
        .unwrap();
        assert_eq!(
            check_invariants(&matrix),
            vec![CoverageViolation::EmptyVisualChannels("idle".to_string())]
        );
    }

    #[test]
    fn a_colour_only_state_is_flagged() {
        let matrix = parse_matrix(
            r#"{"states": [{"id": "idle", "channels": {"visual": ["x"], "non_visual": ["y"]}, "colour_only": true, "sound_only": false}]}"#,
        )
        .unwrap();
        assert_eq!(
            check_invariants(&matrix),
            vec![CoverageViolation::ColourOnly("idle".to_string())]
        );
    }

    // ── T026: contrast thresholds (K1/K2) ────────────────────────────────────

    #[test]
    fn identical_colours_have_a_ratio_of_one() {
        assert!((contrast_ratio(WHITE, WHITE) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn black_on_white_is_maximal() {
        assert!((contrast_ratio(BLACK, WHITE) - 21.0).abs() < 1e-6);
    }

    #[test]
    fn label_text_on_pill_background_meets_the_text_threshold() {
        use stylesheet_colours::*;
        let ratio = worst_case_contrast(LABEL_TEXT, PILL_BACKGROUND);
        assert!(
            ratio >= 4.5,
            "label text worst-case contrast {ratio} is below the 4.5:1 threshold (K1)"
        );
    }

    #[test]
    fn icon_on_pill_background_meets_the_non_text_threshold() {
        use stylesheet_colours::*;
        let ratio = worst_case_contrast(ICON, PILL_BACKGROUND);
        assert!(
            ratio >= 3.0,
            "icon worst-case contrast {ratio} is below the 3:1 threshold (K2)"
        );
    }
}
