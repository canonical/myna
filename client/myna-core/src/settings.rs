//! Client settings: the persisted streaming-mode preference (T047/T048).
//!
//! One setting today: [`StreamingMode`] (Auto | Streaming | Batch), resolved
//! against the tier gate when Auto (FR-002/FR-003).
//!
//! ## Where it lives, and why that is not a file
//!
//! The store is **GSettings**, schema `com.canonical.Myna.Dictation`
//! (`client/data/glib-2.0/schemas/`). Three kinds of writer have to reach the
//! same values: the confined `myna` snap (over the `gsettings` interface),
//! unconfined host tools (`myna-dictate`, a future Settings page), and further
//! snaps as they grow configuration APIs (T54). A JSON file
//! cannot be that store - this used to be `~/.config/myna/settings.json`, and
//! the packaged daemon could never read it: inside the snap `$HOME` is
//! `$SNAP_USER_DATA`, and the `home` interface grants no top-level dotfiles, so
//! the value was written on one side of confinement and read on the other.
//!
//! Nothing here fails hard. A machine without the schema installed (an
//! unpackaged build on a box where `make install-schema` was never run) reads
//! defaults, exactly as a missing file did.
//!
//! GSettings is the *store*, not the interface. The one supported way to read
//! and write these keys is `myna.config` (`myna-desktop --config`), which is
//! what [`KEYS`] and [`Store::set`] are for: the `gsettings` CLI cannot reach
//! a snap-only install, which has no schema on the host, and `dconf write`
//! reaches it only by skipping the validation that makes a typo visible.
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
pub const SCHEMA_ID: &str = "com.canonical.Myna.Dictation";

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

/// Why a settings read or write did not happen.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    /// The backend refused the write (read-only key, locked-down dconf).
    #[error("cannot write {0}: {1}")]
    Write(&'static str, glib::BoolError),

    /// A name that is not in [`KEYS`].
    #[error("unknown key: {0}")]
    UnknownKey(String),

    /// A value outside an enum key's range, caught here rather than in the
    /// schema so the rejection can say what *was* allowed.
    #[error("invalid value {value:?} for {key} (allowed: {})", allowed.join(", "))]
    InvalidValue {
        key: &'static str,
        value: String,
        allowed: &'static [&'static str],
    },
}

/// A key the configuration CLI can get, set and reset: enough for it to
/// validate a write and explain a rejection without knowing the schema.
///
/// This table is the contract, and it exists because the alternative was
/// `dconf write`, which bypasses the schema entirely: a typo landed silently
/// and read back as the default. A setting that is one letter wrong and a
/// setting that was never made must not look the same.
pub struct KeySpec {
    pub name: &'static str,
    /// The accepted nicks of an enum key; `None` for free text.
    pub values: Option<&'static [&'static str]>,
    pub summary: &'static str,
}

/// Every key the configuration CLI exposes, in the order it lists them.
pub const KEYS: &[KeySpec] = &[
    KeySpec {
        name: KEY_STREAMING_MODE,
        values: Some(&["auto", "streaming", "batch"]),
        summary: "how transcripts are emitted, and so whether partials show in-field",
    },
    KeySpec {
        name: KEY_LANGUAGE,
        values: None,
        summary: "language hint for the backend; empty lets it decide",
    },
    KeySpec {
        name: KEY_ACTIVATION,
        values: Some(&["auto", "portal", "control"]),
        summary: "how a press reaches the daemon",
    },
    KeySpec {
        name: KEY_HOTKEY,
        values: None,
        summary: "accelerator offered to the portal's bind dialog",
    },
];

/// Look up a key by name.
pub fn key_spec(name: &str) -> Option<&'static KeySpec> {
    KEYS.iter().find(|k| k.name == name)
}

impl Settings {
    /// Read the store. A missing schema or an unreadable backend yields
    /// defaults (Auto) - a broken settings store must never break dictation.
    pub fn load() -> Self {
        match Store::open() {
            Some(store) => Self::from_store(&store),
            None => Self::default(),
        }
    }

    /// Read every key out of an open store. One reader, so [`load`](Self::load)
    /// at startup and [`watch`] on every change cannot answer differently.
    fn from_store(store: &Store) -> Self {
        Self {
            streaming_mode: store.streaming_mode(),
            language: store.text(KEY_LANGUAGE),
            activation: store.text(KEY_ACTIVATION).filter(|a| a != "auto"),
            hotkey: store.text(KEY_HOTKEY),
        }
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

    /// Wrap the `gio::Settings` a signal handed back. The same GObject, one
    /// reference further on, so it stays on the thread that made it.
    fn from_settings(settings: &gio::Settings) -> Self {
        Self {
            settings: settings.clone(),
        }
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

    /// Read any key as the string the store holds; an enum key reads its nick.
    pub fn get(&self, key: &str) -> Result<String, SettingsError> {
        let spec = key_spec(key).ok_or_else(|| SettingsError::UnknownKey(key.to_string()))?;
        Ok(self.settings.string(spec.name).to_string())
    }

    /// Whether the user holds a value of their own here, as opposed to reading
    /// the schema default.
    ///
    /// This is the distinction that matters when a setting looks like it is
    /// not taking effect: "explicitly set to auto" and "never set" read
    /// identically through [`get`](Self::get), and confusing the two is how a
    /// value written to the wrong place looks like a value that was ignored.
    pub fn is_set(&self, key: &str) -> Result<bool, SettingsError> {
        let spec = key_spec(key).ok_or_else(|| SettingsError::UnknownKey(key.to_string()))?;
        Ok(self.settings.user_value(spec.name).is_some())
    }

    /// Validate against [`KEYS`], then write and flush, so a caller that exits
    /// immediately afterwards still lands the value.
    pub fn set(&self, key: &str, value: &str) -> Result<(), SettingsError> {
        let spec = key_spec(key).ok_or_else(|| SettingsError::UnknownKey(key.to_string()))?;
        match spec.values {
            Some(allowed) if !allowed.contains(&value) => {
                return Err(SettingsError::InvalidValue {
                    key: spec.name,
                    value: value.to_string(),
                    allowed,
                })
            }
            _ => {}
        }
        self.settings
            .set_string(spec.name, value)
            .map_err(|e| SettingsError::Write(spec.name, e))?;
        gio::Settings::sync();
        Ok(())
    }

    /// Put a key back to its schema default. Distinct from writing the default
    /// nick by hand: this clears the user's value, so a later change to what
    /// the default *is* reaches a machine that never chose otherwise.
    pub fn reset(&self, key: &str) -> Result<(), SettingsError> {
        let spec = key_spec(key).ok_or_else(|| SettingsError::UnknownKey(key.to_string()))?;
        self.settings.reset(spec.name);
        gio::Settings::sync();
        Ok(())
    }

    /// Write the preference and flush it, so a caller that exits immediately
    /// afterwards still lands the value.
    pub fn set_streaming_mode(&self, mode: StreamingMode) -> Result<(), SettingsError> {
        self.set(KEY_STREAMING_MODE, mode_nick(mode))
    }
}

/// A live subscription to the store: every change re-reads the whole
/// [`Settings`] value and hands it to the callback.
///
/// Reading the settings once at startup made a *restart* the only way to be
/// heard, for every writer there is - `gsettings`, `myna-dictate`, a Settings
/// page, another snap growing a configuration API (T54). GSettings already
/// broadcasts its changes, so the subscription belongs next to the store
/// rather than in each writer, which would otherwise need the daemon's unit
/// name and the right to restart it.
///
/// Dropping this stops the watch and joins its thread.
pub struct SettingsWatch {
    context: glib::MainContext,
    main_loop: glib::MainLoop,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for SettingsWatch {
    fn drop(&mut self) {
        // Queued into the context rather than called directly: `quit` before
        // `run` is a no-op, and the thread has only reached `run` *after*
        // reporting itself ready - so calling it here could hang the join on
        // a loop that started a moment later.
        let main_loop = self.main_loop.clone();
        self.context.invoke(move || main_loop.quit());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Watch the settings store, calling `on_change` with the new value whenever
/// any key changes. `None` when the schema is not installed - the same
/// condition under which [`Settings::load`] reads defaults, and equally not a
/// failure: there is simply nothing to watch.
///
/// Returning implies the subscription is live, so a change made immediately
/// after this call cannot be missed.
pub fn watch(on_change: impl Fn(Settings) + Send + 'static) -> Option<SettingsWatch> {
    watch_with(Store::open, on_change)
}

/// The watcher proper, over an injectable store so the tests can drive a
/// memory backend instead of the machine's dconf.
///
/// The thread exists because of what the notification needs: GSettings
/// delivers `changed` into the GLib main context that was thread-default when
/// the object was made, and the daemon's main thread is a tokio runtime with
/// no GLib loop on it. [`Store`] is not `Send`, so the thread opens its own
/// rather than being handed one.
fn watch_with(
    open: impl FnOnce() -> Option<Store> + Send + 'static,
    on_change: impl Fn(Settings) + Send + 'static,
) -> Option<SettingsWatch> {
    let context = glib::MainContext::new();
    let main_loop = glib::MainLoop::new(Some(&context), false);
    // Rendezvous, not a queue: `watch` promises a live subscription, so it
    // waits here until the handler is connected (or the store turned out not
    // to exist).
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<bool>(0);

    let thread = std::thread::Builder::new()
        .name("myna-settings".into())
        .spawn({
            let (context, main_loop) = (context.clone(), main_loop.clone());
            move || {
                // The context has to be this thread's default *around* both
                // the `Settings` construction and the loop, which is exactly
                // the scope `with_thread_default` gives. Failing to acquire it
                // drops `ready_tx` unsent, which `watch_with` reads as "no
                // watch" - the same answer as a missing schema.
                let _ = context.with_thread_default(move || {
                    let Some(store) = open() else {
                        let _ = ready_tx.send(false);
                        return;
                    };
                    store.settings.connect_changed(None, move |settings, key| {
                        crate::dbg_log!("settings", "{key} changed");
                        on_change(Settings::from_store(&Store::from_settings(settings)));
                    });
                    let _ = ready_tx.send(true);
                    main_loop.run();
                });
            }
        })
        .ok()?;

    match ready_rx.recv() {
        Ok(true) => Some(SettingsWatch {
            context,
            main_loop,
            thread: Some(thread),
        }),
        // The thread is already returning; nothing to join against.
        _ => None,
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
    use std::time::Duration;

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

    fn test_schema() -> gio::SettingsSchema {
        gio::SettingsSchemaSource::from_directory(
            std::path::Path::new(env!("MYNA_TEST_SCHEMA_DIR")),
            None,
            true,
        )
        .expect("compiled test schema (build.rs)")
        .lookup(SCHEMA_ID, true)
        .expect("the shipped schema declares SCHEMA_ID")
    }

    /// A store of its own: no dconf, no user state, nothing shared between
    /// tests. The production path differs only in where the schema and the
    /// backend come from.
    fn test_store() -> Store {
        Store {
            settings: gio::Settings::new_full(
                &test_schema(),
                Some(&gio::functions::memory_settings_backend_new()),
                None,
            ),
        }
    }

    /// A store over a keyfile at `path`, which is how the watcher test gets
    /// *two* stores over one set of values: a memory backend is private to the
    /// object that made it, and a `SettingsBackend` is a GObject that cannot
    /// cross to the watcher thread anyway. Two independent backends over one
    /// file is also the shape production has - two processes over one dconf
    /// database - rather than one object shared behind a mutex.
    fn store_on(path: &std::path::Path) -> Store {
        let backend = gio::functions::keyfile_settings_backend_new(
            path.to_str().expect("utf-8 temp path"),
            "/com/canonical/myna/dictation/",
            // A group is required: with none, the keyfile backend treats keys
            // sitting directly under the root path as readonly.
            Some("dictation"),
        );
        Store {
            settings: gio::Settings::new_full(&test_schema(), Some(&backend), None),
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

    /// [`KEYS`] is what the CLI validates against, so it has to agree with the
    /// schema in *both* directions: every nick it offers must be accepted, and
    /// it must not be missing one the schema would take. The second half is the
    /// one a static table gets wrong over time - a nick added to the schema and
    /// not here would be rejected by `myna.config` alone, for no visible reason.
    #[test]
    fn key_specs_match_the_schema_range() {
        let schema = test_schema();
        let store = test_store();
        for spec in KEYS {
            let Some(allowed) = spec.values else { continue };
            for value in allowed {
                store
                    .set(spec.name, value)
                    .unwrap_or_else(|e| panic!("schema rejects {}={value}: {e}", spec.name));
                assert_eq!(store.get(spec.name).unwrap(), *value);
            }
            // ("enum", <[nicks]>) for an enum-typed key.
            let range = schema.key(spec.name).range();
            let (kind, values) = range
                .get::<(String, glib::Variant)>()
                .expect("a range is (s, v)");
            assert_eq!(kind, "enum", "{} is declared as an enum", spec.name);
            let mut from_schema: Vec<String> = values
                .iter()
                .map(|v| v.get::<String>().expect("a nick is a string"))
                .collect();
            let mut from_table: Vec<String> = allowed.iter().map(|v| v.to_string()).collect();
            from_schema.sort();
            from_table.sort();
            assert_eq!(
                from_schema, from_table,
                "KEYS and the schema disagree on {}",
                spec.name
            );
        }
    }

    /// Every key the schema declares is reachable from the CLI. A key added to
    /// the schema and not to [`KEYS`] is invisible: unsettable and unlistable,
    /// with nothing anywhere saying it exists.
    #[test]
    fn key_specs_cover_every_key_in_the_schema() {
        let mut from_schema: Vec<String> = test_schema()
            .list_keys()
            .iter()
            .map(|k| k.to_string())
            .collect();
        let mut from_table: Vec<String> = KEYS.iter().map(|k| k.name.to_string()).collect();
        from_schema.sort();
        from_table.sort();
        assert_eq!(from_schema, from_table);
    }

    /// The whole reason the CLI exists rather than `dconf write`: a typo is
    /// refused, and the refusal names the range instead of landing silently.
    #[test]
    fn an_invalid_enum_value_is_refused_with_its_range() {
        let store = test_store();
        store.set(KEY_STREAMING_MODE, "streaming").unwrap();
        let err = store
            .set(KEY_STREAMING_MODE, "strea")
            .expect_err("a typo is not a value");
        let message = err.to_string();
        assert!(message.contains("strea"), "{message}");
        assert!(message.contains("auto, streaming, batch"), "{message}");
        // ...and the store is untouched, so a rejected write cannot be
        // mistaken for one that landed.
        assert_eq!(store.streaming_mode(), StreamingMode::Streaming);
    }

    #[test]
    fn an_unknown_key_is_refused() {
        let store = test_store();
        for result in [
            store.set("streaming", "auto").err(),
            store.get("streaming").err(),
            store.reset("streaming").err(),
        ] {
            assert!(matches!(result, Some(SettingsError::UnknownKey(k)) if k == "streaming"));
        }
    }

    /// Free-text keys have no range, so anything is a value - including the
    /// empty string, which is how a UI clears one.
    #[test]
    fn free_text_keys_take_any_value() {
        let store = test_store();
        store.set(KEY_LANGUAGE, "fr").unwrap();
        assert_eq!(store.text(KEY_LANGUAGE).as_deref(), Some("fr"));
        store.set(KEY_LANGUAGE, "").unwrap();
        assert_eq!(store.text(KEY_LANGUAGE), None);
    }

    /// `reset` clears the user's value rather than writing today's default over
    /// it, so a machine that never chose follows the default if it ever moves.
    #[test]
    fn reset_restores_the_schema_default() {
        let store = test_store();
        store.set(KEY_STREAMING_MODE, "batch").unwrap();
        store.set(KEY_HOTKEY, "<Super>d").unwrap();
        store.reset(KEY_STREAMING_MODE).unwrap();
        store.reset(KEY_HOTKEY).unwrap();
        assert_eq!(store.streaming_mode(), StreamingMode::Auto);
        assert_eq!(store.text(KEY_HOTKEY), None);
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

    /// The point of the watch: a value written by *another* holder of the same
    /// store reaches a running daemon, with no restart and no polling.
    #[test]
    fn a_write_by_another_holder_reaches_the_watcher() {
        let path = std::env::temp_dir().join(format!("myna-watch-{}.ini", std::process::id()));
        std::fs::remove_file(&path).ok();
        let (tx, rx) = std::sync::mpsc::channel();
        let watch = watch_with(
            {
                let path = path.clone();
                move || Some(store_on(&path))
            },
            move |settings| {
                let _ = tx.send(settings);
            },
        )
        .expect("the test schema is always installed");

        store_on(&path)
            .set_streaming_mode(StreamingMode::Batch)
            .unwrap();
        let seen = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the change is delivered");
        assert_eq!(seen.streaming_mode, StreamingMode::Batch);

        // Dropping the handle ends the subscription (and joins the thread,
        // which is what would hang here if `quit` had raced `run`).
        drop(watch);
        store_on(&path)
            .set_streaming_mode(StreamingMode::Streaming)
            .unwrap();
        assert!(
            rx.recv_timeout(Duration::from_millis(250)).is_err(),
            "a dropped watch must stop delivering"
        );
        std::fs::remove_file(&path).ok();
    }

    /// No schema is not a failure, it is "nothing to watch" - and the caller
    /// has to hear that rather than block on a thread that already gave up.
    #[test]
    fn without_a_store_there_is_no_watch() {
        assert!(watch_with(|| None, |_| unreachable!()).is_none());
    }
}
