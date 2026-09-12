//! Read the daemon's published state from *outside* the daemon.
//!
//! The serving side of `com.canonical.Myna.Dictation` is [`crate::dbus::serve`]; this is
//! the other end of the same interface, and it exists because the interface is
//! the only place the running daemon's state is legible at all. Everything
//! else about a dictation session is either in the journal (historical) or in
//! the daemon's own memory (private).

use std::collections::HashMap;

use zbus::names::InterfaceName;
use zbus::zvariant::OwnedValue;
use zbus::Connection;

use super::{BUS_NAME, OBJECT_PATH};

/// What the daemon is publishing right now.
#[derive(Debug, Default, PartialEq)]
pub struct DaemonStatus {
    pub state: String,
    /// Publisher-owned label for the current state; empty while idle.
    pub status_message: String,
}

/// Read the published properties, or say why they could not be read.
///
/// A daemon that is not running is not an error here: `--status` answering
/// "not running" is a perfectly good answer, and it is the answer someone
/// running this is most likely looking for.
pub async fn read() -> Result<DaemonStatus, zbus::Error> {
    let connection = Connection::session().await?;
    let proxy = zbus::fdo::PropertiesProxy::builder(&connection)
        .destination(BUS_NAME)?
        .path(OBJECT_PATH)?
        .build()
        .await?;
    let all: HashMap<String, OwnedValue> =
        proxy.get_all(InterfaceName::try_from(BUS_NAME)?).await?;
    Ok(DaemonStatus {
        state: text(&all, "State"),
        status_message: text(&all, "StatusMessage"),
    })
}

fn text(all: &HashMap<String, OwnedValue>, key: &str) -> String {
    all.get(key)
        .and_then(|v| String::try_from(v.try_clone().ok()?).ok())
        .unwrap_or_default()
}

/// Ask the running daemon to raise the portal's bind dialog for the dictation
/// shortcut, returning what it reported.
///
/// The call has to land in the daemon: the portal keys a binding by the
/// caller's app id, so binding from this process would file it under whatever
/// confinement *this* command runs in and leave the daemon still unbound.
pub async fn bind_shortcut(preferred: Option<&str>) -> Result<(bool, String), zbus::Error> {
    let connection = Connection::session().await?;
    connection
        .call_method(
            Some(BUS_NAME),
            OBJECT_PATH,
            Some(BUS_NAME),
            "BindShortcut",
            &(preferred.unwrap_or_default()),
        )
        .await?
        .body()
        .deserialize()
}
