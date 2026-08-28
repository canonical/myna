//! The real `zbus`-backed [`Bus`]: serves `com.canonical.Myna.Dictation` at
//! `/com/canonical/Myna/Dictation` on the session bus (feature 004, contract
//! dbus-interface.md §Bus topology). State + level only — the property shapes
//! are `s`/`d`, so no transcript-bearing value can cross (C3).
//!
//! Name lifecycle: requested at [`ZbusBus::serve`], released when the
//! connection drops at shutdown (P13/P14; the gated round-trip suite proves
//! it). Method handling (`Start`/`Stop`/`Toggle`) lands with `DbusTrigger`
//! (US4).
//!
//! The name doubles as the daemon's **singleton lock**: exactly one
//! `myna-desktop` may own it, and failing to get it is fatal rather than a
//! degrade (see [`ServeError::AlreadyRunning`]).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use zbus::fdo::{DBusProxy, RequestNameFlags, RequestNameReply};
use zbus::names::BusName;
use zbus::Connection;

use crate::dbus::{Bus, PropertyValue, BUS_NAME, OBJECT_PATH};

/// Why [`ZbusBus::serve`] failed. The two arms have opposite dispositions:
/// [`Self::Bus`] degrades to notifications, [`Self::AlreadyRunning`] must not.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    /// Another `myna-desktop` owns the name. A second daemon can never be a
    /// working daemon: the GlobalShortcuts portal keeps the hotkey with
    /// whoever bound it first, while the indicator name would go to whoever
    /// started last, so the process holding the key and the process holding
    /// the UI are different ones and every press looks like nothing happened.
    #[error("another myna-desktop already owns {BUS_NAME}")]
    AlreadyRunning {
        /// PID of the owner, when the bus will tell us.
        owner_pid: Option<u32>,
    },
    /// The session bus is unreachable or unusable - fall back to
    /// notifications (P15).
    #[error(transparent)]
    Bus(#[from] zbus::Error),
}

/// The served property values (the `com.canonical.Myna.Dictation` members — the
/// interface defines no signals; every update is pushed via the standard
/// `PropertiesChanged`, the one broadcast that crosses snap confinement to
/// unconfined subscribers, contract §Confinement). `State` starts `idle`,
/// levels at floor, no error — the dormant snapshot a name-appeared client
/// reads (X8).
#[derive(Debug, Default)]
struct ServedState {
    state: String,
    audio_rms: f64,
    audio_peak: f64,
    error_message: String,
}

impl ServedState {
    fn new() -> Self {
        Self {
            state: crate::indicator::dbus::wire_state::IDLE.to_string(),
            ..Default::default()
        }
    }
}

/// The `com.canonical.Myna.Dictation` object. Properties read the shared [`ServedState`]
/// (updated by the publisher through the [`Bus`] seam); the `Start`/`Stop`/
/// `Toggle` methods feed a [`DbusTriggerSource`] when one is attached (the
/// panel-button activation path, P9–P12/C6), and are otherwise no-ops.
struct DictationObject {
    served: Arc<Mutex<ServedState>>,
    trigger: Option<crate::shortcut::dbus::DbusTriggerSource>,
}

#[zbus::interface(name = "com.canonical.Myna.Dictation")]
impl DictationObject {
    /// `Start`: begin a session (a Press edge for the trigger — C6).
    async fn start(&self) -> (bool, String) {
        if let Some(trigger) = &self.trigger {
            trigger.start();
        }
        // The reason is content-free and this object is only a surface; a
        // startability refusal (C7/P11) is signalled upstream before the
        // trigger is pushed, so here it always succeeds.
        (true, String::new())
    }

    /// `Stop`: end the session (a Release edge — C6).
    async fn stop(&self) {
        if let Some(trigger) = &self.trigger {
            trigger.stop();
        }
    }

    /// `Toggle`: Start if idle, else Stop (the panel-button action, C6).
    async fn toggle(&self) {
        if let Some(trigger) = &self.trigger {
            trigger.toggle();
        }
    }

    #[zbus(property)]
    async fn state(&self) -> String {
        self.served
            .lock()
            .expect("served state poisoned")
            .state
            .clone()
    }

    #[zbus(property)]
    async fn audio_rms(&self) -> f64 {
        self.served.lock().expect("served state poisoned").audio_rms
    }

    #[zbus(property)]
    async fn audio_peak(&self) -> f64 {
        self.served
            .lock()
            .expect("served state poisoned")
            .audio_peak
    }

    #[zbus(property)]
    async fn error_message(&self) -> String {
        self.served
            .lock()
            .expect("served state poisoned")
            .error_message
            .clone()
    }
}

/// The real session-bus [`Bus`]. Best-effort: a bus hiccup is logged, never
/// fatal — dictation must not die because the indicator surface did (P15).
pub struct ZbusBus {
    conn: Connection,
    served: Arc<Mutex<ServedState>>,
}

impl std::fmt::Debug for ZbusBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZbusBus").field("name", &BUS_NAME).finish()
    }
}

impl ZbusBus {
    /// Connect to the session bus, serve `/com/canonical/Myna/Dictation`, and take the
    /// well-known name (C1). [`ServeError::Bus`] means the bus is unreachable
    /// and the caller falls back to `NotifyIndicator` (P15);
    /// [`ServeError::AlreadyRunning`] means a second daemon and is fatal.
    pub async fn serve() -> Result<Self, ServeError> {
        Self::serve_with_trigger(None).await
    }

    /// Like [`serve`](Self::serve), but attaches a [`DbusTriggerSource`] so
    /// the served `Start`/`Stop`/`Toggle` methods feed the panel-button
    /// trigger (T140/T141).
    pub async fn serve_with_trigger(
        trigger: Option<crate::shortcut::dbus::DbusTriggerSource>,
    ) -> Result<Self, ServeError> {
        let conn = connect_session().await?;
        let served = Arc::new(Mutex::new(ServedState::new()));
        conn.object_server()
            .at(
                OBJECT_PATH,
                DictationObject {
                    served: Arc::clone(&served),
                    trigger,
                },
            )
            .await?;
        // `DoNotQueue` alone. zbus's default is all three flags, which makes
        // the name last-writer-wins in both directions: without
        // `AllowReplacement` a later daemon cannot take the indicator from
        // us, and without `ReplaceExisting` we cannot take it from an
        // earlier one - the request just reports who is already there.
        //
        // "Already there" arrives two ways: zbus turns the bus's `Exists`
        // reply into `Error::NameTaken`, and a queued request (impossible
        // under `DoNotQueue`, but the reply shape allows it) says the same.
        // Both are the singleton violation, not a bus fault.
        let reply = match conn
            .request_name_with_flags(BUS_NAME, RequestNameFlags::DoNotQueue.into())
            .await
        {
            Ok(reply) => reply,
            Err(zbus::Error::NameTaken) => {
                return Err(ServeError::AlreadyRunning {
                    owner_pid: name_owner_pid(&conn).await,
                })
            }
            Err(e) => return Err(ServeError::Bus(e)),
        };
        match reply {
            RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner => {
                myna_core::info_log!("dbus", "acquired {BUS_NAME}");
                Ok(Self { conn, served })
            }
            RequestNameReply::Exists | RequestNameReply::InQueue => {
                Err(ServeError::AlreadyRunning {
                    owner_pid: name_owner_pid(&conn).await,
                })
            }
        }
    }

    /// The connection, for components that need it (the method-serving
    /// `DbusTrigger` wiring in US4).
    pub fn connection(&self) -> &Connection {
        &self.conn
    }
}

/// PID of whoever currently owns [`BUS_NAME`], for the "already running"
/// message. Best-effort: the answer is advice to a human, not control flow.
async fn name_owner_pid(conn: &Connection) -> Option<u32> {
    let name = BusName::try_from(BUS_NAME).ok()?;
    DBusProxy::new(conn)
        .await
        .ok()?
        .get_connection_unix_process_id(name)
        .await
        .ok()
}

/// Connect to the session bus, recovering from a stale `guid=` in
/// `DBUS_SESSION_BUS_ADDRESS`.
///
/// A tmux/screen server (or any process) that survives logout keeps the
/// *previous* session bus's address — same `unix:path=$XDG_RUNTIME_DIR/bus`,
/// but with the old bus's `guid=`. libdbus/GIO ignore the guid hint and just
/// connect; zbus verifies it and refuses ("Server GUID mismatch"). Since the
/// socket path is authoritative and the guid is only an optional hint, on that
/// specific failure we retry once with the guid stripped — matching every
/// other D-Bus client — rather than dropping a working session to the
/// notification fallback.
///
/// Shared by every session-bus client in this crate (the publisher, the
/// GlobalShortcuts portal trigger): `Connection::session()` alone would
/// hard-fail in the stale-guid environment.
pub(crate) async fn connect_session() -> zbus::Result<Connection> {
    match Connection::session().await {
        Err(zbus::Error::Handshake(msg)) if msg.contains("GUID mismatch") => {
            let Some(address) = sanitized_session_address() else {
                return Err(zbus::Error::Handshake(msg));
            };
            eprintln!(
                "note: DBUS_SESSION_BUS_ADDRESS carries a stale guid (survived logout, \
                 e.g. tmux/screen); retrying at the socket path without it"
            );
            zbus::conn::Builder::address(address.as_str())?
                .build()
                .await
        }
        other => other,
    }
}

/// `DBUS_SESSION_BUS_ADDRESS` with every `guid=…` hint dropped, or `None` when
/// the variable is unset or had no guid to strip (nothing to retry).
fn sanitized_session_address() -> Option<String> {
    let raw = std::env::var("DBUS_SESSION_BUS_ADDRESS").ok()?;
    let stripped = strip_guid(&raw);
    (stripped != raw).then_some(stripped)
}

/// Drop `guid=` key/value pairs from a D-Bus address (`;`-separated entries,
/// each `transport:key=val,key=val`). Pure, for the unit test.
fn strip_guid(address: &str) -> String {
    address
        .split(';')
        .map(|entry| {
            entry
                .split(',')
                .filter(|kv| !kv.starts_with("guid="))
                .collect::<Vec<_>>()
                .join(",")
        })
        .collect::<Vec<_>>()
        .join(";")
}

#[async_trait]
impl Bus for ZbusBus {
    async fn set_property(&mut self, name: &str, value: PropertyValue) {
        let result = async {
            // Update the served snapshot, then emit PropertiesChanged with the
            // fresh value via the macro-generated helper.
            {
                let mut served = self.served.lock().expect("served state poisoned");
                match (name, &value) {
                    ("State", PropertyValue::Str(s)) => served.state = s.clone(),
                    ("ErrorMessage", PropertyValue::Str(s)) => served.error_message = s.clone(),
                    ("AudioRms", PropertyValue::F64(d)) => served.audio_rms = *d,
                    ("AudioPeak", PropertyValue::F64(d)) => served.audio_peak = *d,
                    _ => {
                        myna_core::dbg_log!("dbus", "ignoring unknown property set: {name}");
                        return Ok(());
                    }
                }
            }
            let iface_ref = self
                .conn
                .object_server()
                .interface::<_, DictationObject>(OBJECT_PATH)
                .await?;
            let iface = iface_ref.get().await;
            let emitter = iface_ref.signal_emitter();
            match name {
                "State" => iface.state_changed(emitter).await,
                "ErrorMessage" => iface.error_message_changed(emitter).await,
                "AudioRms" => iface.audio_rms_changed(emitter).await,
                "AudioPeak" => iface.audio_peak_changed(emitter).await,
                _ => Ok(()),
            }
        }
        .await;
        if let Err(e) = result {
            myna_core::dbg_log!("dbus", "property set {name} failed: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::strip_guid;

    #[test]
    fn strip_guid_removes_the_stale_hint_keeps_the_path() {
        assert_eq!(
            strip_guid("unix:path=/run/user/1000/bus,guid=07b3a7a2051b7e37"),
            "unix:path=/run/user/1000/bus"
        );
    }

    #[test]
    fn strip_guid_is_a_noop_without_a_guid() {
        let addr = "unix:path=/run/user/1000/bus";
        assert_eq!(strip_guid(addr), addr);
    }

    #[test]
    fn strip_guid_handles_multiple_address_entries() {
        // dbus-daemon emits the guid as a trailing param; a bus address may
        // list several `;`-separated entries.
        assert_eq!(
            strip_guid(
                "unix:path=/run/user/1000/bus,guid=deadbeef;unix:abstract=/tmp/foo,guid=cafe"
            ),
            "unix:path=/run/user/1000/bus;unix:abstract=/tmp/foo"
        );
    }
}
