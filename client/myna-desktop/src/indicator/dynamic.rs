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
/// `RegisterClient`/`UnregisterClient` keep the server's `ClientRegistry` up
/// to date and the server prunes vanished unique names via
/// `NameOwnerChanged`, so a crashed HUD is handled without an explicit
/// `UnregisterClient`.
pub struct DynamicIndicator {
    dbus: DbusIndicator,
    notify: NotifyIndicator,
    clients: Arc<ClientRegistry>,
}

impl DynamicIndicator {
    pub fn new(dbus: DbusIndicator, notify: NotifyIndicator, clients: Arc<ClientRegistry>) -> Self {
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
impl Indicator for DynamicIndicator {
    async fn set_audio_drops(&mut self, not_resident: u64, not_active: u64) {
        self.dbus.set_audio_drops(not_resident, not_active).await;
    }

    async fn set_state(&mut self, state: IndicatorState) {
        // Always publish via D-Bus for the HUD(s).
        self.dbus.set_state(state.clone()).await;
        // Suppress the notification fallback while any HUD is present,
        // closing a toast left over from before the HUD appeared.
        if self.has_clients() {
            self.notify.hide().await;
        } else {
            self.notify.set_state(state).await;
        }
    }

    async fn hide(&mut self) {
        self.dbus.hide().await;
        self.notify.hide().await;
    }
}
