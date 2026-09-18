//! The state-to-channel coverage matrix (SC-002, FR-032,
//! `contracts/coverage-matrix.md`): loads and validates the checked-in
//! `extensions/myna-shell/coverage-matrix.json`.
//!
//! The contrast-ratio regression check (FR-013) used to live here too,
//! against the Shell extension's `stylesheet.css`. The HUD is now the
//! standalone `myna-hud` renderer and owns its own stylesheet, so the check
//! moved to `myna_hud::contrast`, where `include_str!` keeps it tied to the
//! file it describes.

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
}
