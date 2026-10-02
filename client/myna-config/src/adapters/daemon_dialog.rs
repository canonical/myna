//! The daemon's shortcut dialog, as a restart of Myna's service sees it.
//!
//! GNOME keeps the portal's dialog up whatever happens to the daemon that
//! asked for it, so restarting the daemon under it leaves a dialog nobody
//! waits on. A restart therefore waits for `ShortcutDialog` to go false.

use std::time::Duration;

use async_trait::async_trait;
use gio::prelude::*;
use gtk4::glib;

use crate::adapters::system_configurator::RestartGate;
use crate::command::CancellationToken;
use crate::ports::SystemConfiguratorError;

const DICTATION_BUS: &str = "com.canonical.Myna.Dictation";
const DICTATION_PATH: &str = "/com/canonical/Myna/Dictation";
const POLL: Duration = Duration::from_millis(250);

/// Holds a restart while the daemon has a shortcut dialog up, from any
/// client or its own retry. A daemon that is not running, or predates
/// `ShortcutDialog`, holds nothing.
pub struct DaemonDialogGate;

#[async_trait(?Send)]
impl RestartGate for DaemonDialogGate {
    async fn until_clear(
        &self,
        cancellation: CancellationToken,
    ) -> Result<(), SystemConfiguratorError> {
        let Ok(proxy) = gio::DBusProxy::for_bus_future(
            gio::BusType::Session,
            gio::DBusProxyFlags::DO_NOT_AUTO_START,
            None,
            DICTATION_BUS,
            DICTATION_PATH,
            DICTATION_BUS,
        )
        .await
        else {
            return Ok(());
        };
        let mut held = false;
        while dialog_up(&proxy) {
            if cancellation.is_cancelled() {
                return Err(SystemConfiguratorError::Cancelled);
            }
            if !held {
                held = true;
                glib::g_message!(
                    crate::LOG_DOMAIN,
                    "restart: held until the shortcut dialog is answered"
                );
            }
            glib::timeout_future(POLL).await;
        }
        Ok(())
    }
}

fn dialog_up(proxy: &gio::DBusProxy) -> bool {
    proxy.name_owner().is_some()
        && proxy
            .cached_property("ShortcutDialog")
            .and_then(|value| value.get::<bool>())
            .unwrap_or(false)
}
