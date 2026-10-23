//! Dynamic fallback indicator — wraps the D-Bus publisher and the
//! notification fallback, suppressing the fallback while any HUD client is
//! registered via `RegisterClient` (see `crate::dbus::serve::ClientRegistry`).

use async_trait::async_trait;

use super::dbus::DbusIndicator;
use super::notify::NotifyIndicator;
use super::{Indicator, IndicatorState};
use crate::dbus::serve::ClientRegistry;
use std::sync::Arc;

/// An `Indicator` that always publishes via `com.canonical.Myna.Dictation`
/// (the D-Bus path the HUD consumes) and **also** drives the notification
/// fallback only when no HUD client is registered.
///
/// A client is anything that registered, and the HUD is not the only kind: the
/// process that hosts the HUD (the Xfce and Plasma supervisor, the GNOME extension)
/// registers the moment the daemon's name appears, well before the HUD can,
/// so the toast does not stand in for a pill that is on its way.
///
/// `RegisterClient`/`UnregisterClient` keep the server's `ClientRegistry` up
/// to date and the server prunes vanished unique names via
/// `NameOwnerChanged`, so a crashed HUD is handled without an explicit
/// `UnregisterClient`.
pub struct DynamicIndicator<N = NotifyIndicator> {
    dbus: DbusIndicator,
    notify: N,
    clients: Arc<ClientRegistry>,
}

impl<N> DynamicIndicator<N> {
    pub fn new(dbus: DbusIndicator, notify: N, clients: Arc<ClientRegistry>) -> Self {
        Self {
            dbus,
            notify,
            clients,
        }
    }

    fn has_clients(&self) -> bool {
        self.clients.has_clients()
    }
}

#[async_trait]
impl<N: Indicator> Indicator for DynamicIndicator<N> {
    async fn set_audio_drops(&mut self, not_active: u64) {
        self.dbus.set_audio_drops(not_active).await;
    }

    async fn set_input_levels(&mut self, levels: super::InputLevels) {
        self.dbus.set_input_levels(levels).await;
    }

    async fn set_last_error(&mut self, headline: &str, detail: &str) {
        self.dbus.set_last_error(headline, detail).await;
    }

    async fn set_state(&mut self, state: IndicatorState) {
        // Always publish via D-Bus for the HUD(s).
        self.dbus.set_state(state.clone()).await;
        // Suppress the notification fallback while any HUD is present,
        // closing a toast left over from before the HUD appeared.
        if self.has_clients() {
            self.notify.set_state(IndicatorState::Hidden).await;
        } else {
            self.notify.set_state(state).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dbus::{DictationService, FakeBus};
    use crate::indicator::mock::MockIndicator;
    use crate::indicator::readiness::Readiness;

    struct Rig {
        indicator: DynamicIndicator<MockIndicator>,
        clients: Arc<ClientRegistry>,
        toasts: Arc<std::sync::Mutex<Vec<IndicatorState>>>,
    }

    fn rig() -> Rig {
        let service = DictationService::new(FakeBus::new());
        let dbus = DbusIndicator::new(service.bus(), Readiness::default());
        let notify = MockIndicator::new();
        let toasts = notify.log();
        let clients = Arc::new(ClientRegistry::default());
        Rig {
            indicator: DynamicIndicator::new(dbus, notify, clients.clone()),
            clients,
            toasts,
        }
    }

    fn last(rig: &Rig) -> Option<IndicatorState> {
        rig.toasts.lock().unwrap().last().cloned()
    }

    #[tokio::test]
    async fn with_nobody_registered_a_dictation_falls_back_to_a_toast() {
        let mut rig = rig();
        rig.indicator.set_state(IndicatorState::Recording).await;
        assert_eq!(last(&rig), Some(IndicatorState::Recording));
    }

    /// The HUD needs seconds to start; its host registers first.
    #[tokio::test]
    async fn a_registered_host_with_no_hud_suppresses_the_toast_until_it_unregisters() {
        let mut rig = rig();
        rig.clients.insert(":1.40".into());
        rig.indicator.set_state(IndicatorState::Recording).await;
        assert_eq!(last(&rig), Some(IndicatorState::Hidden));

        rig.clients.remove(":1.40");
        rig.indicator.set_state(IndicatorState::Transcribing).await;
        assert_eq!(last(&rig), Some(IndicatorState::Transcribing));
    }

    #[tokio::test]
    async fn the_host_outlives_its_hud() {
        let mut rig = rig();
        rig.clients.insert(":1.40".into());
        rig.clients.insert(":1.41".into());
        // The HUD dies without unregistering; the host's pill is still due.
        assert!(rig.clients.remove_if_present(":1.41"));
        rig.indicator.set_state(IndicatorState::Recording).await;
        assert_eq!(last(&rig), Some(IndicatorState::Hidden));
        // And the host dying alone ends the suppression.
        assert!(rig.clients.remove_if_present(":1.40"));
        rig.indicator.set_state(IndicatorState::Recording).await;
        assert_eq!(last(&rig), Some(IndicatorState::Recording));
    }
}
