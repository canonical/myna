//! Client settings: the persisted dictation preferences under one GSettings
//! schema.
//!
//! ## Where it lives
//!
//! The API is **GSettings**, schema `com.canonical.Myna.Dictation`
//! (`client/data/glib-2.0/schemas/`); the *backend* is chosen by the
//! environment: `GSETTINGS_BACKEND=keyfile` plus `XDG_CONFIG_HOME`. In the
//! snap both are set per app (`myna-snap/snap/snapcraft.yaml`), so the store
//! is the snap-private file
//! `$SNAP_USER_COMMON/.config/glib-2.0/settings/keyfile` - plaintext,
//! editable, captured by `snap save`, removed with the snap; `myna.config`
//! (glib's own `gsettings` behind a wrapper) is the packaged way in.
//! Unpackaged, the dev launch paths export the same pair, so the store is
//! `~/.config/glib-2.0/settings/keyfile` - one store shape everywhere. Live
//! reload is the keyfile backend's own file monitor, which is what [`watch`]
//! rides on; the tests below drive that same backend directly.
//!
//! Nothing here fails hard. A machine without the schema installed (an
//! unpackaged build on a box where `make install-schema` was never run) reads
//! defaults, exactly as a missing file did.

use std::path::PathBuf;

use gio::glib;
use gio::prelude::SettingsExt;

use crate::StreamingMode;

/// The schema every myna client settings key lives under.
pub const SCHEMA_ID: &str = "com.canonical.Myna.Dictation";

/// The persisted streaming-mode preference.
pub const KEY_STREAMING_MODE: &str = "streaming-mode";

/// The spoken language passed to the backend; empty means "backend decides".
pub const KEY_LANGUAGE: &str = "language";

/// The HUD indicator style: `bar` (accent level bar), `ribbon` (GPU wave) or
/// `vumeter` (segmented bar).
pub const KEY_HUD_STYLE: &str = "hud-style";

/// The `hud-style` the schema defaults to. Duplicated from the schema so the
/// value survives a machine with no schema installed, which is exactly when
/// [`Settings::hud_style`] reads `None`.
pub const DEFAULT_HUD_STYLE: &str = "bar";

/// How long a toggle session may go without voice before the daemon ends it
/// on its own, in seconds; `0` turns the timeout off.
pub const KEY_SILENCE_TIMEOUT: &str = "silence-timeout";

/// Whether the daemon plays a theme sound when a session starts, stops or
/// fails.
pub const KEY_SOUNDS: &str = "sounds";

/// The schema default for [`KEY_SOUNDS`], and what a machine with no schema
/// installed gets.
pub const DEFAULT_SOUNDS: bool = true;

/// The schema default for [`KEY_SILENCE_TIMEOUT`], also what a machine with
/// no schema installed gets - a forgotten session should end there too.
pub const DEFAULT_SILENCE_TIMEOUT_SECS: u32 = 30;

/// The settings, as a plain value: read once, no live binding. Callers that
/// want change notification should hold a [`Store`] instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// The user's explicit choice, `None` when the key holds no user value.
    /// Unset is not "streaming": [`crate::effective_mode`] turns it into the
    /// backend's default.
    pub streaming_mode: Option<StreamingMode>,
    /// `None` where the key is empty - "unset" and "" are the same intent, and
    /// GSettings has no null.
    pub language: Option<String>,
    /// The HUD indicator style nick (`bar` | `ribbon` | `vumeter`), or `None` when
    /// unset (the schema default applies).
    pub hud_style: Option<String>,
    /// Seconds of silence after which a toggle session ends itself; `0` = never.
    pub silence_timeout: u32,
    /// Whether a session's start, stop and failure are heard as well as seen.
    pub sounds: bool,
}

/// What a machine with no schema installed reads: every key's schema default.
impl Default for Settings {
    fn default() -> Self {
        Self {
            streaming_mode: None,
            language: None,
            hud_style: None,
            silence_timeout: DEFAULT_SILENCE_TIMEOUT_SECS,
            sounds: DEFAULT_SOUNDS,
        }
    }
}

impl Settings {
    /// Read the store. A missing schema or an unreadable backend yields
    /// defaults - a broken settings store must never break
    /// dictation.
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
            hud_style: store.text(KEY_HUD_STYLE),
            silence_timeout: store.seconds(KEY_SILENCE_TIMEOUT),
            sounds: store.flag(KEY_SOUNDS),
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
        let schema = source.lookup(SCHEMA_ID, true)?;
        let backend =
            gio::functions::keyfile_settings_backend_new(store_path()?.to_str()?, "/", None);
        Some(Self {
            settings: gio::Settings::new_full(&schema, Some(&backend), None),
        })
    }

    /// Wrap the `gio::Settings` a signal handed back. The same GObject, one
    /// reference further on, so it stays on the thread that made it.
    fn from_settings(settings: &gio::Settings) -> Self {
        Self {
            settings: settings.clone(),
        }
    }

    /// The user's choice, or `None` when the key holds no user value. A
    /// written value equal to the schema default is still a choice, and a
    /// value outside the schema enum (the retired `auto`) is none.
    pub fn streaming_mode(&self) -> Option<StreamingMode> {
        self.settings
            .user_value(KEY_STREAMING_MODE)?
            .str()
            .and_then(mode_from_nick)
    }

    /// A string-valued key, with empty read as absent: a user clearing a field
    /// in a settings UI writes `""`, and that has to mean the same as never
    /// having set it.
    pub fn text(&self, key: &str) -> Option<String> {
        let value = self.settings.string(key).to_string();
        (!value.is_empty()).then_some(value)
    }

    /// An unsigned-seconds key. The schema bounds it; nothing to interpret.
    pub fn seconds(&self, key: &str) -> u32 {
        self.settings.uint(key)
    }

    pub fn flag(&self, key: &str) -> bool {
        self.settings.boolean(key)
    }
}

/// The one settings file, for every process that reads or writes it.
///
/// The snap sets `GSETTINGS_BACKEND=keyfile` + `XDG_CONFIG_HOME`
/// (`myna-snap/snap/snapcraft.yaml`) and `dev/gated-tests.sh` exports the same
/// pair into a scratch home, so that env pair stays the override. With neither
/// set - an unpackaged daemon, the host Settings app, an unpackaged HUD - the
/// answer is the snap-private file anyway, because that is the store the snap
/// is using and a host tool that wrote somewhere else would be editing a file
/// nothing reads. There is no dconf path: two stores is how `hud-style` came
/// to be writable in the Settings app and invisible to the HUD.
///
/// Always an explicit backend, never `g_settings_backend_get_default`: that is
/// a *process singleton* created by the first `GSettings` object on whatever
/// thread and main context that thread happens to have, and the keyfile
/// backend's live-reload monitor dispatches on the context captured at that
/// moment. A process whose first read happens on a context-free main thread
/// (the daemon's tokio main) would get a monitor that is never dispatched:
/// reads work, and no change is ever delivered (observed live with the snap's
/// glib 2.80). One backend per [`Store`] puts each store's monitor on the
/// thread that owns the store - for [`watch`], the watcher thread with its
/// running loop.
pub fn store_path() -> Option<PathBuf> {
    let config_dir = if std::env::var_os("GSETTINGS_BACKEND").as_deref()
        == Some(std::ffi::OsStr::new("keyfile"))
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?
    } else {
        std::env::var_os("SNAP_USER_COMMON")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join("snap/myna/common"))
            })?
            .join(".config")
    };
    Some(config_dir.join("glib-2.0/settings/keyfile"))
}

/// A live subscription to the store: every change re-reads the whole
/// [`Settings`] value and hands it to the callback.
///
/// Reading the settings once at startup made a *restart* the only way to be
/// heard, for every writer there is - `gsettings`, `myna-testbed`, a Settings
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

fn mode_from_nick(nick: &str) -> Option<StreamingMode> {
    match nick {
        "streaming" => Some(StreamingMode::Streaming),
        "batch" => Some(StreamingMode::Batch),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// The enum nicks in the schema. Kept next to their parser so the two
    /// cannot drift, and deliberately not derived from the serde names: the
    /// wire spelling and the settings spelling are separate contracts that
    /// happen to agree.
    fn mode_nick(mode: StreamingMode) -> &'static str {
        match mode {
            StreamingMode::Streaming => "streaming",
            StreamingMode::Batch => "batch",
        }
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
    /// file is also the shape production has - two processes over one
    /// keyfile - rather than one object shared behind a mutex.
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

    /// Write a mode the way any external writer would: straight through
    /// `gio::Settings`, with a sync so a change made just before an assert
    /// (or just before the process exits) is on disk.
    fn set_mode(store: &Store, mode: StreamingMode) {
        store
            .settings
            .set_string(KEY_STREAMING_MODE, mode_nick(mode))
            .expect("schema accepts nick");
        gio::Settings::sync();
    }

    /// T046: the preference round-trips through the settings store.
    #[test]
    fn settings_persist_across_load() {
        let store = test_store();
        set_mode(&store, StreamingMode::Batch);
        assert_eq!(store.streaming_mode(), Some(StreamingMode::Batch));
        set_mode(&store, StreamingMode::Streaming);
        assert_eq!(store.streaming_mode(), Some(StreamingMode::Streaming));
    }

    /// An untouched store holds no choice, so the backend's default applies;
    /// the no-schema fallback agrees.
    #[test]
    fn an_unset_key_is_no_choice() {
        assert_eq!(test_store().streaming_mode(), None);
        assert_eq!(Settings::default().streaming_mode, None);
    }

    /// Choosing the value that happens to be the schema default is still a
    /// choice, and only a reset gives it up.
    #[test]
    fn the_default_value_written_is_a_choice_until_reset() {
        let store = test_store();
        set_mode(&store, StreamingMode::Streaming);
        assert_eq!(store.streaming_mode(), Some(StreamingMode::Streaming));
        store.settings.reset(KEY_STREAMING_MODE);
        assert_eq!(store.streaming_mode(), None);
    }

    /// The resolver's fallback for an unknown backend is the schema default,
    /// so the two must not drift.
    #[test]
    fn the_unknown_backend_fallback_is_the_schema_default() {
        let default = test_store()
            .settings
            .default_value(KEY_STREAMING_MODE)
            .expect("the key has a default");
        assert_eq!(
            default.str().and_then(mode_from_nick),
            Some(crate::effective_mode(None, None).mode)
        );
    }

    /// The silence timeout's schema default and the no-schema fallback are
    /// one number, so a machine without the schema still ends a forgotten
    /// session; a written value reads back through `from_store`.
    #[test]
    fn silence_timeout_reads_the_schema_default_and_round_trips() {
        let store = test_store();
        assert_eq!(
            store.seconds(KEY_SILENCE_TIMEOUT),
            DEFAULT_SILENCE_TIMEOUT_SECS
        );
        assert_eq!(
            Settings::from_store(&store).silence_timeout,
            Settings::default().silence_timeout
        );
        assert!(store.settings.set_uint(KEY_SILENCE_TIMEOUT, 0).is_ok());
        assert_eq!(Settings::from_store(&store).silence_timeout, 0);
        assert!(store.settings.set_uint(KEY_SILENCE_TIMEOUT, 120).is_ok());
        assert_eq!(Settings::from_store(&store).silence_timeout, 120);
    }

    /// Sounds are on out of the box, with or without a schema, and turning
    /// them off reaches the value the daemon reads.
    #[test]
    fn sounds_read_the_schema_default_and_round_trip() {
        let store = test_store();
        assert_eq!(Settings::from_store(&store).sounds, DEFAULT_SOUNDS);
        assert_eq!(Settings::default().sounds, DEFAULT_SOUNDS);
        assert!(store.settings.set_boolean(KEY_SOUNDS, false).is_ok());
        assert!(!Settings::from_store(&store).sounds);
        assert!(store.settings.set_boolean(KEY_SOUNDS, true).is_ok());
        assert!(Settings::from_store(&store).sounds);
    }

    /// The schema's nicks and this module's parser are one contract; a value
    /// the schema would reject must not be one we can produce.
    #[test]
    fn every_mode_nick_round_trips_through_the_schema() {
        let store = test_store();
        for mode in [StreamingMode::Streaming, StreamingMode::Batch] {
            set_mode(&store, mode);
            assert_eq!(store.streaming_mode(), Some(mode));
            assert_eq!(mode_from_nick(mode_nick(mode)), Some(mode));
        }
    }

    /// A nick this build does not know is no mode at all, never a guess.
    #[test]
    fn an_unknown_nick_is_no_mode() {
        assert_eq!(mode_from_nick("supersonic"), None);
    }

    /// A style retired from the schema can still sit in a user's keyfile, and
    /// a hand edit can put anything there; either must read as the default
    /// rather than reach the HUD as a nick the schema no longer has.
    #[test]
    fn a_hud_style_outside_the_schema_reads_the_default() {
        let path = std::env::temp_dir().join(format!("myna-hud-style-{}.ini", std::process::id()));
        for retired in ["progress", "hologram"] {
            std::fs::write(&path, format!("[dictation]\nhud-style='{retired}'\n")).unwrap();
            assert_eq!(
                Settings::from_store(&store_on(&path)).hud_style.as_deref(),
                Some(DEFAULT_HUD_STYLE),
                "{retired}"
            );
        }
        std::fs::remove_file(&path).ok();
    }

    /// `auto` left the schema with the tier gate, but a keyfile written before
    /// that still holds it; it must read as no choice like any other value
    /// the schema does not know.
    #[test]
    fn a_streaming_mode_outside_the_schema_is_no_choice() {
        let path =
            std::env::temp_dir().join(format!("myna-streaming-mode-{}.ini", std::process::id()));
        for retired in ["auto", "supersonic"] {
            std::fs::write(&path, format!("[dictation]\nstreaming-mode='{retired}'\n")).unwrap();
            assert_eq!(
                Settings::from_store(&store_on(&path)).streaming_mode,
                None,
                "{retired}"
            );
        }
        std::fs::remove_file(&path).ok();
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

        set_mode(&store_on(&path), StreamingMode::Batch);
        let seen = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the change is delivered");
        assert_eq!(seen.streaming_mode, Some(StreamingMode::Batch));

        // Dropping the handle ends the subscription (and joins the thread,
        // which is what would hang here if `quit` had raced `run`).
        drop(watch);
        set_mode(&store_on(&path), StreamingMode::Streaming);
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
