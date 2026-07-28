//! `IbusInjector` — the shipped IBus text-injection backend (plan T22, T018).
//!
//! Speaks the IBus wire protocol (D-Bus / GVariant) directly over `zbus`
//! (research R1): no FFI, no GObject-introspection, no subprocess. It registers
//! an IBus component + engine, is made the active (global) engine per session,
//! commits committed segments via the engine's `CommitText` signal, and restores
//! the prior engine on session end. Focus and secure-field state arrive through
//! the engine's `FocusIn`/`FocusOut`/`SetContentType` callbacks (R4/R5).
//!
//! Commit-only by default; with the controller's opt-in `--preedit` (R9), the
//! volatile streaming hypothesis is rendered via `UpdatePreeditText` (underlined,
//! replaced on each update, cleared by `commit`/`HidePreeditText`) — never
//! committed, and withheld from known-secure fields exactly like `commit`.
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
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream::{BoxStream, StreamExt};
use tokio::sync::broadcast;
use tokio::sync::Notify;
use tokio_stream::wrappers::BroadcastStream;
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

/// `IBusPreeditFocusMode::CLEAR`: the preedit is discarded on focus-out (never
/// implicitly committed) — the only safe mode for volatile dictation text.
/// The daemon parses the engine's `UpdatePreeditText` signal strictly as
/// `(vubu)` (ibus 1.5.34, bus/engineproxy.c): a 3-arg `(vub)` emission fails
/// `g_variant_get` there and is dropped *silently* — the engine MUST send the
/// mode. (Root-caused 2026-07-28: commits landed but preedit never rendered.)
const PREEDIT_MODE_CLEAR: u32 = 0;

/// Whether an IBus input-purpose is a secure field we refuse to inject into.
fn is_secure_purpose(purpose: u32) -> bool {
    purpose == PURPOSE_PASSWORD || purpose == PURPOSE_PIN
}

/// How long `acquire` waits for the daemon to focus our engine before checking
/// the content-type. Secure fields (PASSWORD/PIN) reliably drive `FocusIn`
/// immediately, so this window catches them; a slow/absent `FocusIn` is the
/// normal-field case and we proceed (a hard-fail here breaks legitimate
/// dictation into fields IBus focuses differently).
const FOCUS_WAIT: Duration = Duration::from_millis(400);

/// After `FocusIn`, how long to let `SetContentType` settle before reading the
/// purpose. IBus delivers the content-type in the same burst as focus for a
/// secure field, so this short grace closes the FocusIn/SetContentType race
/// without adding perceptible latency (the security-relevant window).
const CONTENT_TYPE_GRACE: Duration = Duration::from_millis(50);

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

/// Wrap a serialized field in a variant (`v`): IBusText's attribute list is a
/// variant-wrapped `IBusAttrList`, not an inline structure. (The inline shape
/// `(sa{sv}s(sa{sv}av))` is *tolerated* by the daemon's CommitText path but
/// its UpdatePreeditText handler fails to deserialize it and forwards an
/// empty preedit with visible=false — root-caused 2026-07-28 by diffing our
/// signal bytes against libibus-serialized canonical bytes.)
fn variant_wrap(v: Value<'static>) -> Value<'static> {
    Value::Value(Box::new(v))
}

/// `IBusText` → `(sa{sv}sv)`: the committed string with an empty attribute list.
fn ibus_text(text: &str) -> Value<'static> {
    Value::from(
        StructureBuilder::new()
            .add_field("IBusText".to_string())
            .add_field(empty_attach())
            .add_field(text.to_string())
            .append_field(variant_wrap(ibus_attr_list())) // `v`
            .build()
            .expect("IBusText structure"),
    )
}

// IBusAttrType / IBusAttrUnderline constants for the preedit attribute.
const ATTR_TYPE_UNDERLINE: u32 = 1;
const ATTR_UNDERLINE_SINGLE: u32 = 1;

/// `IBusAttrList` carrying one underline attribute spanning `[0, end)` — the
/// conventional "this text is volatile" marker for a preedit region (R9).
fn ibus_preedit_attr_list(end: u32) -> Value<'static> {
    let underline = Value::from(
        StructureBuilder::new()
            .add_field("IBusAttribute".to_string())
            .add_field(empty_attach())
            .add_field(ATTR_TYPE_UNDERLINE)
            .add_field(ATTR_UNDERLINE_SINGLE)
            .add_field(0u32) // start index (chars)
            .add_field(end) // end index (chars)
            .build()
            .expect("IBusAttribute structure"),
    );
    Value::from(
        StructureBuilder::new()
            .add_field("IBusAttrList".to_string())
            .add_field(empty_attach())
            .add_field(vec![underline]) // attributes (av)
            .build()
            .expect("IBusAttrList structure"),
    )
}

/// `IBusText` for preedit: the volatile hypothesis, underlined over its whole
/// length so the field renders it as uncommitted (the IME-convention visual).
fn ibus_preedit_text(text: &str) -> Value<'static> {
    let chars = text.chars().count() as u32;
    Value::from(
        StructureBuilder::new()
            .add_field("IBusText".to_string())
            .add_field(empty_attach())
            .add_field(text.to_string())
            .append_field(variant_wrap(ibus_preedit_attr_list(chars))) // `v`
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

/// The invoking user's real home, read from `/etc/passwd` (world-readable,
/// including under snap confinement). Needed because snapd redirects `$HOME`
/// (and the gnome runtime redirects `XDG_CONFIG_HOME`) into `~/snap/<name>/…`,
/// while the IBus daemon writes its address file under the *real* home.
fn real_home_from_passwd(user: &str, passwd: &str) -> Option<PathBuf> {
    passwd.lines().find_map(|line| {
        let mut fields = line.split(':');
        if fields.next()? == user {
            // name:passwd:uid:gid:gecos:**home**:shell
            fields.nth(4).filter(|h| !h.is_empty()).map(PathBuf::from)
        } else {
            None
        }
    })
}

/// Candidate `ibus/bus` directories, best first. Ordinarily this is just
/// `$XDG_CONFIG_HOME/ibus/bus` / `~/.config/ibus/bus`; under snap confinement
/// (`$SNAP` set) the real user's config dir is appended, since the snap-private
/// `$HOME` never contains the daemon's address file.
fn candidate_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut push_unique = |d: PathBuf| {
        if !dirs.contains(&d) {
            dirs.push(d);
        }
    };
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            push_unique(PathBuf::from(xdg).join("ibus/bus"));
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            push_unique(PathBuf::from(home).join(".config/ibus/bus"));
        }
    }
    if std::env::var_os("SNAP").is_some() {
        if let Some(real) = std::env::var("USER")
            .ok()
            .and_then(|u| real_home_from_passwd(&u, &std::fs::read_to_string("/etc/passwd").unwrap_or_default()))
        {
            push_unique(real.join(".config/ibus/bus"));
        }
    }
    dirs
}

/// Locate the IBus private-bus address: `$IBUS_ADDRESS`, else the socket file
/// under `~/.config/ibus/bus/` (the file the daemon writes — every candidate
/// dir from [`candidate_dirs`] is searched). We pick the entry matching the
/// current display, and **validated** against liveness so a stale address file
/// (e.g. left by a crashed/replaced daemon) yields an actionable error rather
/// than a bare "connection refused".
fn discover_address() -> Result<String, InjectError> {
    if let Ok(addr) = std::env::var("IBUS_ADDRESS") {
        if !addr.is_empty() {
            return Ok(addr);
        }
    }
    let dirs = candidate_dirs();
    let first = dirs.first().cloned().unwrap_or_else(|| PathBuf::from("~/.config/ibus/bus"));
    let mut files: Vec<PathBuf> = Vec::new();
    for dir in &dirs {
        if let Ok(entries) = std::fs::read_dir(dir) {
            files.extend(entries.filter_map(|e| e.ok().map(|e| e.path())));
        }
    }
    if files.is_empty() {
        return Err(InjectError::Unavailable(format!(
            "no IBus socket dir {} (is an IBus daemon running? try `ibus restart`)",
            first.display()
        )));
    }

    // Prefer a file whose name ends with the current Wayland/X display.
    let want = std::env::var("WAYLAND_DISPLAY")
        .map(|w| format!("unix-{w}"))
        .or_else(|_| std::env::var("DISPLAY").map(|d| format!("unix{}", d.replace(':', "-"))))
        .ok();

    pick_address(files, want.as_deref(), &first)
}

/// Rank the candidate address files (display match first, then newest) and
/// take the first whose daemon is alive and whose socket exists.
/// `searched` names the primary dir for error messages.
fn pick_address(
    mut files: Vec<PathBuf>,
    want: Option<&str>,
    searched: &Path,
) -> Result<String, InjectError> {
    // Newest last, so the display match (if any) or the newest wins.
    files.sort_by_key(|p| p.metadata().and_then(|m| m.modified()).ok());
    // Rank display matches ahead of the rest, newest first within each group.
    let ranked: Vec<&PathBuf> = {
        let matches_display = |p: &&PathBuf| {
            want.is_some_and(|w| {
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
            searched.display()
        )),
    })
}

// ── Engine + Factory D-Bus objects ──────────────────────────────────────────

/// Shared engine state: the daemon's `FocusIn`/`FocusOut`/`SetContentType`
/// callbacks land on the object; this state relays them to the injector.
struct EngineState {
    /// Focus-loss events are **broadcast**: every utterance subscribes its own
    /// receiver, so focus-loss safety holds for session N, not just the first
    /// (a single-consumer channel silently disabled it after utterance 1).
    focus_tx: broadcast::Sender<FocusEvent>,
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
        myna_core::dbg_log!("inject", "IBus FocusIn received");
        self.state.focus_in.notify_waiters();
    }

    /// Newer IBus delivers focus with context/client ids.
    #[zbus(name = "FocusInId")]
    async fn focus_in_id(&self, _object_path: String, _client: String) {
        self.focus_in().await;
    }

    async fn focus_out(&self) {
        self.state.focused.store(false, Ordering::SeqCst);
        myna_core::dbg_log!("inject", "IBus FocusOut received");
        let _ = self.state.focus_tx.send(FocusEvent::FocusOut);
    }

    #[zbus(name = "FocusOutId")]
    async fn focus_out_id(&self, _object_path: String) {
        self.focus_out().await;
    }

    /// `SetContentType(purpose, hints)` — the secure-field signal (R5).
    /// Metadata only (never field content), so safe to debug-log.
    async fn set_content_type(&self, purpose: u32, hints: u32) {
        myna_core::dbg_log!(
            "inject",
            "IBus SetContentType: purpose={purpose} hints={hints}{}",
            if is_secure_purpose(purpose) { " (SECURE)" } else { "" }
        );
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
    /// The global engine to restore on teardown (saved at `acquire`).
    prior_engine: Option<String>,
    /// True while our engine is the active/global one (drives restore-once).
    active: bool,
    objects_served: bool,
    /// True while a preedit region is showing in the target (so `commit` and
    /// teardown clear it exactly when needed, never emitting redundant
    /// `HidePreeditText` signals).
    preedit_active: bool,
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

        // Capacity is generous for a 2-event vocabulary; a lagging session is
        // treated as focus-lost (fail-safe) by `focus_events`.
        let (focus_tx, _) = broadcast::channel(16);
        let state = Arc::new(EngineState {
            focus_tx,
            purpose: AtomicU32::new(0),
            focused: AtomicBool::new(false),
            focus_in: Arc::new(Notify::new()),
        });
        Ok(Self {
            conn,
            state,
            prior_engine: None,
            active: false,
            objects_served: false,
            preedit_active: false,
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

    /// Emit `HidePreeditText` if a preedit region is up. Best-effort: a
    /// failed hide at teardown must not mask the engine restore.
    async fn hide_preedit(&mut self) {
        if !self.preedit_active {
            return;
        }
        self.preedit_active = false;
        let _ = self
            .conn
            .emit_signal(None::<&str>, ENGINE_PATH, ENGINE_IFACE, "HidePreeditText", &())
            .await;
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

        // Wait for the daemon to focus our engine on the current context, then read
        // the content-type (secure-field check, R5). The security model: a secure
        // field (PASSWORD/PIN) reliably drives `FocusIn` immediately followed by
        // `SetContentType` in the same burst. So we wait for `FocusIn` (bounded by
        // FOCUS_WAIT), then give `SetContentType` a short grace to settle before
        // reading `purpose` — this closes the FocusIn→SetContentType race that a
        // bare read would lose. We do NOT hard-fail on a slow/absent `FocusIn`:
        // that is the ordinary-field case (IBus focuses different widgets on
        // different schedules), and refusing there breaks legitimate dictation.
        let focus_received = tokio::time::timeout(FOCUS_WAIT, self.state.focus_in.notified())
            .await
            .is_ok();
        if focus_received {
            // Focus arrived — let SetContentType land so a password field can't slip
            // through on the race between the two callbacks.
            tokio::time::sleep(CONTENT_TYPE_GRACE).await;
        }

        let purpose = self.state.purpose.load(Ordering::SeqCst);
        if is_secure_purpose(purpose) {
            // Refuse and restore immediately — never inject into a secure field.
            myna_core::dbg_log!("inject", "acquire refused: secure field (purpose={purpose})");
            self.restore_prior_engine().await;
            return Err(InjectError::SecureField);
        }

        myna_core::dbg_log!(
            "inject",
            "acquire ok: focus_received={focus_received} purpose={purpose}"
        );
        Ok(InjectionTarget::new(ENGINE_PATH, false))
    }

    async fn set_activity(&mut self, _active: bool) {
        // No dedicated IBus activity channel in the commit-only MVP.
    }

    async fn commit(&mut self, text: &str) -> Result<(), InjectError> {
        // A commit clears the preedit region (contract injector.md): the
        // volatile tail is superseded by stable text.
        self.hide_preedit().await;
        if text.is_empty() {
            return Ok(());
        }
        // Commit-time secure-field re-check (F2 hardening): `SetContentType` can
        // arrive *after* `acquire` returned — late delivery on the
        // Wayland/text-input-v3 path, or a mid-session focus change into a
        // secure field. `acquire`'s check alone can't cover that window, so
        // re-read the latest purpose here and never commit into a known-secure
        // field (I5, FR-021).
        let purpose = self.state.purpose.load(Ordering::SeqCst);
        if is_secure_purpose(purpose) {
            myna_core::dbg_log!("inject", "commit REFUSED: secure field (purpose={purpose})");
            return Err(InjectError::SecureField);
        }
        self.conn
            .emit_signal(None::<&str>, ENGINE_PATH, ENGINE_IFACE, "CommitText", &(ibus_text(text),))
            .await
            .map_err(|e| InjectError::Backend(format!("CommitText failed: {e}")))
    }

    async fn set_preedit(&mut self, text: &str) {
        // Same guard as `commit` (F2/I5): never render even volatile text into
        // a known-secure field — preedit is still text in the target.
        let purpose = self.state.purpose.load(Ordering::SeqCst);
        if is_secure_purpose(purpose) {
            myna_core::dbg_log!("inject", "preedit REFUSED: secure field (purpose={purpose})");
            return;
        }
        if text.is_empty() {
            self.hide_preedit().await;
            return;
        }
        // `UpdatePreeditText(IBusText, cursor_pos, visible, mode)` — the
        // region is *replaced* on each update (replacement-safe, R9), so
        // successive unstable hypotheses never accumulate. Cursor at the end
        // (chars). Mode is PREEDIT_CLEAR: focus-out must discard the volatile
        // text, never commit it.
        let cursor = text.chars().count() as u32;
        match self
            .conn
            .emit_signal(
                None::<&str>,
                ENGINE_PATH,
                ENGINE_IFACE,
                "UpdatePreeditText",
                &(ibus_preedit_text(text), cursor, true, PREEDIT_MODE_CLEAR),
            )
            .await
        {
            Ok(()) => self.preedit_active = true,
            Err(e) => myna_core::dbg_log!("inject", "UpdatePreeditText failed: {e}"),
        }
    }

    fn supports_preedit(&self) -> bool {
        // IBus has a replacement-safe preedit region (R9). Whether it is *used*
        // is the controller's call (opt-in `--preedit`); commit-only otherwise.
        true
    }

    async fn cancel(&mut self) {
        self.hide_preedit().await;
        self.restore_prior_engine().await;
    }

    async fn end(&mut self) {
        self.hide_preedit().await;
        self.restore_prior_engine().await;
    }

    fn focus_events(&mut self) -> BoxStream<'static, FocusEvent> {
        // Fresh subscription per utterance. If a session ever lags the
        // broadcast (missed focus events), fail safe: synthesize a FocusOut so
        // the controller finalizes instead of committing across a possible
        // focus boundary (FR-014/FR-022).
        BroadcastStream::new(self.state.focus_tx.subscribe())
            .map(|r| r.unwrap_or(FocusEvent::FocusOut))
            .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// R9: the preedit `IBusText` carries one underline attribute spanning the
    /// whole string (the volatile-text marker), and the cursor/end index counts
    /// **chars**, not bytes (IBus indexes are character-based).
    #[test]
    fn preedit_text_is_fully_underlined_and_char_indexed() {
        let v = ibus_preedit_text("héllo w");
        let Value::Structure(s) = &v else { panic!("IBusText must be a structure") };
        assert_eq!(s.fields()[0], Value::from("IBusText"));
        assert_eq!(s.fields()[2], Value::from("héllo w"));
        // The attribute list is a variant (`v`) wrapping the IBusAttrList
        // structure — not an inline structure (daemon compatibility).
        let Value::Value(attrs_box) = &s.fields()[3] else { panic!("attrs must be variant-wrapped") };
        let Value::Structure(attrs) = &**attrs_box else { panic!("attrs structure") };
        assert_eq!(attrs.fields()[0], Value::from("IBusAttrList"));
        let Value::Array(list) = &attrs.fields()[2] else { panic!("attr array") };
        assert_eq!(list.len(), 1, "exactly one (underline) attribute");
        // `av` elements are variant-wrapped.
        let Value::Value(inner) = &list[0] else { panic!("attr variant") };
        let Value::Structure(attr) = &**inner else { panic!("attr structure") };
        assert_eq!(attr.fields()[0], Value::from("IBusAttribute"));
        assert_eq!(attr.fields()[2], Value::from(ATTR_TYPE_UNDERLINE));
        assert_eq!(attr.fields()[3], Value::from(ATTR_UNDERLINE_SINGLE));
        assert_eq!(attr.fields()[4], Value::from(0u32));
        // "héllo w" is 7 chars but 8 bytes — the span must be 7.
        assert_eq!(attr.fields()[5], Value::from(7u32));
    }

    /// I5/FR-021: PASSWORD and PIN purposes are the secure set, checked both at
    /// `acquire` and at `commit` (late SetContentType delivery).
    #[test]
    fn secure_purpose_classification() {
        assert!(is_secure_purpose(PURPOSE_PASSWORD));
        assert!(is_secure_purpose(PURPOSE_PIN));
        // 0 (unknown/default) and ordinary purposes must not refuse.
        for purpose in [0, 1, 2, 3, 4, 5, 6, 7, 10, 15, 255] {
            assert!(!is_secure_purpose(purpose), "purpose {purpose} must be injectable");
        }
    }

    /// Snap confinement (feature 005): the real home is recovered from
    /// /etc/passwd, since snapd redirects $HOME into ~/snap/<name>/.
    #[test]
    fn real_home_parsed_from_passwd() {
        let passwd = "root:x:0:0:root:/root:/bin/bash\n\
                      charles:x:1000:1000:Charles,,,:/home/charles:/bin/bash\n";
        assert_eq!(
            real_home_from_passwd("charles", passwd),
            Some(PathBuf::from("/home/charles"))
        );
        assert_eq!(real_home_from_passwd("root", passwd), Some(PathBuf::from("/root")));
        assert_eq!(real_home_from_passwd("nobody-here", passwd), None);
        // Tolerates blank/garbage lines.
        assert_eq!(real_home_from_passwd("charles", "\ngarbage\n"), None);
    }

    /// Write a fake IBus address file (+ its socket path, so liveness passes)
    /// into `dir`; returns the file path. Uses *this* test process's PID so
    /// the daemon-alive check holds.
    fn fake_address_file(dir: &Path, name: &str, addr_suffix: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let sock = dir.join(format!("sock-{addr_suffix}"));
        std::fs::write(&sock, []).unwrap();
        let file = dir.join(name);
        std::fs::write(
            &file,
            format!(
                "IBUS_ADDRESS=unix:path={}\nIBUS_DAEMON_PID={}\n",
                sock.display(),
                std::process::id()
            ),
        )
        .unwrap();
        file
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("myna-ibus-test-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The daemon's address file is found in ANY candidate dir (feature 005 —
    /// under confinement the first dirs are snap-private and empty).
    #[test]
    fn address_found_in_later_candidate_dir() {
        let snap_private = temp_dir("snap");
        let real_home = temp_dir("real");
        let file = fake_address_file(&real_home, "abc-unix-wayland-0", "real");

        // Searching only the (empty) snap-private dir yields nothing…
        let files: Vec<PathBuf> = std::fs::read_dir(&snap_private)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        assert!(pick_address(files, Some("unix-wayland-0"), &snap_private).is_err());

        // …but with the real-home dir's entries included, the address is found.
        let files: Vec<PathBuf> = std::fs::read_dir(&real_home)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        let addr = pick_address(files, Some("unix-wayland-0"), &snap_private).unwrap();
        assert!(addr.starts_with("unix:path="), "{addr}");
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    /// A stale address file (dead daemon PID) is reported as stale, not used.
    #[test]
    fn stale_daemon_is_not_picked() {
        let dir = temp_dir("stale");
        std::fs::write(
            dir.join("abc-unix-wayland-0"),
            format!("IBUS_ADDRESS=unix:path={}/gone\nIBUS_DAEMON_PID=2\n", dir.display()),
        )
        .unwrap();
        // PID 2 is not this process but is alive on Linux — use a PID that
        // cannot exist instead.
        std::fs::write(
            dir.join("def-unix-wayland-0"),
            format!("IBUS_ADDRESS=unix:path={}/gone\nIBUS_DAEMON_PID=4194303\n", dir.display()),
        )
        .unwrap();
        let files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        let err = pick_address(files, Some("unix-wayland-0"), &dir).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("stale") || msg.contains("no usable"), "{msg}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}


