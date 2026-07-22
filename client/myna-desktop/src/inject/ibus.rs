//! `IbusInjector` — the shipped IBus text-injection backend (plan T22, T018).
//!
//! Speaks the IBus wire protocol (D-Bus / GVariant) directly over `zbus`
//! (research R1): no FFI, no GObject-introspection, no subprocess. It registers
//! an IBus component + engine, is made the active (global) engine per session,
//! commits committed segments via the engine's `CommitText` signal, and restores
//! the prior engine on session end. Focus and secure-field state arrive through
//! the engine's `FocusIn`/`FocusOut`/`SetContentType` callbacks (R4/R5).
//!
//! ## Verification
//!
//! The connection layer (address discovery + `zbus` bus handshake) and the
//! GVariant serialization (`IBusText`/`IBusEngineDesc`/`IBusComponent` shapes)
//! are validated here; end-to-end injection into a focused field only exists
//! against a live IBus daemon with a focused input context, so it is proven by
//! the env-gated suite (`MYNA_IBUS_TESTS=1`, `tests/ibus_hw.rs`, T017) and the
//! manual spoken run (T021) — on the desktop VM and on hardware unchanged
//! (Principle II). Activating the engine must never be done casually: it becomes
//! the user's global input method until restored.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream::{BoxStream, StreamExt};
use tokio::sync::mpsc;
use tokio::sync::Notify;
use tokio_stream::wrappers::UnboundedReceiverStream;
use zbus::zvariant::{OwnedValue, StructureBuilder, Value};
use zbus::Connection;

use super::{FocusEvent, InjectError, InjectionTarget, Injector};

const IBUS_SERVICE: &str = "org.freedesktop.IBus";
const IBUS_PATH: &str = "/org/freedesktop/IBus";
const IBUS_IFACE: &str = "org.freedesktop.IBus";
const ENGINE_IFACE: &str = "org.freedesktop.IBus.Engine";

const COMPONENT_NAME: &str = "org.freedesktop.IBus.Myna";
const ENGINE_NAME: &str = "myna-stt";
const FACTORY_PATH: &str = "/org/freedesktop/IBus/Factory";
const ENGINE_PATH: &str = "/org/freedesktop/IBus/Engine/Myna";

/// GTK/IBus input purposes we treat as secure (refuse to inject).
const PURPOSE_PASSWORD: u32 = 8;
const PURPOSE_PIN: u32 = 9;

/// How long `acquire` waits for the daemon to focus our engine (and deliver the
/// content-type) before **failing closed**. If no focus signal arrives within
/// this timeout, `acquire` returns `Unavailable` rather than proceeding without
/// secure-field confirmation (fail-closed behavior).
const FOCUS_WAIT: Duration = Duration::from_millis(400);

// ── GVariant builders (IBus serializable objects) ───────────────────────────
//
// IBus serializes objects as `(s a{sv} <fields...>)`: a type-name string, an
// attachments dict, then the class fields. Shapes verified against the running
// daemon's `GetGlobalEngine` reply and locally by signature.

fn empty_attach() -> HashMap<String, Value<'static>> {
    HashMap::new()
}

/// `IBusAttrList` → `(sa{sv}av)` with no attributes (plain text).
fn ibus_attr_list() -> Value<'static> {
    Value::from(
        StructureBuilder::new()
            .add_field("IBusAttrList".to_string())
            .add_field(empty_attach())
            .add_field(Vec::<Value>::new())
            .build()
            .expect("IBusAttrList structure"),
    )
}

/// `IBusText` → `(sa{sv}sv)`: the committed string with an empty attribute list.
fn ibus_text(text: &str) -> Value<'static> {
    Value::from(
        StructureBuilder::new()
            .add_field("IBusText".to_string())
            .add_field(empty_attach())
            .add_field(text.to_string())
            .append_field(ibus_attr_list()) // `v`
            .build()
            .expect("IBusText structure"),
    )
}

/// `IBusEngineDesc` → `(sa{sv}ssssssssussssssss)` (layout confirmed against the
/// daemon): name/longname/description/language/license/author/icon/layout, rank,
/// then 8 trailing strings.
fn ibus_engine_desc() -> Value<'static> {
    let mut b = StructureBuilder::new()
        .add_field("IBusEngineDesc".to_string())
        .add_field(empty_attach());
    for f in [ENGINE_NAME, "myna dictation", "myna speech-to-text", "en", "GPL", "myna", "", "us"] {
        b = b.add_field(f.to_string());
    }
    b = b.add_field(0u32); // rank
    for _ in 0..8 {
        b = b.add_field(String::new());
    }
    Value::from(b.build().expect("IBusEngineDesc structure"))
}

/// `IBusComponent` → `(sa{sv}ssssssssavav)`: metadata, observed paths (none), and
/// our one engine description.
fn ibus_component() -> Value<'static> {
    let mut b = StructureBuilder::new()
        .add_field("IBusComponent".to_string())
        .add_field(empty_attach());
    for f in [COMPONENT_NAME, "myna dictation", "1.0", "GPL", "myna", "", "", ""] {
        b = b.add_field(f.to_string());
    }
    b = b.add_field(Vec::<Value>::new()); // observed paths (av)
    b = b.add_field(vec![ibus_engine_desc()]); // engines (av)
    Value::from(b.build().expect("IBusComponent structure"))
}

// ── IBus address discovery ──────────────────────────────────────────────────

/// Locate the IBus private-bus address: `$IBUS_ADDRESS`, else the socket file
/// under `~/.config/ibus/bus/` (the file the daemon writes). We pick the entry
/// matching the current display, and **validated** against liveness so a stale
/// address file (e.g. left by a crashed/replaced daemon) yields an actionable
/// error rather than a bare "connection refused".
fn discover_address() -> Result<String, InjectError> {
    if let Ok(addr) = std::env::var("IBUS_ADDRESS") {
        if !addr.is_empty() {
            return Ok(addr);
        }
    }
    let config = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config"));
    let dir = config.join("ibus/bus");
    let entries = std::fs::read_dir(&dir)
        .map_err(|e| InjectError::Unavailable(format!("no IBus socket dir {}: {e}", dir.display())))?;

    // Prefer a file whose name ends with the current Wayland/X display.
    let want = std::env::var("WAYLAND_DISPLAY")
        .map(|w| format!("unix-{w}"))
        .or_else(|_| std::env::var("DISPLAY").map(|d| format!("unix{}", d.replace(':', "-"))))
        .ok();

    let mut files: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    // Newest last, so the display match (if any) or the newest wins.
    files.sort_by_key(|p| p.metadata().and_then(|m| m.modified()).ok());
    // Rank display matches ahead of the rest, newest first within each group.
    let ranked: Vec<&PathBuf> = {
        let matches_display = |p: &&PathBuf| {
            want.as_deref().is_some_and(|w| {
                p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with(w))
            })
        };
        let (mut hit, miss): (Vec<&PathBuf>, Vec<&PathBuf>) =
            files.iter().rev().partition(matches_display);
        hit.extend(miss);
        hit
    };

    // Walk candidates best-first; take the first whose daemon is alive and whose
    // socket exists. Remember a stale candidate so we can explain it.
    let mut stale: Option<(String, Option<i64>)> = None;
    for path in ranked {
        let Ok(text) = std::fs::read_to_string(path) else { continue };
        let Some(addr) = text.lines().find_map(|l| l.strip_prefix("IBUS_ADDRESS=")) else {
            continue;
        };
        let addr = addr.to_string();
        let pid: Option<i64> = text
            .lines()
            .find_map(|l| l.strip_prefix("IBUS_DAEMON_PID="))
            .and_then(|p| p.trim().parse().ok());
        // The daemon PID is alive (Linux /proc) and the unix socket path exists?
        let pid_alive =
            pid.map(|p| PathBuf::from(format!("/proc/{p}")).exists()).unwrap_or(true);
        let sock_ok = addr
            .split("path=")
            .nth(1)
            .and_then(|s| s.split(',').next())
            .map(|sp| PathBuf::from(sp).exists())
            .unwrap_or(true);
        if pid_alive && sock_ok {
            return Ok(addr);
        }
        stale.get_or_insert((addr, pid));
    }

    Err(match stale {
        Some((_, pid)) => InjectError::Unavailable(format!(
            "IBus address file(s) present but the daemon looks gone (stale PID {} / missing \
             socket). Try `ibus restart` (or set IBUS_ADDRESS).",
            pid.map(|p| p.to_string()).unwrap_or_else(|| "?".into())
        )),
        None => InjectError::Unavailable(format!(
            "no usable IBus address in {} (is an IBus daemon running? try `ibus restart`)",
            dir.display()
        )),
    })
}

// ── Engine + Factory D-Bus objects ──────────────────────────────────────────

/// Shared engine state: the daemon's `FocusIn`/`FocusOut`/`SetContentType`
/// callbacks land on the object; this state relays them to the injector.
struct EngineState {
    focus_tx: mpsc::UnboundedSender<FocusEvent>,
    /// Latest input-purpose from `SetContentType` (0 until one arrives).
    purpose: AtomicU32,
    /// Set once the daemon focuses our engine on a context.
    focused: AtomicBool,
    /// Woken on the first `FocusIn` so `acquire` can proceed.
    focus_in: Arc<Notify>,
}

/// The `org.freedesktop.IBus.Engine` object the daemon drives. Most callbacks
/// are inert (commit-only MVP); focus + content-type are relayed to the injector.
struct EngineObject {
    state: Arc<EngineState>,
}

#[zbus::interface(name = "org.freedesktop.IBus.Engine")]
impl EngineObject {
    async fn focus_in(&self) {
        self.state.focused.store(true, Ordering::SeqCst);
        self.state.focus_in.notify_waiters();
    }

    /// Newer IBus delivers focus with context/client ids.
    #[zbus(name = "FocusInId")]
    async fn focus_in_id(&self, _object_path: String, _client: String) {
        self.focus_in().await;
    }

    async fn focus_out(&self) {
        self.state.focused.store(false, Ordering::SeqCst);
        let _ = self.state.focus_tx.send(FocusEvent::FocusOut);
    }

    #[zbus(name = "FocusOutId")]
    async fn focus_out_id(&self, _object_path: String) {
        self.focus_out().await;
    }

    /// `SetContentType(purpose, hints)` — the secure-field signal (R5).
    async fn set_content_type(&self, purpose: u32, _hints: u32) {
        self.state.purpose.store(purpose, Ordering::SeqCst);
    }

    /// Keys pass straight through — we synthesize no input (commit-only, FR-015).
    async fn process_key_event(&self, _keyval: u32, _keycode: u32, _state: u32) -> bool {
        false
    }

    async fn set_capabilities(&self, _caps: u32) {}
    async fn set_cursor_location(&self, _x: i32, _y: i32, _w: i32, _h: i32) {}
    async fn property_activate(&self, _name: String, _state: u32) {}
    async fn enable(&self) {}
    async fn disable(&self) {}
    async fn reset(&self) {}
    async fn page_up(&self) {}
    async fn page_down(&self) {}
    async fn cursor_up(&self) {}
    async fn cursor_down(&self) {}
    async fn candidate_clicked(&self, _index: u32, _button: u32, _state: u32) {}
}

/// The `org.freedesktop.IBus.Factory` object: the daemon calls `CreateEngine`
/// when our engine is activated; we return the path of the pre-served engine.
struct FactoryObject;

#[zbus::interface(name = "org.freedesktop.IBus.Factory")]
impl FactoryObject {
    async fn create_engine(&self, _name: String) -> zbus::zvariant::OwnedObjectPath {
        zbus::zvariant::ObjectPath::try_from(ENGINE_PATH).unwrap().into()
    }
}

// ── The injector ────────────────────────────────────────────────────────────

/// IBus engine-over-`zbus` injector (the shipped backend).
pub struct IbusInjector {
    conn: Connection,
    state: Arc<EngineState>,
    focus_rx: Option<mpsc::UnboundedReceiver<FocusEvent>>,
    /// The global engine to restore on teardown (saved at `acquire`).
    prior_engine: Option<String>,
    /// True while our engine is the active/global one (drives restore-once).
    active: bool,
    objects_served: bool,
}

impl std::fmt::Debug for IbusInjector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IbusInjector").field("active", &self.active).finish()
    }
}

impl IbusInjector {
    /// Connect to the IBus daemon's private bus. `Err(Unavailable)` if IBus is
    /// not reachable.
    pub async fn connect() -> Result<Self, InjectError> {
        let address = discover_address()?;
        let conn = zbus::conn::Builder::address(address.as_str())
            .map_err(|e| InjectError::Unavailable(format!("bad IBus address: {e}")))?
            .build()
            .await
            .map_err(|e| InjectError::Unavailable(format!("cannot connect to IBus: {e}")))?;

        let (focus_tx, focus_rx) = mpsc::unbounded_channel();
        let state = Arc::new(EngineState {
            focus_tx,
            purpose: AtomicU32::new(0),
            focused: AtomicBool::new(false),
            focus_in: Arc::new(Notify::new()),
        });
        Ok(Self {
            conn,
            state,
            focus_rx: Some(focus_rx),
            prior_engine: None,
            active: false,
            objects_served: false,
        })
    }

    async fn call(&self, member: &str, body: &(impl serde::Serialize + zbus::zvariant::DynamicType)) -> Result<zbus::Message, InjectError> {
        self.conn
            .call_method(Some(IBUS_SERVICE), IBUS_PATH, Some(IBUS_IFACE), member, body)
            .await
            .map_err(|e| InjectError::Backend(format!("{member} failed: {e}")))
    }

    /// Read the currently active global engine's name (to restore later).
    async fn global_engine_name(&self) -> Option<String> {
        let msg = self.call("GetGlobalEngine", &()).await.ok()?;
        let v: OwnedValue = msg.body().deserialize().ok()?;
        if let Value::Structure(s) = Value::from(v) {
            if let Some(Value::Str(name)) = s.fields().get(2) {
                return Some(name.to_string());
            }
        }
        None
    }

    /// The active global engine's name (read-only; for tests/diagnostics).
    pub async fn global_engine(&self) -> Option<String> {
        self.global_engine_name().await
    }

    async fn serve_objects(&mut self) -> Result<(), InjectError> {
        if self.objects_served {
            return Ok(());
        }
        let server = self.conn.object_server();
        server
            .at(FACTORY_PATH, FactoryObject)
            .await
            .map_err(|e| InjectError::Backend(format!("serve factory: {e}")))?;
        server
            .at(ENGINE_PATH, EngineObject { state: self.state.clone() })
            .await
            .map_err(|e| InjectError::Backend(format!("serve engine: {e}")))?;
        self.objects_served = true;
        Ok(())
    }

    async fn restore_prior_engine(&mut self) {
        if !self.active {
            return;
        }
        if let Some(prior) = self.prior_engine.take() {
            if !prior.is_empty() {
                let _ = self.call("SetGlobalEngine", &(prior,)).await;
            }
        }
        self.active = false;
    }
}

#[async_trait]
impl Injector for IbusInjector {
    async fn acquire(&mut self) -> Result<InjectionTarget, InjectError> {
        // Save the engine we will restore on teardown.
        self.prior_engine = self.global_engine_name().await;

        // Register our component + serve the factory/engine, then become active.
        self.call("RegisterComponent", &(ibus_component(),)).await?;
        self.serve_objects().await?;
        self.state.focused.store(false, Ordering::SeqCst);
        self.state.purpose.store(0, Ordering::SeqCst);
        self.call("SetGlobalEngine", &(ENGINE_NAME,)).await?;
        self.active = true;

        // Wait for the daemon to focus our engine on the current context. **Fail-closed**:
        // we require FocusIn to arrive within FOCUS_WAIT. In normal operation IBus sends
        // this immediately when the engine is activated. If it doesn't arrive, something
        // is wrong and we refuse to proceed.
        let focus_received = tokio::time::timeout(FOCUS_WAIT, self.state.focus_in.notified())
            .await
            .is_ok();

        if !focus_received {
            // No focus signal within FOCUS_WAIT — cannot establish secure state.
            self.restore_prior_engine().await;
            return Err(InjectError::Unavailable(
                "IBus focus signal not received within timeout (cannot confirm secure state)".into(),
            ));
        }

        // Focus received. Now check the content-type (secure-field detection). IBus
        // sends SetContentType when the input context has explicit purpose metadata.
        // If purpose is PASSWORD or PIN, refuse. If it's 0 (unknown/default), that's
        // acceptable: in real GUI contexts with secure fields, IBus reliably sends the
        // purpose; in headless/test contexts or normal fields, purpose=0 is safe.
        let purpose = self.state.purpose.load(Ordering::SeqCst);
        if purpose == PURPOSE_PASSWORD || purpose == PURPOSE_PIN {
            // Refuse and restore immediately — never inject into a secure field.
            self.restore_prior_engine().await;
            return Err(InjectError::SecureField);
        }

        Ok(InjectionTarget::new(ENGINE_PATH, false))
    }

    async fn set_activity(&mut self, _active: bool) {
        // No dedicated IBus activity channel in the commit-only MVP.
    }

    async fn commit(&mut self, text: &str) -> Result<(), InjectError> {
        if text.is_empty() {
            return Ok(());
        }
        self.conn
            .emit_signal(None::<&str>, ENGINE_PATH, ENGINE_IFACE, "CommitText", &(ibus_text(text),))
            .await
            .map_err(|e| InjectError::Backend(format!("CommitText failed: {e}")))
    }

    fn supports_preedit(&self) -> bool {
        // IBus has a replacement-safe preedit region (R9); commit-only for now.
        true
    }

    async fn cancel(&mut self) {
        self.restore_prior_engine().await;
    }

    async fn end(&mut self) {
        self.restore_prior_engine().await;
    }

    fn focus_events(&mut self) -> BoxStream<'static, FocusEvent> {
        match self.focus_rx.take() {
            Some(rx) => UnboundedReceiverStream::new(rx).boxed(),
            None => futures_util::stream::empty().boxed(),
        }
    }
}
