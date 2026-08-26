//! Client settings: the persisted streaming-mode preference (T047/T048).
//!
//! One setting today: [`StreamingMode`] (Auto | Streaming | Batch), resolved
//! against the tier gate when Auto (FR-002/FR-003).
//!
//! ## Where it lives, and why that is not a file
//!
//! The store is **GSettings**, schema `org.myna.dictation`
//! (`client/data/glib-2.0/schemas/`). Three kinds of writer have to reach the
//! same values: the confined `myna` snap (over the `gsettings` interface),
//! unconfined host tools (`gsettings set`, `myna-dictate`, a future Settings
//! page), and further snaps as they grow configuration APIs (T54). A JSON file
//! cannot be that store - this used to be `~/.config/myna/settings.json`, and
//! the packaged daemon could never read it: inside the snap `$HOME` is
//! `$SNAP_USER_DATA`, and the `home` interface grants no top-level dotfiles, so
//! the value was written on one side of confinement and read on the other.
//!
//! Nothing here fails hard. A machine without the schema installed (an
//! unpackaged build on a box where `make install-schema` was never run) reads
//! defaults, exactly as a missing file did.
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

use gio::glib;
use gio::prelude::SettingsExt;

use crate::StreamingMode;

/// The schema every myna client settings key lives under.
pub const SCHEMA_ID: &str = "org.myna.dictation";

/// The persisted streaming-mode preference.
pub const KEY_STREAMING_MODE: &str = "streaming-mode";

/// The language hint passed to the backend; empty means "backend decides".
pub const KEY_LANGUAGE: &str = "language";

/// How a press reaches the daemon: `auto` | `portal` | `control`.
pub const KEY_ACTIVATION: &str = "activation";

/// The accelerator offered to the portal's bind dialog; empty offers none.
pub const KEY_HOTKEY: &str = "hotkey";

/// The settings, as a plain value: read once, no live binding. Callers that
/// want change notification should hold a [`Store`] instead.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    pub streaming_mode: StreamingMode,
    /// `None` where the key is empty - "unset" and "" are the same intent, and
    /// GSettings has no null.
    pub language: Option<String>,
    /// The nick as stored (`auto` | `portal` | `control`); the desktop app owns
    /// the enum, so this stays a string in the shared crate. `auto` is `None`.
    pub activation: Option<String>,
    pub hotkey: Option<String>,
}

/// Why a settings write did not happen.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    /// No `org.myna.dictation` in any installed schema source. Packaged, the
    /// snap ships its own; unpackaged, `make install-schema` puts it on the
    /// host.
    #[error("GSettings schema {SCHEMA_ID} is not installed")]
    SchemaMissing,
    /// The backend refused the write (read-only key, locked-down dconf).
    #[error("cannot write {0}: {1}")]
    Write(&'static str, glib::BoolError),
}

impl Settings {
    /// Read the store. A missing schema or an unreadable backend yields
    /// defaults (Auto) - a broken settings store must never break dictation.
    pub fn load() -> Self {
        match Store::open() {
            Some(store) => Self {
                streaming_mode: store.streaming_mode(),
                language: store.text(KEY_LANGUAGE),
                activation: store.text(KEY_ACTIVATION).filter(|a| a != "auto"),
                hotkey: store.text(KEY_HOTKEY),
            },
            None => Self::default(),
        }
    }

    /// Persist. Unlike [`Self::load`] this reports failure: a caller asking to
    /// *change* a setting has to hear that it did not take.
    pub fn save(&self) -> Result<(), SettingsError> {
        Store::open()
            .ok_or(SettingsError::SchemaMissing)?
            .set_streaming_mode(self.streaming_mode)
    }
}

/// A live handle on the settings store.
///
/// Deliberately not `Send`: `gio::Settings` is a GObject bound to the thread
/// that made it. Read it where you need it (both binaries do so once, at
/// startup) rather than passing it around.
pub struct Store {
    settings: gio::Settings,
}

impl Store {
    /// Open the store, or `None` when the schema is not installed.
    ///
    /// The lookup is what makes this safe: `gio::Settings::new` *aborts* the
    /// process on an unknown schema id, which is not an acceptable failure
    /// mode for a dictation daemon that starts before the desktop does.
    pub fn open() -> Option<Self> {
        let source = gio::SettingsSchemaSource::default()?;
        source.lookup(SCHEMA_ID, true)?;
        Some(Self {
            settings: gio::Settings::new(SCHEMA_ID),
        })
    }

    /// The persisted preference; an unset key reads the schema default (Auto).
    pub fn streaming_mode(&self) -> StreamingMode {
        mode_from_nick(self.settings.string(KEY_STREAMING_MODE).as_str()).unwrap_or_default()
    }

    /// A string-valued key, with empty read as absent: a user clearing a field
    /// in a settings UI writes `""`, and that has to mean the same as never
    /// having set it.
    pub fn text(&self, key: &str) -> Option<String> {
        let value = self.settings.string(key).to_string();
        (!value.is_empty()).then_some(value)
    }

    /// Write the preference and flush it, so a caller that exits immediately
    /// afterwards still lands the value.
    pub fn set_streaming_mode(&self, mode: StreamingMode) -> Result<(), SettingsError> {
        self.settings
            .set_string(KEY_STREAMING_MODE, mode_nick(mode))
            .map_err(|e| SettingsError::Write(KEY_STREAMING_MODE, e))?;
        gio::Settings::sync();
        Ok(())
    }
}

/// The enum nicks in the schema. Kept next to their parser so the two cannot
/// drift, and deliberately not derived from the serde names: the wire spelling
/// and the settings spelling are separate contracts that happen to agree.
fn mode_nick(mode: StreamingMode) -> &'static str {
    match mode {
        StreamingMode::Auto => "auto",
        StreamingMode::Streaming => "streaming",
        StreamingMode::Batch => "batch",
    }
}

fn mode_from_nick(nick: &str) -> Option<StreamingMode> {
    match nick {
        "auto" => Some(StreamingMode::Auto),
        "streaming" => Some(StreamingMode::Streaming),
        "batch" => Some(StreamingMode::Batch),
        _ => None,
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

    /// A store on the compiled schema from `build.rs`, with a memory backend:
    /// no dconf, no user state, nothing shared between tests. The production
    /// path differs only in where the schema and the backend come from.
    fn test_store() -> Store {
        let source = gio::SettingsSchemaSource::from_directory(
            std::path::Path::new(env!("MYNA_TEST_SCHEMA_DIR")),
            None,
            true,
        )
        .expect("compiled test schema (build.rs)");
        let schema = source
            .lookup(SCHEMA_ID, true)
            .expect("the shipped schema declares SCHEMA_ID");
        Store {
            settings: gio::Settings::new_full(
                &schema,
                Some(&gio::functions::memory_settings_backend_new()),
                None,
            ),
        }
    }

    /// T046: the preference round-trips through the settings store.
    #[test]
    fn settings_persist_across_load() {
        let store = test_store();
        store.set_streaming_mode(StreamingMode::Batch).unwrap();
        assert_eq!(store.streaming_mode(), StreamingMode::Batch);
        store.set_streaming_mode(StreamingMode::Streaming).unwrap();
        assert_eq!(store.streaming_mode(), StreamingMode::Streaming);
    }

    /// An untouched store reads the schema's own default, so a fresh machine
    /// gets Auto without anything having to write it first.
    #[test]
    fn an_unset_key_reads_the_schema_default() {
        assert_eq!(test_store().streaming_mode(), StreamingMode::Auto);
        assert_eq!(Settings::default().streaming_mode, StreamingMode::Auto);
    }

    /// The schema's nicks and this module's parser are one contract; a value
    /// the schema would reject must not be one we can produce.
    #[test]
    fn every_mode_nick_round_trips_through_the_schema() {
        let store = test_store();
        for mode in [
            StreamingMode::Auto,
            StreamingMode::Streaming,
            StreamingMode::Batch,
        ] {
            store.set_streaming_mode(mode).expect("schema accepts nick");
            assert_eq!(store.streaming_mode(), mode);
            assert_eq!(mode_from_nick(mode_nick(mode)), Some(mode));
        }
    }

    /// Where the old JSON store fell back on a malformed file, this falls back
    /// on a value the schema allows but this build does not know.
    #[test]
    fn an_unknown_nick_falls_back_to_the_default() {
        assert_eq!(mode_from_nick("supersonic"), None);
        assert_eq!(
            mode_from_nick("supersonic").unwrap_or_default(),
            StreamingMode::Auto
        );
    }

    /// `Store::open` must answer `None` rather than aborting when the schema is
    /// absent - `gio::Settings::new` on an unknown id kills the process, and a
    /// daemon that starts before the desktop cannot afford that.
    #[test]
    fn a_missing_schema_is_none_not_an_abort() {
        let empty = std::env::temp_dir().join(format!("myna-empty-schemas-{}", std::process::id()));
        std::fs::create_dir_all(&empty).unwrap();
        // An empty directory has no compiled schemas at all, so it cannot be a
        // source; either way the answer must be "no schema", never a crash.
        let source = gio::SettingsSchemaSource::from_directory(&empty, None, true);
        assert!(source.is_err() || source.unwrap().lookup(SCHEMA_ID, true).is_none());
        std::fs::remove_dir_all(&empty).ok();
    }
}
