//! The real `zbus`-backed [`Bus`]: serves `org.myna.Dictation` at
//! `/org/myna/Dictation` on the session bus (feature 004, contract
//! dbus-interface.md §Bus topology). State + level only — the property shapes
//! are `s`/`d`, so no transcript-bearing value can cross (C3).
//!
//! Name lifecycle: requested at [`ZbusBus::serve`], released when the
//! connection drops at shutdown (P13/P14; the gated round-trip suite proves
//! it). Method handling (`Start`/`Stop`/`Toggle`) lands with `DbusTrigger`
//! (US4).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use zbus::object_server::SignalEmitter;
use zbus::Connection;

use crate::dbus::{Bus, PropertyValue, BUS_NAME, OBJECT_PATH};

/// The served property values (the `org.myna.Dictation` members). `State`
/// starts `idle`, levels at floor, no error — the dormant snapshot a
/// name-appeared client reads (X8).
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

/// The `org.myna.Dictation` object. Properties read the shared [`ServedState`]
/// (updated by the publisher through the [`Bus`] seam); the `StateChanged`
/// signal is emitted by the publisher via [`DictationObject::state_changed`].
struct DictationObject {
    served: Arc<Mutex<ServedState>>,
}

#[zbus::interface(name = "org.myna.Dictation")]
impl DictationObject {
    #[zbus(property)]
    async fn state(&self) -> String {
        self.served.lock().expect("served state poisoned").state.clone()
    }

    #[zbus(property)]
    async fn audio_rms(&self) -> f64 {
        self.served.lock().expect("served state poisoned").audio_rms
    }

    #[zbus(property)]
    async fn audio_peak(&self) -> f64 {
        self.served.lock().expect("served state poisoned").audio_peak
    }

    #[zbus(property)]
    async fn error_message(&self) -> String {
        self.served
            .lock()
            .expect("served state poisoned")
            .error_message
            .clone()
    }

    /// `StateChanged(s state, s error_message)` — one per state transition
    /// (C2). Named distinctly in Rust so it can't collide with the
    /// property-change helper the macro generates for the `State` property.
    #[zbus(signal, name = "StateChanged")]
    async fn publish_state_changed(
        emitter: &SignalEmitter<'_>,
        state: String,
        error_message: String,
    ) -> zbus::Result<()>;
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
    /// Connect to the session bus, serve `/org/myna/Dictation`, and request
    /// the well-known name (C1). `Err` when the bus is unreachable — the
    /// caller falls back to `NotifyIndicator` (P15).
    pub async fn serve() -> zbus::Result<Self> {
        let conn = connect_session().await?;
        let served = Arc::new(Mutex::new(ServedState::new()));
        conn.object_server()
            .at(OBJECT_PATH, DictationObject { served: Arc::clone(&served) })
            .await?;
        conn.request_name(BUS_NAME).await?;
        Ok(Self { conn, served })
    }

    /// The connection, for components that need it (the method-serving
    /// `DbusTrigger` wiring in US4).
    pub fn connection(&self) -> &Connection {
        &self.conn
    }
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
            zbus::conn::Builder::address(address.as_str())?.build().await
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
    async fn emit_state_changed(&mut self, state: &str, error_message: &str) {
        let result = async {
            let iface_ref = self
                .conn
                .object_server()
                .interface::<_, DictationObject>(OBJECT_PATH)
                .await?;
            DictationObject::publish_state_changed(
                iface_ref.signal_emitter(),
                state.to_string(),
                error_message.to_string(),
            )
            .await
        }
        .await;
        if let Err(e) = result {
            myna_core::dbg_log!("dbus", "StateChanged emit failed: {e}");
        }
    }

    async fn set_property(&mut self, name: &str, value: PropertyValue) {
        let result = async {
            // Update the served snapshot, then emit PropertiesChanged with the
            // fresh value via the macro-generated helper.
            {
                let mut served = self.served.lock().expect("served state poisoned");
                match (name, &value) {
                    ("State", PropertyValue::Str(s)) => served.state = s.clone(),
                    ("ErrorMessage", PropertyValue::Str(s)) => {
                        served.error_message = s.clone()
                    }
                    ("AudioRms", PropertyValue::F64(d)) => served.audio_rms = *d,
                    ("AudioPeak", PropertyValue::F64(d)) => served.audio_peak = *d,
                    _ => {
                        myna_core::dbg_log!(
                            "dbus",
                            "ignoring unknown property set: {name}"
                        );
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
            strip_guid("unix:path=/run/user/1000/bus,guid=deadbeef;unix:abstract=/tmp/foo,guid=cafe"),
            "unix:path=/run/user/1000/bus;unix:abstract=/tmp/foo"
        );
    }
}
