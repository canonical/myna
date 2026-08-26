//! dbus_consumer — the `org.myna.Dictation` consumer (feature 004; contracts
//! `dbus-interface.md` / `extension.md` X7–X10, re-homed to the renderer by
//! the 2026-08-26 architecture revision). Ported from
//! `extensions/myna-shell/dbus.js`.
//!
//! Dormant while the name has no owner (no proxy, no state emissions — X7);
//! activates on name-appeared (connects + reflects the current State — X8)
//! and clears to idle on name-vanished (daemon crash/exit).
//! [`DictationService::disable`] removes the watch, drops the proxy and
//! every subscription (X9); re-enabling re-establishes cleanly (X10).
//!
//! All updates arrive one way: the standard
//! `org.freedesktop.DBus.Properties.PropertiesChanged` signal. The proxy
//! applies it to its property cache before emitting, so we simply re-read
//! the cached `State`/`ErrorMessage`/`AudioRms`/`AudioPeak` and forward what
//! changed. This is the one push channel that works for EVERY publisher,
//! confined or not (contract `dbus-interface.md` §Confinement) — which is
//! why the interface defines no custom signals.
//!
//! **Levels are never deduplicated** (R16a): the renderer uses *arrival
//! time*, not value, to detect a stale stream, so a steady voice — which
//! legitimately repeats the same quantized RMS/peak for consecutive pumps —
//! must keep refreshing that timestamp. The *state descriptor* IS
//! deduplicated, because the publisher pushes the whole property set on
//! every level tick and re-emitting an unchanged state would restart notice
//! timers.
//!
//! The bus plumbing lives behind seams so the lifecycle is contract-testable
//! headless ([`tests/dbus_consumer.rs`]): this module owns the *rules*, and
//! the zbus wiring (T124) drives it.

/// The bus name and object the consumer watches.
pub const BUS_NAME: &str = "org.myna.Dictation";
pub const OBJECT_PATH: &str = "/org/myna/Dictation";

/// A snapshot of the interface's four properties, as read from the proxy's
/// cache (E1/E2/E3).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub state: String,
    pub error_message: String,
    pub audio_rms: f64,
    pub audio_peak: f64,
}

type StateCallback = Box<dyn Fn(&str, &str)>;
type LevelCallback = Box<dyn Fn(f64, f64)>;
type AvailabilityCallback = Box<dyn Fn(bool)>;

/// Builder for [`DictationService`] — the callbacks are all optional, in the
/// GJS original's spirit.
#[derive(Default)]
pub struct DictationServiceBuilder {
    on_state_changed: Option<StateCallback>,
    on_level: Option<LevelCallback>,
    on_availability_changed: Option<AvailabilityCallback>,
}

impl DictationServiceBuilder {
    /// Called with `(state, error_message)` on every transition, on
    /// name-appeared (reflecting the current State), and with `("idle", "")`
    /// on name-vanished.
    pub fn on_state_changed(mut self, f: impl Fn(&str, &str) + 'static) -> Self {
        self.on_state_changed = Some(Box::new(f));
        self
    }

    /// Called for every level arrival — never deduplicated (R16a).
    pub fn on_level(mut self, f: impl Fn(f64, f64) + 'static) -> Self {
        self.on_level = Some(Box::new(f));
        self
    }

    /// Called when the name gains/loses an owner.
    pub fn on_availability_changed(mut self, f: impl Fn(bool) + 'static) -> Self {
        self.on_availability_changed = Some(Box::new(f));
        self
    }

    pub fn build(self) -> DictationService {
        DictationService {
            on_state_changed: self.on_state_changed,
            on_level: self.on_level,
            on_availability_changed: self.on_availability_changed,
            watching: false,
            available: false,
            last_state: None,
        }
    }
}

/// Consumes `org.myna.Dictation` and reports state/level/availability to the
/// application.
pub struct DictationService {
    on_state_changed: Option<StateCallback>,
    on_level: Option<LevelCallback>,
    on_availability_changed: Option<AvailabilityCallback>,
    watching: bool,
    available: bool,
    /// The last `(state, error)` pair emitted, for the dedup rule.
    last_state: Option<(String, String)>,
}

impl DictationService {
    pub fn builder() -> DictationServiceBuilder {
        DictationServiceBuilder::default()
    }

    /// Start watching the name. Dormant until it has an owner (X7).
    pub fn enable(&mut self) {
        self.watching = true;
    }

    /// Remove the watch, drop the proxy and every subscription (X9). Safe
    /// when already dormant; re-[`enable`](Self::enable) re-establishes
    /// cleanly (X10).
    pub fn disable(&mut self) {
        self.watching = false;
        self.available = false;
        self.last_state = None;
    }

    /// Whether the name currently has an owner (E5).
    pub fn is_available(&self) -> bool {
        self.available
    }

    /// Whether the name watch is established.
    pub fn is_watching(&self) -> bool {
        self.watching
    }

    /// The name gained an owner: connect and reflect the current State (X8).
    /// Driven by the bus wiring (or a test).
    pub fn simulate_name_appeared(&mut self, snapshot: Snapshot) {
        if !self.watching {
            return;
        }
        self.available = true;
        if let Some(cb) = &self.on_availability_changed {
            cb(true);
        }
        self.reflect(snapshot);
    }

    /// The name lost its owner: clear to idle rather than freezing in an
    /// active state (X8, the daemon-crash edge case).
    pub fn simulate_name_vanished(&mut self) {
        if !self.watching {
            return;
        }
        self.available = false;
        self.last_state = Some((crate::states::wire::IDLE.to_string(), String::new()));
        if let Some(cb) = &self.on_state_changed {
            cb(crate::states::wire::IDLE, "");
        }
        if let Some(cb) = &self.on_availability_changed {
            cb(false);
        }
    }

    /// A `PropertiesChanged` push: forward the state (deduplicated) and the
    /// levels (never deduplicated).
    pub fn simulate_properties_changed(&mut self, snapshot: Snapshot) {
        if !self.watching || !self.available {
            return;
        }
        self.reflect(snapshot);
    }

    fn reflect(&mut self, snapshot: Snapshot) {
        let pair = (snapshot.state.clone(), snapshot.error_message.clone());
        if self.last_state.as_ref() != Some(&pair) {
            self.last_state = Some(pair);
            if let Some(cb) = &self.on_state_changed {
                cb(&snapshot.state, &snapshot.error_message);
            }
        }
        // Arrival time is part of the stale-decay contract: forward every
        // level update, identical or not (R16a).
        if let Some(cb) = &self.on_level {
            cb(snapshot.audio_rms, snapshot.audio_peak);
        }
    }
}
