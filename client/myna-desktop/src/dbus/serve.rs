//! The real `zbus`-backed [`Bus`]: serves `com.canonical.Myna.Dictation` at
//! `/com/canonical/Myna/Dictation` on the session bus (feature 004, contract
//! dbus-interface.md §Bus topology). State, level and the HUD's presentation
//! style, plus `DraftText` and `DraftTail`: the only transcript-bearing
//! values, empty unless the user turned processing on (C3).
//!
//! Name lifecycle: requested at [`ZbusBus::serve`], released when the
//! connection drops at shutdown (P13/P14; the gated round-trip suite proves
//! it). `Toggle` pokes the daemon's [`ControlTrigger`], which is what the
//! desktop custom shortcut calls. `PreviewSounds` plays a sound set through the
//! daemon's own [`Previewer`], for Myna Settings' Preview button: the cues are
//! compiled into the daemon, so the daemon is the one that can play them.
//!
//! [`ControlTrigger`]: crate::shortcut::control::ControlTrigger
//! [`Previewer`]: crate::sound::player::Previewer
//!
//! The name doubles as the daemon's **singleton lock**: exactly one
//! `myna-desktop` may own it, and failing to get it is fatal rather than a
//! degrade (see [`ServeError::AlreadyRunning`]).

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use zbus::fdo::{DBusProxy, RequestNameFlags, RequestNameReply};
use zbus::message::Header;
use zbus::names::BusName;
use zbus::Connection;

use crate::dbus::{Bus, PropertyValue, BUS_NAME, OBJECT_PATH};
use crate::indicator::dbus::wire_state;
use crate::sound::player::Previewer;

/// Why [`ZbusBus::serve`] failed. The two arms have opposite dispositions:
/// [`Self::Bus`] degrades to notifications, [`Self::AlreadyRunning`] must not.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    /// Another `myna-desktop` owns the name. A second daemon can never be a
    /// working daemon: the shortcut reaches whoever owns the name, so the
    /// second one would never hear a press.
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

/// The `com.canonical.Myna.Dictation` object and the properties it serves,
/// declared once: the D-Bus name, the served field, its type, its default,
/// and how a set reaches it and its `PropertiesChanged`. The interface
/// defines no signals; every update is pushed via the standard
/// `PropertiesChanged`, the one broadcast that crosses snap confinement to
/// unconfined subscribers (contract §Confinement).
///
/// Adding a property is one row in the table at the bottom and nothing
/// else: the getter clients read, the snapshot field, the set and the change
/// emission all come from it. The rest of the interface's members follow the
/// table's marker, unchanged.
macro_rules! dictation_object {
    (
        { $( $methods:item )* }
        @properties {
            $( $(#[$doc:meta])* $name:literal => $field:ident : $ty:ty, $variant:ident,
               $changed:ident = $default:expr; )*
        }
    ) => {
        #[derive(Debug)]
        struct ServedState {
            $( $(#[$doc])* $field: $ty, )*
        }

        impl ServedState {
            fn new() -> Self {
                Self { $( $field: $default, )* }
            }

            /// Apply one set to the snapshot; `false` for a name this
            /// interface does not serve.
            fn set(&mut self, name: &str, value: &PropertyValue) -> bool {
                match (name, value) {
                    $( ($name, PropertyValue::$variant(v)) => {
                        self.$field = v.clone();
                        true
                    } )*
                    _ => false,
                }
            }
        }

        #[zbus::interface(name = "com.canonical.Myna.Dictation")]
        impl DictationObject {
            $(
                $(#[$doc])*
                #[zbus(property)]
                async fn $field(&self) -> $ty {
                    self.served
                        .lock()
                        .expect("served state poisoned")
                        .$field
                        .clone()
                }
            )*

            $( $methods )*
        }

        impl DictationObject {
            /// Emit `PropertiesChanged` for `name`, or nothing for a name
            /// this interface does not serve.
            async fn announce(
                &self,
                emitter: &zbus::object_server::SignalEmitter<'_>,
                name: &str,
            ) -> zbus::Result<()> {
                match name {
                    $( $name => self.$changed(emitter).await, )*
                    _ => Ok(()),
                }
            }
        }
    };
}

dictation_object! {
    {

    /// `Toggle`: start dictation if idle, else stop it.
    async fn toggle(&self) {
        if let Some(trigger) = &self.trigger {
            trigger.poke();
        }
    }

    /// `PreviewSounds`: play the start, stop and error cues of the set named
    /// by its `sound-set` nick, whatever the setting holds. Returns once the
    /// preview is queued, not when it has played.
    async fn preview_sounds(&self, set: &str) -> Result<(), PreviewError> {
        let state = self
            .served
            .lock()
            .expect("served state poisoned")
            .state
            .clone();
        let result = preview_sounds(&state, set, self.preview.as_ref());
        myna_core::info_log!("dbus", "PreviewSounds {set} -> {result:?}");
        result
    }

    /// `RescanModels`: read every text share's capabilities again and bind
    /// what is now covered, without waiting for the next poll.
    async fn rescan_models(&self) {
        myna_core::info_log!("dbus", "RescanModels");
        self.rescan.notify_one();
    }

    /// `RegisterClient`: a HUD client announces itself. The sender's unique
    /// bus name is the identity; the call is idempotent and the server
    /// monitors `NameOwnerChanged` so a crashed client is pruned without an
    /// explicit `UnregisterClient`. Returns the current client count.
    async fn register_client(&self, #[zbus(header)] header: Header<'_>) -> u32 {
        let sender = header.sender().map(|s| s.to_string()).unwrap_or_default();
        if sender.is_empty() {
            return self.clients.len() as u32;
        }
        let count = self.clients.insert(sender.clone());
        myna_core::info_log!("dbus", "RegisterClient {sender} -> {count} client(s)");
        count
    }

    /// `UnregisterClient`: the HUD is going away. Idempotent.
    async fn unregister_client(&self, #[zbus(header)] header: Header<'_>) -> u32 {
        let sender = header.sender().map(|s| s.to_string()).unwrap_or_default();
        if sender.is_empty() {
            return self.clients.len() as u32;
        }
        let count = self.clients.remove(&sender);
        myna_core::info_log!("dbus", "UnregisterClient {sender} -> {count} client(s)");
        count
    }

    #[zbus(property)]
    /// Setting `ActiveChain` writes the settings store, which stays the
    /// source of truth: the property follows once the daemon has read it
    /// back. An id no saved chain has fails; a chain whose models are
    /// missing is allowed and runs degraded.
    #[zbus(property)]
    async fn set_active_chain(&mut self, id: String) -> zbus::fdo::Result<()> {
        // On zbus's executor, which is no tokio runtime: a keyfile write is
        // short enough to make in place.
        let wanted = (!id.is_empty()).then_some(id.as_str());
        let result = myna_core::chains::activate_in_store(wanted);
        myna_core::info_log!("dbus", "ActiveChain {id:?} -> {result:?}");
        match result {
            Ok(()) => Ok(()),
            Err(myna_core::chains::ActivateError::Unknown(id)) => Err(
                zbus::fdo::Error::InvalidArgs(format!("no saved chain {id:?}")),
            ),
            Err(myna_core::chains::ActivateError::Store(why)) => Err(zbus::fdo::Error::Failed(why)),
        }
    }
    }
    @properties {
    /// `idle`, `listening`, ...; see [`crate::indicator::dbus::wire_state`].
    "State" => state: String, Str, state_changed = wire_state::IDLE.to_string();
    "StatusMessage" => status_message: String, Str, status_message_changed = String::new();
    "AudioRms" => audio_rms: f64, F64, audio_rms_changed = 0.0;
    "AudioPeak" => audio_peak: f64, F64, audio_peak_changed = 0.0;
    /// Which audio-level presentation the HUD should draw, as the
    /// `hud-style` settings nick. The renderer reads no settings of its
    /// own - see `dbus::hud_style` for why that reader was removed.
    "HudStyle" => hud_style: String, Str, hud_style_changed = myna_core::Settings::load()
        .hud_style
        .unwrap_or_else(|| myna_core::settings::DEFAULT_HUD_STYLE.to_string());
    /// How the key reaches the daemon; always `control` now. Read here, not
    /// left empty: a HUD that connects between `serve()` and the first
    /// publish reads this snapshot.
    "Activation" => activation: String, Str, activation_changed = String::new();
    /// Chunks refused because the session was already over. Only ever a bug.
    "AudioDroppedNotActive" => audio_dropped_not_active: u64, U64,
        audio_dropped_not_active_changed = 0;
    /// The last session's median chunk mean, signed linear full scale (its
    /// DC offset); NaN until a session has ended.
    "InputOffset" => input_offset: f64, F64, input_offset_changed = f64::NAN;
    /// The quietest the last session's input got.
    "InputNoiseFloor" => input_noise_floor: f64, F64, input_noise_floor_changed = f64::NAN;
    /// The loudest sustained voice in the last session, 0 when none was heard.
    "InputSpeechLevel" => input_speech_level: f64, F64,
        input_speech_level_changed = f64::NAN;
    /// The last session's speech-band signal-to-noise ratio in dB; NaN until
    /// a session has ended, or when it had too little sound.
    "InputSnr" => input_snr: f64, F64, input_snr_changed = f64::NAN;
    /// The latest failure's headline, as `StatusMessage` showed it; empty
    /// until one happens, and never cleared. Sticky: see
    /// [`crate::dbus::publish_last_error`].
    "LastError" => last_error: String, Str, last_error_changed = String::new();
    /// The untranslated cause behind `LastError`. May name paths; consumers
    /// redact before showing or exporting it.
    "LastErrorDetail" => last_error_detail: String, Str,
        last_error_detail_changed = String::new();
    /// When it happened, in microseconds since the epoch; 0 for never.
    "LastErrorTime" => last_error_time: i64, I64, last_error_time_changed = 0;
    /// Text heard in this dictation and not yet written to the field. Empty
    /// unless the user turned processing on; the one place transcript
    /// content crosses this bus.
    "DraftText" => draft_text: String, Str, draft_text_changed = String::new();
    /// The recogniser's current guess at what follows `DraftText`.
    "DraftTail" => draft_tail: String, Str, draft_tail_changed = String::new();
    /// Whether every model step of the chosen processing chain has a model
    /// (`ready`), some has none and is skipped (`degraded`), or one was
    /// connected after the daemon started and only a restart sees it.
    "ChainStatus" => chain_status: String, Str, chain_status_changed =
        crate::dbus::chain::READY.to_string();
    /// The tasks (`text.translate`) no connected model covers, comma
    /// separated; empty while the chain is `ready`.
    "ChainMissing" => chain_missing: String, Str, chain_missing_changed = String::new();
    /// JSON list of `{task, params}` no connected model covers; `[]` while
    /// the chain is `ready`.
    "ChainMissingRequests" => chain_missing_requests: String, Str,
        chain_missing_requests_changed = "[]".to_string();
    /// JSON `{models, steps}`: every model of every connected text share and
    /// what became of each step of the chain.
    "InstalledSteps" => installed_steps: String, Str, installed_steps_changed =
        r#"{"models":[],"steps":[]}"#.to_string();
    /// JSON list of every saved chain in the user's order, each
    /// `{id, name, steps, ready, missing, missing_requests}`.
    "Chains" => chains: String, Str, chains_changed = "[]".to_string();
    /// The id of the chain in use; empty for none.
    "ActiveChain" => active_chain: String, Str, active_chain_changed = String::new();
    }
}
/// Tracks which HUD clients are currently registered via
/// `RegisterClient`/`UnregisterClient`. The sender's unique bus name
/// (`:1.xxx`) is the key, so a client that dies without unregistering can be
/// pruned when its name vanishes.
#[derive(Debug, Default)]
pub struct ClientRegistry {
    clients: Mutex<HashSet<String>>,
    has_clients: AtomicBool,
}

impl ClientRegistry {
    pub(crate) fn insert(&self, sender: String) -> u32 {
        let mut clients = self.clients.lock().expect("client registry poisoned");
        clients.insert(sender);
        let count = clients.len() as u32;
        self.has_clients
            .store(!clients.is_empty(), Ordering::SeqCst);
        count
    }

    pub(crate) fn remove(&self, sender: &str) -> u32 {
        let mut clients = self.clients.lock().expect("client registry poisoned");
        clients.remove(sender);
        let count = clients.len() as u32;
        self.has_clients
            .store(!clients.is_empty(), Ordering::SeqCst);
        count
    }

    pub(crate) fn remove_if_present(&self, sender: &str) -> bool {
        let mut clients = self.clients.lock().expect("client registry poisoned");
        let removed = clients.remove(sender);
        if removed {
            self.has_clients
                .store(!clients.is_empty(), Ordering::SeqCst);
        }
        removed
    }

    /// Whether at least one client is currently registered.
    pub fn has_clients(&self) -> bool {
        self.has_clients.load(Ordering::SeqCst)
    }

    /// Number of registered clients.
    fn len(&self) -> usize {
        self.clients.lock().expect("client registry poisoned").len()
    }
}

/// The `com.canonical.Myna.Dictation` object. Properties read the shared [`ServedState`]
/// (updated by the publisher through the [`Bus`] seam); `Toggle` pokes the
/// trigger when one is attached, and is otherwise a no-op.
struct DictationObject {
    served: Arc<Mutex<ServedState>>,
    trigger: Option<crate::shortcut::control::Poke>,
    preview: Option<Previewer>,
    clients: Arc<ClientRegistry>,
    rescan: Arc<tokio::sync::Notify>,
}

/// Why `PreviewSounds` played nothing, as
/// `com.canonical.Myna.Dictation.Error.<variant>` on the wire so Myna Settings
/// can tell the user which.
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "com.canonical.Myna.Dictation.Error")]
pub enum PreviewError {
    #[zbus(error)]
    ZBus(zbus::Error),
    /// A session is under way: the microphone would record the preview.
    Busy(String),
    /// A preview is already playing; a second would replay over its end.
    AlreadyPlaying(String),
    /// Not a `sound-set` nick this daemon knows.
    UnknownSoundSet(String),
    /// This daemon runs without a sound player.
    NoPlayer(String),
}

/// `PreviewSounds`, off the bus. Only between sessions: from the press until
/// the session is over the microphone may be open, and a preview played then
/// would be dictated.
fn preview_sounds(
    state: &str,
    nick: &str,
    preview: Option<&Previewer>,
) -> Result<(), PreviewError> {
    let set = myna_core::SoundSet::from_nick(nick)
        .ok_or_else(|| PreviewError::UnknownSoundSet(format!("no sound set {nick:?}")))?;
    if ![wire_state::IDLE, wire_state::NOTICE, wire_state::ERROR].contains(&state) {
        return Err(PreviewError::Busy(format!("dictation is {state}")));
    }
    let preview =
        preview.ok_or_else(|| PreviewError::NoPlayer("no sound player is running".into()))?;
    preview
        .preview(set)
        .map_err(|_| PreviewError::AlreadyPlaying("a preview is already playing".into()))
}

/// The real session-bus [`Bus`]. Best-effort: a bus hiccup is logged, never
/// fatal — dictation must not die because the indicator surface did (P15).
pub struct ZbusBus {
    conn: Connection,
    served: Arc<Mutex<ServedState>>,
    clients: Arc<ClientRegistry>,
    /// Flips to `true` once the connection behind `conn` is gone for good.
    lost: tokio::sync::watch::Sender<bool>,
}

impl ZbusBus {
    /// Connect to the session bus, serve `/com/canonical/Myna/Dictation`, and take the
    /// well-known name (C1). [`ServeError::Bus`] means the bus is unreachable
    /// and the caller falls back to `NotifyIndicator` (P15);
    /// [`ServeError::AlreadyRunning`] means a second daemon and is fatal.
    pub async fn serve() -> Result<Self, ServeError> {
        Self::serve_with(None, None, Arc::default()).await
    }

    /// Like [`serve`](Self::serve), with `Toggle` poking `trigger` and
    /// `PreviewSounds` playing through `preview`.
    pub async fn serve_with(
        trigger: Option<crate::shortcut::control::Poke>,
        preview: Option<Previewer>,
        rescan: Arc<tokio::sync::Notify>,
    ) -> Result<Self, ServeError> {
        let conn = connect_session().await?;
        let served = Arc::new(Mutex::new(ServedState::new()));
        let clients: Arc<ClientRegistry> = Arc::new(ClientRegistry::default());
        conn.object_server()
            .at(
                OBJECT_PATH,
                DictationObject {
                    served: Arc::clone(&served),
                    trigger,
                    preview,
                    clients: Arc::clone(&clients),
                    rescan,
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
                let bus = Self {
                    conn,
                    served,
                    clients: Arc::clone(&clients),
                    lost: tokio::sync::watch::Sender::new(false),
                };
                bus.spawn_client_prune_task();
                Ok(bus)
            }
            RequestNameReply::Exists | RequestNameReply::InQueue => {
                Err(ServeError::AlreadyRunning {
                    owner_pid: name_owner_pid(&conn).await,
                })
            }
        }
    }

    /// Watch `NameOwnerChanged` for the life of the connection: prune HUD
    /// clients that vanish, and when the stream itself ends - which zbus does
    /// only once the socket is closed - report the bus as lost.
    fn spawn_client_prune_task(&self) {
        let conn = self.conn.clone();
        let clients = Arc::clone(&self.clients);
        let lost = self.lost.clone();
        tokio::spawn(async move {
            let Ok(proxy) = DBusProxy::new(&conn).await else {
                return;
            };
            let Ok(mut stream) = proxy.receive_name_owner_changed().await else {
                return;
            };
            use futures_util::StreamExt;
            while let Some(change) = stream.next().await {
                let Ok(args) = change.args() else { continue };
                // Only care when a unique name loses its owner (client vanished without UnregisterClient).
                if args.new_owner().is_some() {
                    continue;
                }
                let vanished = args.name().as_str().to_string();
                if vanished.starts_with(':') && clients.remove_if_present(&vanished) {
                    myna_core::info_log!(
                        "dbus",
                        "pruned vanished client {vanished} -> {} client(s)",
                        clients.len()
                    );
                }
            }
            let _ = lost.send(true);
        });
    }

    /// Resolves once the session bus behind this publisher is gone (a logout
    /// replaces it), taking the well-known name with it. There is no
    /// reconnecting to a bus: the caller exits non-zero and the service's
    /// `Restart=on-failure` brings a fresh process up on the new one.
    pub fn lost(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        let mut rx = self.lost.subscribe();
        async move {
            // Err means the sender is gone, which is the bus gone too.
            let _ = rx.wait_for(|lost| *lost).await;
        }
    }

    /// The shared client registry, for the dynamic notification fallback.
    pub fn client_registry(&self) -> Arc<ClientRegistry> {
        Arc::clone(&self.clients)
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
/// `Connection::session()` alone would hard-fail in the stale-guid
/// environment.
async fn connect_session() -> zbus::Result<Connection> {
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
            let known = {
                let mut served = self.served.lock().expect("served state poisoned");
                served.set(name, &value)
            };
            if !known {
                myna_core::dbg_log!("dbus", "ignoring unknown property set: {name}");
                return Ok(());
            }
            let iface_ref = self
                .conn
                .object_server()
                .interface::<_, DictationObject>(OBJECT_PATH)
                .await?;
            let iface = iface_ref.get().await;
            iface.announce(iface_ref.signal_emitter(), name).await
        }
        .await;
        if let Err(e) = result {
            myna_core::dbg_log!("dbus", "property set {name} failed: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{preview_sounds, strip_guid, wire_state, ClientRegistry, PreviewError};
    use crate::live::Live;
    use crate::sound::player::Player;

    fn player() -> Player {
        Player::spawn_with_output(Live::new(myna_core::SoundSet::Myna), |_| Ok(()))
            .expect("player thread")
    }

    /// Between sessions every set previews, whatever is showing.
    #[test]
    fn a_preview_plays_between_sessions() {
        for state in [wire_state::IDLE, wire_state::NOTICE, wire_state::ERROR] {
            for set in myna_core::SoundSet::ALL {
                let player = player();
                let result = preview_sounds(state, set.nick(), Some(&player.previewer()));
                assert!(result.is_ok(), "{state} {set:?}: {result:?}");
            }
        }
    }

    /// From the press to the end of the session the microphone may be open,
    /// so nothing plays, the player not even asked.
    #[test]
    fn a_preview_during_a_session_is_refused() {
        let player = player();
        for state in [
            wire_state::LOADING,
            wire_state::RECORDING,
            wire_state::TRANSCRIBING,
            wire_state::FINALIZING,
        ] {
            let result = preview_sounds(state, "myna", Some(&player.previewer()));
            assert!(matches!(result, Err(PreviewError::Busy(_))), "{state}");
        }
        assert!(preview_sounds(wire_state::IDLE, "myna", Some(&player.previewer())).is_ok());
    }

    #[test]
    fn a_preview_of_an_unknown_set_is_refused() {
        let player = player();
        let result = preview_sounds(wire_state::IDLE, "kazoo", Some(&player.previewer()));
        assert!(matches!(result, Err(PreviewError::UnknownSoundSet(_))));
        assert!(preview_sounds(wire_state::IDLE, "myna", Some(&player.previewer())).is_ok());
    }

    #[test]
    fn without_a_player_a_preview_says_so() {
        let result = preview_sounds(wire_state::IDLE, "myna", None);
        assert!(matches!(result, Err(PreviewError::NoPlayer(_))));
    }

    #[test]
    fn a_preview_during_a_preview_is_refused() {
        let player = player();
        let previewer = player.previewer();
        // The output never returns, so the first preview holds the player.
        let (_hold, held) = std::sync::mpsc::channel::<()>();
        let held = std::sync::Mutex::new(held);
        let busy = Player::spawn_with_output(Live::new(myna_core::SoundSet::Myna), move |_| {
            let _ = held.lock().unwrap().recv();
            Ok(())
        })
        .expect("player thread");
        assert!(preview_sounds(wire_state::IDLE, "hum", Some(&busy.previewer())).is_ok());
        let again = preview_sounds(wire_state::IDLE, "tine", Some(&busy.previewer()));
        assert!(matches!(again, Err(PreviewError::AlreadyPlaying(_))));
        assert!(preview_sounds(wire_state::IDLE, "tine", Some(&previewer)).is_ok());
    }

    /// The error names are the contract Myna Settings matches on.
    #[test]
    fn preview_errors_carry_their_names_on_the_wire() {
        use zbus::DBusError;
        for (error, name) in [
            (PreviewError::Busy(String::new()), "Busy"),
            (
                PreviewError::AlreadyPlaying(String::new()),
                "AlreadyPlaying",
            ),
            (
                PreviewError::UnknownSoundSet(String::new()),
                "UnknownSoundSet",
            ),
            (PreviewError::NoPlayer(String::new()), "NoPlayer"),
        ] {
            assert_eq!(
                error.name().as_str(),
                format!("com.canonical.Myna.Dictation.Error.{name}")
            );
        }
    }

    #[test]
    fn client_registry_tracks_sender_and_prunes() {
        let registry = ClientRegistry::default();
        assert!(!registry.has_clients());
        assert_eq!(registry.len(), 0);

        // Register two clients.
        assert_eq!(registry.insert(":1.10".into()), 1);
        assert!(registry.has_clients());
        assert_eq!(registry.insert(":1.21".into()), 2);
        assert_eq!(registry.len(), 2);
        // Idempotent.
        assert_eq!(registry.insert(":1.10".into()), 2);

        // Unregister one.
        assert_eq!(registry.remove(":1.10"), 1);
        assert!(registry.has_clients());
        // Prune via NameOwnerChanged (vanished without unregister).
        assert!(registry.remove_if_present(":1.21"));
        assert!(!registry.has_clients());
        assert_eq!(registry.len(), 0);
        // No-op if not present.
        assert!(!registry.remove_if_present(":1.99"));
    }

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
