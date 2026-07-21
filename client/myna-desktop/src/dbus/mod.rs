//! The `org.myna.Dictation` D-Bus publisher boundary (feature
//! 004-gnome-shell-indicator).
//!
//! `myna-desktop` publishes dictation **state + level only** (never transcript
//! text — constitution V) on the session bus for the GNOME Shell extension
//! (`extensions/myna-shell/`), per `specs/004-gnome-shell-indicator/contracts/
//! dbus-interface.md`. All publisher logic is written against the small [`Bus`]
//! seam so the mapping/throttling is hermetic-testable over [`FakeBus`]
//! (research R11); the real `zbus`-backed implementation that owns the
//! well-known name and serves `/org/myna/Dictation` lands in the polish phase
//! (contract publisher.md P13–P15).

/// Well-known bus name owned by `myna-desktop --dbus` (contract §Bus topology).
pub const BUS_NAME: &str = "org.myna.Dictation";
/// Object path the interface is served at.
pub const OBJECT_PATH: &str = "/org/myna/Dictation";
/// Interface name (identical to the bus name for the MVP).
pub const INTERFACE: &str = "org.myna.Dictation";

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

/// A property value crossing the bus seam. The contract only uses `s` and `d`
/// shapes (`State`/`ErrorMessage` strings, `AudioRms`/`AudioPeak` doubles), so
/// those are the only variants — no transcript-bearing shape exists here (C3).
#[derive(Debug, Clone, PartialEq)]
pub enum PropertyValue {
    /// A D-Bus string (`s`) property.
    Str(String),
    /// A D-Bus double (`d`) property.
    F64(f64),
}

/// The bus boundary all publisher logic is written against (research R11):
/// emit a `StateChanged` signal; set a property. Small enough that the real
/// `zbus` implementation (polish phase) and the hermetic [`FakeBus`] are both
/// trivial, keeping the state/level mapping testable without a session bus.
#[async_trait]
pub trait Bus: Send {
    /// Emit `StateChanged(state, error_message)` — exactly once per state
    /// transition (C2); `error_message` is empty unless `state == "error"` and
    /// is always a content-free reason (C3).
    async fn emit_state_changed(&mut self, state: &str, error_message: &str);

    /// Set a property (`State` / `AudioRms` / `AudioPeak` / `ErrorMessage`),
    /// emitting `PropertiesChanged` on the real bus.
    async fn set_property(&mut self, name: &str, value: PropertyValue);
}

/// Shared handle to the bus for the publisher halves (indicator, level pump,
/// trigger). Cloning shares the same underlying bus.
pub type SharedBus = Arc<tokio::sync::Mutex<dyn Bus>>;

/// The served `org.myna.Dictation` object: owns the [`Bus`] handle the
/// `DbusIndicator` (state), the level pump (levels), and the `DbusTrigger`
/// (methods) all publish through, plus the bus-name lifecycle (request on
/// start, release on shutdown — C1/C9). The zbus serve lands with the gated
/// round-trip suite (P13–P15).
pub struct DictationService {
    bus: SharedBus,
}

impl DictationService {
    /// Wrap a [`Bus`] implementation as the served object.
    pub fn new<B: Bus + 'static>(bus: B) -> Self {
        Self {
            bus: Arc::new(tokio::sync::Mutex::new(bus)),
        }
    }

    /// A shared handle for publisher components.
    pub fn bus(&self) -> SharedBus {
        Arc::clone(&self.bus)
    }
}

/// Hermetic in-memory [`Bus`] (research R11): records every emitted
/// `StateChanged` (state + error args, in order) and keeps the latest snapshot
/// of each property. A permanent fixture (like `indicator::mock::MockIndicator`)
/// — clone it *before* handing it to the publisher; all clones share the
/// recording.
#[derive(Clone, Default)]
pub struct FakeBus {
    inner: Arc<Mutex<FakeBusInner>>,
}

#[derive(Default)]
struct FakeBusInner {
    signals: Vec<(String, String)>,
    properties: HashMap<String, PropertyValue>,
}

impl FakeBus {
    /// A fresh fake with nothing recorded.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every `StateChanged` emitted, as `(state, error_message)` in order.
    pub fn signals(&self) -> Vec<(String, String)> {
        self.inner.lock().expect("fake bus poisoned").signals.clone()
    }

    /// The latest value set for `name`, if any.
    pub fn property(&self, name: &str) -> Option<PropertyValue> {
        self.inner
            .lock()
            .expect("fake bus poisoned")
            .properties
            .get(name)
            .cloned()
    }
}

#[async_trait]
impl Bus for FakeBus {
    async fn emit_state_changed(&mut self, state: &str, error_message: &str) {
        self.inner
            .lock()
            .expect("fake bus poisoned")
            .signals
            .push((state.to_string(), error_message.to_string()));
    }

    async fn set_property(&mut self, name: &str, value: PropertyValue) {
        self.inner
            .lock()
            .expect("fake bus poisoned")
            .properties
            .insert(name.to_string(), value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Contract publisher.md P-seam / research R11: the hermetic fake bus
    /// records emitted `StateChanged` signals (name + args) and the latest
    /// property snapshot, so publisher logic is testable without a session bus.
    #[tokio::test]
    async fn fake_bus_records_signals_and_latest_properties() {
        let mut bus = FakeBus::new();

        bus.emit_state_changed("recording", "").await;
        bus.emit_state_changed("error", "no text field is focused").await;
        bus.set_property("State", PropertyValue::Str("recording".into()))
            .await;
        bus.set_property("State", PropertyValue::Str("error".into())).await;
        bus.set_property("AudioRms", PropertyValue::F64(0.42)).await;

        assert_eq!(
            bus.signals(),
            vec![
                ("recording".to_string(), String::new()),
                (
                    "error".to_string(),
                    "no text field is focused".to_string()
                ),
            ],
            "every StateChanged recorded in order, with its args"
        );
        assert_eq!(
            bus.property("State"),
            Some(PropertyValue::Str("error".into())),
            "latest property snapshot wins"
        );
        assert_eq!(
            bus.property("AudioRms"),
            Some(PropertyValue::F64(0.42)),
        );
        assert_eq!(bus.property("AudioPeak"), None, "unset properties stay absent");
    }
}
