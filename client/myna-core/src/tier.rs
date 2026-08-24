//! Per-model, per-hardware RTF tier assessment for the streaming gate (T039/T040).
//!
//! A [`TierAssessment`] is a measured RTF for one model on one machine,
//! recorded in `results/streaming-tiers.json` by the lab (dev/matrix.py) and
//! shipped as a static data file. The gate (FR-002): streaming is viable only
//! when the recorded RTF for the active model is below the threshold (~1.0);
//! no measurement → batch (safe default, FR-010).

use serde::{Deserialize, Serialize};

/// Default RTF threshold: the model must process audio faster than it arrives.
pub const DEFAULT_RTF_THRESHOLD: f64 = 1.0;

/// One measured model×hardware data point (data-model.md, feature 007).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TierAssessment {
    pub model: String,
    pub hardware: String,
    pub rtf: f64,
    pub strategy: String,
    pub measured_at: String,
}

/// The tier table: assessments loaded from the shipped baseline file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TierTable {
    #[serde(default)]
    pub assessments: Vec<TierAssessment>,
}

impl TierTable {
    /// Parse from the JSON baseline file shape.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Serialize to the JSON baseline file shape.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("TierTable is always serializable")
    }

    /// The recorded RTF for `model` on `hardware`, if measured.
    pub fn rtf_for(&self, model: &str, hardware: &str) -> Option<f64> {
        self.assessments
            .iter()
            .find(|a| a.model == model && a.hardware == hardware)
            .map(|a| a.rtf)
    }
}

/// FR-002: the streaming gate. `Some(rtf)` below the threshold → streaming;
/// `Some(rtf)` at/above → batch; `None` (unmeasured) → batch (safe default).
pub fn streaming_viable(table: &TierTable, model: &str, hardware: &str, threshold: f64) -> bool {
    match table.rtf_for(model, hardware) {
        Some(rtf) => rtf < threshold,
        None => false,
    }
}

/// The same gate with the model axis left open: which model the server serves
/// is not knowable before a session opens, so take the most permissive outcome
/// over every model measured on this hardware. Safe because the server gates
/// itself as well - a batch-only backend simply never emits `Unstable`.
pub fn streaming_viable_here(table: &TierTable, hardware: &str, threshold: f64) -> bool {
    table
        .assessments
        .iter()
        .any(|a| a.hardware == hardware && a.rtf < threshold)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> TierTable {
        TierTable {
            assessments: vec![
                TierAssessment {
                    model: "whisper-small".into(),
                    hardware: "gpu-rtx".into(),
                    rtf: 0.3,
                    strategy: "streaming".into(),
                    measured_at: "2026-07-27T00:00:00Z".into(),
                },
                TierAssessment {
                    model: "whisper-small".into(),
                    hardware: "cpu-i5".into(),
                    rtf: 1.4,
                    strategy: "batch".into(),
                    measured_at: "2026-07-27T00:00:00Z".into(),
                },
            ],
        }
    }

    /// T037: the threshold check — below → streaming, at/above → batch.
    #[test]
    fn rtf_below_threshold_allows_streaming() {
        assert!(streaming_viable(
            &table(),
            "whisper-small",
            "gpu-rtx",
            DEFAULT_RTF_THRESHOLD
        ));
    }

    #[test]
    fn rtf_above_threshold_forces_batch() {
        assert!(!streaming_viable(
            &table(),
            "whisper-small",
            "cpu-i5",
            DEFAULT_RTF_THRESHOLD
        ));
    }

    /// T044: an unmeasured model×hardware pair defaults to batch (safe).
    #[test]
    fn unmeasured_tier_defaults_to_batch() {
        assert!(!streaming_viable(
            &table(),
            "nemotron",
            "gpu-rtx",
            DEFAULT_RTF_THRESHOLD
        ));
        assert!(!streaming_viable(
            &table(),
            "whisper-small",
            "unknown-hw",
            DEFAULT_RTF_THRESHOLD
        ));
        assert!(!streaming_viable(
            &TierTable::default(),
            "whisper-small",
            "gpu-rtx",
            1.0
        ));
    }

    #[test]
    fn rtf_exactly_at_threshold_forces_batch() {
        // RTF == 1.0 means inference keeps pace exactly — no headroom for the
        // committed frontier to stay ahead. Batch.
        let t = TierTable {
            assessments: vec![TierAssessment {
                model: "m".into(),
                hardware: "h".into(),
                rtf: 1.0,
                strategy: "batch".into(),
                measured_at: "2026-07-27T00:00:00Z".into(),
            }],
        };
        assert!(!streaming_viable(&t, "m", "h", DEFAULT_RTF_THRESHOLD));
    }

    #[test]
    fn any_measured_model_on_this_hardware_opens_the_gate() {
        // whisper-small is batch on cpu-i5 but streaming on gpu-rtx; with the
        // model unknown, gpu-rtx is viable and cpu-i5 is not.
        assert!(streaming_viable_here(
            &table(),
            "gpu-rtx",
            DEFAULT_RTF_THRESHOLD
        ));
        assert!(!streaming_viable_here(
            &table(),
            "cpu-i5",
            DEFAULT_RTF_THRESHOLD
        ));
    }

    #[test]
    fn unmeasured_hardware_stays_batch() {
        assert!(!streaming_viable_here(
            &table(),
            "unmeasured",
            DEFAULT_RTF_THRESHOLD
        ));
    }

    #[test]
    fn json_round_trip() {
        let t = table();
        let restored = TierTable::from_json(&t.to_json()).unwrap();
        assert_eq!(restored, t);
    }
}
