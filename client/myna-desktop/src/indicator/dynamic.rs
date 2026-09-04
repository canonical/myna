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
    async fn set_state(&mut self, state: IndicatorState) {
        let is_hidden = matches!(state, IndicatorState::Hidden);
        // Always publish via D-Bus for the HUD(s).
        self.dbus.set_state(state.clone()).await;
        // Suppress the notification fallback while any HUD is present;
        // otherwise forward to it. When a HUD appears while a fallback
        // notification is already visible, the next state transition will
        // hide it (and `hide()` below also handles the idle transition).
        if self.has_clients() {
            if is_hidden {
                self.notify.hide().await;
            } else {
                // Ensure any previous fallback toast is closed — we are now
                // suppressing.
                self.notify.hide().await;
            }
        } else {
            self.notify.set_state(state).await;
        }
    }

    async fn hide(&mut self) {
        self.dbus.hide().await;
        self.notify.hide().await;
    }
}
