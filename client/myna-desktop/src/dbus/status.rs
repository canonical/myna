//! Read the daemon's published state from *outside* the daemon.
//!
//! The serving side of `org.myna.Dictation` is [`crate::dbus::serve`]; this is
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
    /// Empty when nothing is wrong - the daemon clears it on recovery.
    pub error: String,
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
        error: text(&all, "ErrorMessage"),
    })
}

fn text(all: &HashMap<String, OwnedValue>, key: &str) -> String {
    all.get(key)
        .and_then(|v| String::try_from(v.try_clone().ok()?).ok())
        .unwrap_or_default()
}
