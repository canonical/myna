//! The `com.canonical.Myna.Dictation` D-Bus publisher boundary (feature
//! 004-gnome-shell-indicator).
//!
//! `myna-desktop` publishes dictation **state + level only** (never transcript
//! text — constitution V) on the session bus for the GNOME Shell extension
//! (`extensions/myna-shell/`), per `specs/004-gnome-shell-indicator/contracts/
//! dbus-interface.md`. All publisher logic is written against the small [`Bus`]
//! seam so the mapping/throttling is hermetic-testable over [`FakeBus`]
//! (research R11); the real `zbus`-backed implementation that owns the
//! well-known name and serves `/com/canonical/Myna/Dictation` lands in the polish phase
//! (contract publisher.md P13–P15).

pub mod pump;
pub mod serve;
pub mod status;

/// Well-known bus name owned by `myna-desktop --dbus` (contract §Bus topology).
pub const BUS_NAME: &str = "com.canonical.Myna.Dictation";
/// Object path the interface is served at.
pub const OBJECT_PATH: &str = "/com/canonical/Myna/Dictation";
/// Interface name (identical to the bus name for the MVP).
pub const INTERFACE: &str = "com.canonical.Myna.Dictation";

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

/// A property value crossing the bus seam. The contract only uses `s` and `d`
/// shapes (`State`/`StatusMessage` strings, `AudioRms`/`AudioPeak` doubles), so
/// those are the only variants — no transcript-bearing shape exists here (C3).
#[derive(Debug, Clone, PartialEq)]
pub enum PropertyValue {
    /// A D-Bus string (`s`) property.
    Str(String),
    /// A D-Bus double (`d`) property.
    F64(f64),
}

/// The bus boundary all publisher logic is written against (research R11):
/// set a property. Every set is pushed to subscribers with the standard
/// `org.freedesktop.DBus.Properties.PropertiesChanged` — the interface
/// defines no custom signals, because a strictly-confined publisher may not
/// broadcast those to unconfined subscribers while `PropertiesChanged` on its
/// own path crosses confinement freely (contract dbus-interface.md
/// §Confinement). Small enough that the real `zbus` implementation and the
/// hermetic [`FakeBus`] are both trivial, keeping the state/level mapping
/// testable without a session bus.
#[async_trait]
pub trait Bus: Send {
    /// Set a property (`State` / `StatusMessage` / `AudioRms` / `AudioPeak`),
    /// emitting `PropertiesChanged` on the real bus.
    async fn set_property(&mut self, name: &str, value: PropertyValue);
}

/// Shared handle to the bus for the publisher halves (indicator, level pump,
/// trigger). Cloning shares the same underlying bus.
pub type SharedBus = Arc<tokio::sync::Mutex<dyn Bus>>;

/// The served `com.canonical.Myna.Dictation` object: owns the [`Bus`] handle the
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

/// Hermetic in-memory [`Bus`] (research R11): records every property set (in
/// order) and keeps the latest snapshot of each property. A permanent fixture
/// (like `indicator::mock::MockIndicator`) — clone it *before* handing it to
/// the publisher; all clones share the recording.
#[derive(Clone, Default)]
pub struct FakeBus {
    inner: Arc<Mutex<FakeBusInner>>,
}

#[derive(Default)]
struct FakeBusInner {
    sets: Vec<(String, PropertyValue)>,
    properties: HashMap<String, PropertyValue>,
}

impl FakeBus {
    /// A fresh fake with nothing recorded.
    pub fn new() -> Self {
        Self::default()
    }

    /// The `State` values set, in order — the transition sequence a
    /// `PropertiesChanged` subscriber observes (C2).
    pub fn state_history(&self) -> Vec<String> {
        self.inner
            .lock()
            .expect("fake bus poisoned")
            .sets
            .iter()
            .filter_map(|(name, value)| match (name.as_str(), value) {
                ("State", PropertyValue::Str(s)) => Some(s.clone()),
                _ => None,
            })
            .collect()
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
    async fn set_property(&mut self, name: &str, value: PropertyValue) {
        let mut inner = self.inner.lock().expect("fake bus poisoned");
        inner.sets.push((name.to_string(), value.clone()));
        inner.properties.insert(name.to_string(), value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Contract publisher.md P-seam / research R11: the hermetic fake bus
    /// records property sets (in order) and the latest property snapshot, so
    /// publisher logic is testable without a session bus.
    #[tokio::test]
    async fn fake_bus_records_sets_and_latest_properties() {
        let mut bus = FakeBus::new();

        bus.set_property("State", PropertyValue::Str("recording".into()))
            .await;
        bus.set_property("State", PropertyValue::Str("error".into()))
            .await;
        bus.set_property("AudioRms", PropertyValue::F64(0.42)).await;

        assert_eq!(
            bus.state_history(),
            vec!["recording".to_string(), "error".to_string()],
            "every State set recorded in order"
        );
        assert_eq!(
            bus.property("State"),
            Some(PropertyValue::Str("error".into())),
            "latest property snapshot wins"
        );
        assert_eq!(bus.property("AudioRms"), Some(PropertyValue::F64(0.42)),);
        assert_eq!(
            bus.property("AudioPeak"),
            None,
            "unset properties stay absent"
        );
    }
}
