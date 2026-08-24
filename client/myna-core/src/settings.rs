//! Client settings: the persisted streaming-mode preference (T047/T048).
//!
//! One setting today: [`StreamingMode`] (Auto | Streaming | Batch), resolved
//! against the tier gate when Auto (FR-002/FR-003). Persisted as JSON at
//! `$XDG_CONFIG_HOME/myna/settings.json` (default `~/.config/myna/`). The
//! desktop app can re-bind the same enum onto dconf/snap config (T54) without
//! touching the enum or resolution logic.
//!
//! Two layers, deliberately separated:
//!
//! - [`resolve_mode`] is pure - table, model, hardware in, mode out - and is
//!   where the gate semantics are pinned by unit tests.
//! - [`effective_mode`] is the host-side wrapper every *binary* should call:
//!   it finds the shipped baseline ([`tier_table`]), fingerprints the machine
//!   ([`hardware_tier`]), and resolves without needing a server connection.
//!   One implementation, so the CLI and the desktop daemon cannot drift.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::StreamingMode;

/// The persisted settings document.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub streaming_mode: StreamingMode,
}

impl Settings {
    /// The settings file path: `$XDG_CONFIG_HOME/myna/settings.json`, falling
    /// back to `~/.config/myna/settings.json`.
    pub fn path() -> PathBuf {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("myna").join("settings.json")
    }

    /// Load from disk; a missing or malformed file yields defaults (Auto) —
    /// a broken settings file must never break dictation.
    pub fn load() -> Self {
        let Ok(text) = std::fs::read_to_string(Self::path()) else {
            return Self::default();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    /// Persist to disk, creating the config directory. Best-effort: dictation
    /// must work even when the config dir is unwritable.
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).expect("Settings is serializable");
        std::fs::write(path, text)
    }
}

/// Resolve the user's mode preference against the tier gate (FR-002/FR-003):
/// - `Streaming` → always streaming (user accepted potential latency)
/// - `Batch` → always batch
/// - `Auto` → streaming iff [`crate::streaming_viable`] says the model×hardware
///   tier sustains it; otherwise batch (and unmeasured tiers → batch, T044)
pub fn resolve_mode(
    preference: StreamingMode,
    table: &crate::TierTable,
    model: &str,
    hardware: &str,
) -> StreamingMode {
    match preference {
        StreamingMode::Streaming | StreamingMode::Batch => preference,
        StreamingMode::Auto => {
            if crate::streaming_viable(table, model, hardware, crate::DEFAULT_RTF_THRESHOLD) {
                StreamingMode::Streaming
            } else {
                StreamingMode::Batch
            }
        }
    }
}

/// Coarse hardware fingerprint used as the tier table's `hardware` key.
///
/// Deliberately coarse: the lab pins a machine with `MYNA_HARDWARE_TIER` when
/// recording a baseline, and anything unrecognised falls through to the batch
/// default rather than guessing.
pub fn hardware_tier() -> String {
    std::env::var("MYNA_HARDWARE_TIER")
        .unwrap_or_else(|_| format!("{}-cpu-generic", std::env::consts::ARCH))
}

/// The shipped RTF baseline, searched in this order:
///
/// 1. `$MYNA_TIER_TABLE` - explicit override for the lab and for tests
/// 2. `$SNAP/usr/share/myna/streaming-tiers.json` - the packaged copy
/// 3. `/usr/share/myna/streaming-tiers.json` - a system install
///
/// Missing or unparseable yields an empty table, which gates `Auto` to batch
/// (FR-010). A baseline is measured data, never inferred: an absent file must
/// read as "unmeasured", not as "assume it streams".
pub fn tier_table() -> crate::TierTable {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(explicit) = std::env::var_os("MYNA_TIER_TABLE") {
        candidates.push(PathBuf::from(explicit));
    }
    if let Some(snap) = std::env::var_os("SNAP") {
        candidates.push(PathBuf::from(snap).join("usr/share/myna/streaming-tiers.json"));
    }
    candidates.push(PathBuf::from("/usr/share/myna/streaming-tiers.json"));

    candidates
        .iter()
        .find_map(|path| {
            let text = std::fs::read_to_string(path).ok()?;
            crate::TierTable::from_json(&text).ok()
        })
        .unwrap_or_default()
}

/// The mode this machine will actually use, with no server connection needed.
///
/// `Streaming`/`Batch` are the user's explicit choice and pass straight
/// through; `Auto` goes through the tier gate. The model axis stays open
/// (see [`crate::streaming_viable_here`]) because the active model is
/// server-side and not knowable before a session opens.
pub fn effective_mode(preference: StreamingMode) -> StreamingMode {
    match preference {
        StreamingMode::Streaming | StreamingMode::Batch => preference,
        StreamingMode::Auto => {
            if crate::streaming_viable_here(
                &tier_table(),
                &hardware_tier(),
                crate::DEFAULT_RTF_THRESHOLD,
            ) {
                StreamingMode::Streaming
            } else {
                StreamingMode::Batch
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TierAssessment, TierTable};

    fn table() -> TierTable {
        TierTable {
            assessments: vec![TierAssessment {
                model: "whisper-tiny".into(),
                hardware: "x86_64-cpu-generic".into(),
                rtf: 1.08,
                strategy: "batch".into(),
                measured_at: "2026-07-27T00:00:00Z".into(),
            }],
        }
    }

    /// T045: the user override beats the tier gate, in both directions.
    #[test]
    fn forced_streaming_overrides_a_failing_gate() {
        // RTF 1.08 would gate to batch under Auto, but the user forced it.
        assert_eq!(
            resolve_mode(
                StreamingMode::Streaming,
                &table(),
                "whisper-tiny",
                "x86_64-cpu-generic"
            ),
            StreamingMode::Streaming
        );
    }

    #[test]
    fn forced_batch_overrides_a_passing_gate() {
        let t = TierTable {
            assessments: vec![TierAssessment {
                model: "nemotron".into(),
                hardware: "gpu".into(),
                rtf: 0.2,
                strategy: "streaming".into(),
                measured_at: "2026-07-27T00:00:00Z".into(),
            }],
        };
        assert_eq!(
            resolve_mode(StreamingMode::Batch, &t, "nemotron", "gpu"),
            StreamingMode::Batch
        );
    }

    #[test]
    fn auto_resolves_through_the_gate() {
        let t = table();
        assert_eq!(
            resolve_mode(
                StreamingMode::Auto,
                &t,
                "whisper-tiny",
                "x86_64-cpu-generic"
            ),
            StreamingMode::Batch
        );
        assert_eq!(
            resolve_mode(StreamingMode::Auto, &t, "whisper-tiny", "unmeasured-hw"),
            StreamingMode::Batch
        );
    }

    /// T046: settings round-trip through the JSON file.
    #[test]
    fn settings_persist_across_load() {
        let dir = std::env::temp_dir().join(format!("myna-settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let settings = Settings {
            streaming_mode: StreamingMode::Batch,
        };
        std::fs::write(&path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();
        let restored: Settings =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(restored, settings);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn malformed_settings_fall_back_to_default() {
        let restored: Settings = serde_json::from_str("{not json").unwrap_or_default();
        assert_eq!(restored, Settings::default());
    }
}
