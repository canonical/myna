//! `IbusInjector` — the shipped IBus text-injection backend (plan T22, T018).
//!
//! Speaks the IBus wire protocol (D-Bus / GVariant) directly over `zbus`
//! (research R1): no FFI, no GObject-introspection, no subprocess. It registers
//! an IBus component + engine, is made the active (global) engine per session,
//! commits committed segments via the engine's `CommitText` signal, and restores
//! the prior engine on release. Focus arrives through the engine's
//! `FocusInId`/`FocusOutId` methods, which bind each utterance's lease to one
//! input context, and secure-field state through its write-only `ContentType`
//! property (R4/R5).
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
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};
use tokio::sync::watch;
use zbus::address::transport::{Transport, Unix, UnixSocket};
use zbus::zvariant::{OwnedValue, StructureBuilder, Value};
use zbus::{Address, Connection};

use super::{
    Activation, FocusEvent, InjectError, Injector, Support, Target, TextInputCapabilities,
};

const IBUS_SERVICE: &str = "org.freedesktop.IBus";
const IBUS_PATH: &str = "/org/freedesktop/IBus";
const IBUS_IFACE: &str = "org.freedesktop.IBus";
const ENGINE_IFACE: &str = "org.freedesktop.IBus.Engine";

const COMPONENT_NAME: &str = "org.freedesktop.IBus.Myna";
const ENGINE_NAME: &str = "myna-stt";
const FACTORY_PATH: &str = "/org/freedesktop/IBus/Factory";
const ENGINE_PATH: &str = "/org/freedesktop/IBus/Engine/Myna";

/// The client string ibus-daemon gives its own placeholder input context, the
/// one it focuses after every `FocusOut` (1.5.34). The path it holds looks
/// like any other context's, so the client is what names it.
const FAKE_CLIENT: &str = "fake";

/// How far apart a `FocusOut` and an activation may be to belong together. A
/// global key grab on X11 sends the focused window FocusOut on the key's
/// press, and the key's command reaches the daemon after it: 5-9 ms through
/// gdbus, about 80 ms through `myna.toggle` (Xubuntu noble). A `FocusOut`
/// with no activation this close is a loss, reported once the window is over.
const ACTIVATION_WINDOW: Duration = Duration::from_millis(500);

/// How long focus may stay off the field after a `FocusOut` that came with an
/// activation, or after the last activation since: the grab ends, and
/// FocusIn returns, when the key is released, and a held key repeats its
/// activation until then.
const FOCUS_BLIP_GRACE: Duration = Duration::from_millis(1000);

/// `IBusInputPurpose` values we refuse to inject into.
const PURPOSE_PASSWORD: u32 = 8;
const PURPOSE_PIN: u32 = 9;
/// `IBusInputHints::HIDDEN_TEXT`.
const HINT_HIDDEN_TEXT: u32 = 1 << 12;

/// `IBusPreeditFocusMode::CLEAR`: the preedit is discarded on focus-out (never
/// implicitly committed) — the only safe mode for volatile dictation text.
/// The daemon parses the engine's `UpdatePreeditText` signal strictly as
/// `(vubu)` (ibus 1.5.34, bus/engineproxy.c): a 3-arg `(vub)` emission fails
/// `g_variant_get` there and is dropped *silently* — the engine MUST send the
/// mode. (Root-caused 2026-07-28: commits landed but preedit never rendered.)
const PREEDIT_MODE_CLEAR: u32 = 0;

/// A field's `(purpose, hints)` as the daemon writes it to the engine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ContentType {
    purpose: u32,
    hints: u32,
}

impl ContentType {
    /// Whether this is a secure field we refuse to inject into.
    fn is_secure(self) -> bool {
        // GNOME Shell forwards no PIN purpose; PRIVATE alone is not refused.
        self.purpose == PURPOSE_PASSWORD
            || self.purpose == PURPOSE_PIN
            || self.hints & HINT_HIDDEN_TEXT != 0
    }
}

/// How long `acquire` waits for the daemon to focus our engine before checking
/// the content type. A slow or absent `FocusIn` is the ordinary-field case and
/// we proceed: a hard-fail here breaks dictation into fields IBus focuses
/// differently. The lease then cannot name its context, so the next focus call
/// ends it.
const FOCUS_WAIT: Duration = Duration::from_millis(400);

/// After `FocusIn`, how long to let the `ContentType` write land. The daemon
/// writes it after `FocusIn` to a newly activated engine, asynchronously.
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

// IBusAttrType / IBusAttrPreedit constants for the preedit attribute.
//
// GNOME Shell requests semantic preedit hints and deliberately ignores visual
// attributes such as UNDERLINE. WHOLE lets each client render composing text
// appropriately; traditional IBus clients commonly map it to an underline.
const ATTR_TYPE_HINT: u32 = 4;
const ATTR_PREEDIT_WHOLE: u32 = 1;

/// `IBusAttrList` carrying one whole-preedit hint spanning `[0, end)`.
fn ibus_preedit_attr_list(end: u32) -> Value<'static> {
    let whole = Value::from(
        StructureBuilder::new()
            .add_field("IBusAttribute".to_string())
            .add_field(empty_attach())
            .add_field(ATTR_TYPE_HINT)
            .add_field(ATTR_PREEDIT_WHOLE)
            .add_field(0u32) // start index (chars)
            .add_field(end) // end index (chars)
            .build()
            .expect("IBusAttribute structure"),
    );
    Value::from(
        StructureBuilder::new()
            .add_field("IBusAttrList".to_string())
            .add_field(empty_attach())
            .add_field(vec![whole]) // attributes (av)
            .build()
            .expect("IBusAttrList structure"),
    )
}

/// `IBusText` for preedit: the volatile hypothesis marked as composing over its
/// whole length, leaving the client to choose the visual treatment.
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
    for f in [
        ENGINE_NAME,
        "myna dictation",
        "myna speech-to-text",
        "en",
        "AGPL-3.0-or-later",
        "myna",
        "",
        "us",
    ] {
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
    for f in [
        COMPONENT_NAME,
        "myna dictation",
        "1.0",
        "AGPL-3.0-or-later",
        "myna",
        "",
        "",
        "",
    ] {
        b = b.add_field(f.to_string());
    }
    b = b.add_field(Vec::<Value>::new()); // observed paths (av)
    b = b.add_field(vec![ibus_engine_desc()]); // engines (av)
    Value::from(b.build().expect("IBusComponent structure"))
}

// ── IBus address discovery ──────────────────────────────────────────────────

/// Candidate `ibus/bus` directories, best first. Ordinarily this is just
/// `$XDG_CONFIG_HOME/ibus/bus` / `~/.config/ibus/bus`; under snap confinement
/// the real home's (`$SNAP_REAL_HOME`) config dir is appended, since the
/// snap-private `$HOME` never contains the daemon's address file.
fn candidate_dirs(env: &dyn Fn(&str) -> Option<String>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut push_unique = |d: PathBuf| {
        if !dirs.contains(&d) {
            dirs.push(d);
        }
    };
    if let Some(xdg) = env("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            push_unique(PathBuf::from(xdg).join("ibus/bus"));
        }
    }
    if let Some(home) = env("HOME") {
        if !home.is_empty() {
            push_unique(PathBuf::from(home).join(".config/ibus/bus"));
        }
    }
    if let Some(real) = env("SNAP_REAL_HOME") {
        if !real.is_empty() {
            push_unique(PathBuf::from(real).join(".config/ibus/bus"));
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
fn discover_address() -> Result<Address, InjectError> {
    to_zbus_address(&discover_address_in(&|k| std::env::var(k).ok())?)
}

fn discover_address_in(env: &dyn Fn(&str) -> Option<String>) -> Result<String, InjectError> {
    if let Some(addr) = env("IBUS_ADDRESS") {
        if !addr.is_empty() {
            return Ok(addr);
        }
    }
    let dirs = candidate_dirs(env);
    let first = dirs
        .first()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("~/.config/ibus/bus"));
    let mut files: Vec<PathBuf> = Vec::new();
    for dir in &dirs {
        if let Ok(entries) = std::fs::read_dir(dir) {
            files.extend(entries.filter_map(|e| e.ok().map(|e| e.path())));
        }
    }
    if files.is_empty() {
        let searched: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
        return Err(InjectError::Unavailable(format!(
            "no IBus address file in {} (is an IBus daemon running? try `ibus restart`)",
            searched.join(", ")
        )));
    }

    // Prefer a file whose name ends with the current Wayland/X display.
    let want = env("WAYLAND_DISPLAY")
        .map(|w| format!("unix-{w}"))
        .or_else(|| env("DISPLAY").map(|d| format!("unix{}", d.replace(':', "-"))));

    pick_address(files, want.as_deref(), &first)
}

/// Decode a D-Bus address value into a filesystem path.
///
/// The D-Bus address grammar percent-encodes every byte outside
/// `[-0-9A-Za-z_/.\]`, so an `@` in the user's home arrives as `%40`:
/// `unix:path=/home/first.last%40canonical.com/.cache/ibus/dbus-E7P10tya`.
/// `zbus` unescapes when it connects, so the socket is reachable; a literal
/// `Path::exists` on the raw value is not, and answered "missing socket" for
/// every account whose home is not plain ASCII-alphanumeric - every AD login
/// (root-caused 2026-09-04 on a `didier.roche@canonical.com` home, where the
/// daemon was alive and the address file fresh).
fn address_path(value: &str) -> PathBuf {
    let raw = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        match value
            .get(i + 1..i + 3)
            .filter(|_| raw[i] == b'%')
            .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            Some(byte) => {
                out.push(byte);
                i += 3;
            }
            None => {
                out.push(raw[i]);
                i += 1;
            }
        }
    }
    PathBuf::from(std::ffi::OsString::from_vec(out))
}

/// Parse a D-Bus address for `zbus`, with the unix socket path percent-decoded.
///
/// `zbus` 5.18 parses `path=` with a plain `PathBuf::from` (`address/transport/
/// unix.rs:40`), so the `%40` ibus writes for an `@` in the home reaches
/// `connect(2)` verbatim and fails with `ENOENT`. Decoding into the typed
/// [`Address`] - rather than rewriting the address string - keeps a path that
/// legitimately contains `,` or `;` from being re-parsed as address options.
fn to_zbus_address(raw: &str) -> Result<Address, InjectError> {
    let parsed = Address::try_from(raw)
        .map_err(|e| InjectError::Unavailable(format!("bad IBus address {raw}: {e}")))?;
    let Transport::Unix(unix) = parsed.transport() else {
        return Ok(parsed);
    };
    let UnixSocket::File(path) = unix.path() else {
        return Ok(parsed);
    };
    let decoded = address_path(&path.to_string_lossy());
    if decoded == *path {
        return Ok(parsed);
    }
    let rebuilt = Address::new(Transport::Unix(Unix::new(UnixSocket::File(decoded))));
    match parsed.guid() {
        Some(guid) => rebuilt
            .set_guid(guid.to_owned())
            .map_err(|e| InjectError::Unavailable(format!("bad IBus address guid: {e}"))),
        None => Ok(rebuilt),
    }
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
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with(w))
            })
        };
        let (mut hit, miss): (Vec<&PathBuf>, Vec<&PathBuf>) =
            files.iter().rev().partition(matches_display);
        hit.extend(miss);
        hit
    };

    // Walk candidates best-first; take the first whose daemon is alive and whose
    // socket exists. Remember why the *best* one was rejected: a dead PID and a
    // socket that is not there have different causes and different fixes, so the
    // message names the check that actually failed and the file it failed on.
    let tried = ranked.len();
    let mut stale: Option<(String, String)> = None;
    for path in ranked {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let Some(addr) = text.lines().find_map(|l| l.strip_prefix("IBUS_ADDRESS=")) else {
            continue;
        };
        let addr = addr.to_string();
        let pid: Option<i64> = text
            .lines()
            .find_map(|l| l.strip_prefix("IBUS_DAEMON_PID="))
            .and_then(|p| p.trim().parse().ok());
        // The daemon PID is alive (Linux /proc) and the unix socket path exists?
        // Absent either field, that check has nothing to say and passes.
        let dead_pid = pid.filter(|p| !PathBuf::from(format!("/proc/{p}")).exists());
        let gone_sock = addr
            .split("path=")
            .nth(1)
            .and_then(|s| s.split(',').next())
            .filter(|sp| !address_path(sp).exists())
            .map(str::to_owned);
        if dead_pid.is_none() && gone_sock.is_none() {
            return Ok(addr);
        }
        let why = match (dead_pid, gone_sock) {
            (Some(p), Some(s)) => format!("PID {p} is gone and its socket {s} is missing"),
            (Some(p), None) => format!("PID {p} is gone (no /proc/{p})"),
            (None, Some(s)) => format!("its socket {s} is missing"),
            (None, None) => unreachable!("both checks passed above"),
        };
        let name = path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy()
            .into_owned();
        stale.get_or_insert((name, why));
    }

    Err(match stale {
        Some((name, why)) => InjectError::Unavailable(format!(
            "no IBus daemon answers: tried {tried} address file(s), best is {name} where {why}. \
             Is IBus running in this session? Try `ibus restart` (or set IBUS_ADDRESS)."
        )),
        None => InjectError::Unavailable(format!(
            "no usable IBus address in {} (is an IBus daemon running? try `ibus restart`)",
            searched.display()
        )),
    })
}

// ── Engine + Factory D-Bus objects ──────────────────────────────────────────

/// The input context an identified focus call names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Context<'a> {
    path: &'a str,
    client: &'a str,
}

impl Context<'_> {
    /// Whether this is the daemon's own placeholder context, which no
    /// application ever writes into.
    fn is_fake(self) -> bool {
        self.client == FAKE_CLIENT
    }
}

/// What one focus call from the daemon says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusChange<'a> {
    /// `FocusInId(path, client)`, or plain `FocusIn` (`None`).
    In(Option<Context<'a>>),
    /// `FocusOutId(path)`, or plain `FocusOut` (`None`).
    Out(Option<&'a str>),
}

/// The input context a lease may write to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Binding {
    /// Minted before the engine switch; no focus yet. The previous release's
    /// restore may still be delivering focus calls, which are not this
    /// lease's (see [`Lease::focus`]).
    Pending,
    /// Focused through plain `FocusIn`, as a daemon that has not read
    /// `FocusId` yet does. ibus 1.5.34 re-sends that focus as `FocusInId`
    /// with no `FocusOut` between, which names the context; 1.5.29 names
    /// nothing until focus moves.
    Unnamed,
    /// An unnamed focus turned out to be the daemon's fake context, or left
    /// before anything was written, as the fake context does when a key grab
    /// ends. The next field focused within [`FOCUS_BLIP_GRACE`] binds the
    /// lease, as it would a pending one; writes are held until then.
    Left { blip: u64 },
    /// Focused on this input context.
    Context(String),
    /// Focus left `path`. Unless an activation comes within
    /// [`ACTIVATION_WINDOW`] (`activated`) and focus is back on `path` within
    /// [`FOCUS_BLIP_GRACE`] of the `FocusOut` or of the last activation, a
    /// held key's repeats included (`back`), it is a loss. Writes are held until the
    /// activation and wait after it. `blip` tells this absence from a later
    /// one.
    Away {
        path: String,
        blip: u64,
        activated: bool,
        back: bool,
    },
    /// No focus arrived while acquiring, so any focus call ends the lease.
    Unfocused,
    /// Focus left, a newer lease was minted, or the target was released.
    Lost,
}

/// What ending a lease found, which is what a releasing target knows about
/// its own standing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Retired {
    /// Still held: this target owned the field until now.
    Held,
    /// Focus had already left it.
    Lost,
    /// A newer lease replaced it; that lease's target owns the engine now.
    Superseded,
}

impl Retired {
    /// Whether this target hands back the displaced input method. A superseded
    /// one must not: the lease that replaced it carries that responsibility
    /// now, so switching here takes the engine from a live utterance.
    fn restores(self) -> bool {
        self != Retired::Superseded
    }
}

/// One utterance's right to write, as the daemon's focus calls shape it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Lease {
    id: u64,
    binding: Binding,
    /// The character before the cursor in the focused field, as its latest
    /// surrounding text says. `None` until the field sends one, and at its
    /// start.
    before_cursor: Option<char>,
    /// When the user last used Myna's activation while this lease was held.
    activated_at: Option<tokio::time::Instant>,
    /// Pending, with the daemon's fake context focused: a key grab holds focus
    /// off every field until the key is released.
    on_fake: bool,
    /// Text or a preedit went to the daemon under this lease.
    wrote: bool,
}

/// Where a target stands for its next write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Standing {
    /// Write now.
    Held,
    /// Focus is away with no activation yet: hold the write.
    Unsure,
    /// The right to write is gone.
    Lost,
}

impl Lease {
    fn held_by(&self, id: u64) -> bool {
        self.id == id && self.binding != Binding::Lost
    }

    /// Only focus staying on, first naming, or within a blip coming back to
    /// the bound context keeps it.
    ///
    /// A `Pending` lease ignores the two calls our own restore delivers - a
    /// `FocusOut`, then focus on the daemon's fake context - because nothing
    /// orders the object server's dispatch of them against the next `mint`.
    /// Neither can name this lease's field: the daemon sends no `FocusOut`
    /// before the first `FocusIn` on a newly activated engine, and the fake
    /// context is never a field. Once focused, focus anywhere else ends the
    /// lease, and a `FocusOut` sends it `Away`, where the fake context passes
    /// the same way.
    fn focus(&mut self, change: FocusChange<'_>, blip: u64) {
        let recent = self
            .activated_at
            .is_some_and(|at| at.elapsed() <= ACTIVATION_WINDOW);
        let next = match (&mut self.binding, change) {
            (Binding::Pending, FocusChange::Out(_)) => {
                self.on_fake = false;
                return;
            }
            (Binding::Pending, FocusChange::In(Some(ctx))) if ctx.is_fake() => {
                self.on_fake = true;
                return;
            }
            (Binding::Away { back, .. }, FocusChange::Out(_)) => {
                *back = false;
                return;
            }
            (Binding::Away { .. }, FocusChange::In(Some(ctx))) if ctx.is_fake() => return,
            (Binding::Pending, FocusChange::In(None)) => Binding::Unnamed,
            (Binding::Unnamed, FocusChange::Out(_)) if !self.wrote => Binding::Left { blip },
            // Written to, so a field: named now, it leaves as a named one does.
            (Binding::Unnamed, FocusChange::Out(Some(path))) => Binding::Away {
                path: path.to_owned(),
                blip,
                activated: recent,
                back: false,
            },
            (Binding::Unnamed, FocusChange::In(Some(ctx))) if ctx.is_fake() && !self.wrote => {
                Binding::Left { blip }
            }
            (Binding::Left { .. }, FocusChange::Out(_)) => return,
            (Binding::Left { .. }, FocusChange::In(Some(ctx))) if ctx.is_fake() => return,
            (Binding::Left { .. }, FocusChange::In(None)) => Binding::Unnamed,
            (
                Binding::Pending | Binding::Unnamed | Binding::Left { .. },
                FocusChange::In(Some(ctx)),
            ) if !ctx.is_fake() => Binding::Context(ctx.path.to_owned()),
            (Binding::Context(path), FocusChange::In(Some(ctx))) if *path == ctx.path => return,
            (Binding::Context(path), FocusChange::Out(_)) => Binding::Away {
                path: std::mem::take(path),
                blip,
                activated: recent,
                back: false,
            },
            (
                Binding::Away {
                    path,
                    activated,
                    back,
                    ..
                },
                FocusChange::In(Some(ctx)),
            ) if *path == ctx.path => {
                if !*activated {
                    *back = true;
                    return;
                }
                Binding::Context(std::mem::take(path))
            }
            _ => Binding::Lost,
        };
        self.binding = next;
    }

    /// The user used Myna's activation: a blip under way is one.
    fn activated(&mut self) {
        self.activated_at = Some(tokio::time::Instant::now());
        if let Binding::Away {
            path,
            activated,
            back,
            ..
        } = &mut self.binding
        {
            *activated = true;
            if *back {
                self.binding = Binding::Context(std::mem::take(path));
            }
        }
    }

    /// The blip this lease is away in, if it is. Blip ids are the engine's,
    /// so one names its lease too.
    fn away(&self) -> Option<u64> {
        match self.binding {
            Binding::Away { blip, .. } | Binding::Left { blip } => Some(blip),
            _ => None,
        }
    }

    fn standing(&self, id: u64) -> Option<Standing> {
        Some(match self.binding {
            _ if !self.held_by(id) => Standing::Lost,
            Binding::Away {
                activated: false, ..
            }
            | Binding::Left { .. } => Standing::Unsure,
            Binding::Away { .. } => return None,
            _ => Standing::Held,
        })
    }
}

/// Shared engine state: the daemon's focus calls and `ContentType` writes land
/// on the object; this state relays them to the injector and its targets.
struct EngineState {
    /// Latest `ContentType` the daemon wrote (default until one arrives).
    content_type: watch::Sender<ContentType>,
    /// The current lease. A `watch` retains it, so a wait or focus stream that
    /// starts late still observes a change: the daemon focuses the engine
    /// *during* the `SetGlobalEngine` round trip, before `acquire` waits.
    lease: watch::Sender<Lease>,
    /// Source of lease ids for this engine. Only targets born here consult it.
    next_lease: AtomicU64,
    /// Source of blip ids, so a grace ends only the absence it was set for.
    next_blip: AtomicU64,
    /// Where a blip's grace runs out: the daemon's focus calls arrive on
    /// zbus's executor, which has no timer of tokio's. Without one, focus
    /// leaving is a loss at once.
    runtime: Option<tokio::runtime::Handle>,
    /// The input method our activation displaced, held until some release
    /// hands it back. It is the connection's, not one utterance's: a target
    /// that supersedes another finds `myna-stt` global and must not take that
    /// for the user's engine.
    displaced: watch::Sender<Option<String>>,
}

impl EngineState {
    fn new() -> Self {
        Self {
            content_type: watch::Sender::new(ContentType::default()),
            lease: watch::Sender::new(Lease {
                id: 0,
                binding: Binding::Lost,
                before_cursor: None,
                activated_at: None,
                on_fake: false,
                wrote: false,
            }),
            next_lease: AtomicU64::new(1),
            next_blip: AtomicU64::new(1),
            runtime: tokio::runtime::Handle::try_current().ok(),
            displaced: watch::Sender::new(None),
        }
    }

    /// Mint the lease for the next target, ending any earlier one.
    fn mint(&self) -> u64 {
        let id = self.next_lease.fetch_add(1, Ordering::Relaxed);
        self.lease.send_replace(Lease {
            id,
            binding: Binding::Pending,
            before_cursor: None,
            activated_at: None,
            on_fake: false,
            wrote: false,
        });
        id
    }

    fn holds(&self, id: u64) -> bool {
        self.lease.borrow().held_by(id)
    }

    fn focus(self: &Arc<Self>, change: FocusChange<'_>) {
        let blip = self.next_blip.fetch_add(1, Ordering::Relaxed);
        let mut began = false;
        self.lease.send_modify(|lease| {
            lease.focus(change, blip);
            began = lease.away() == Some(blip);
        });
        if !began {
            return;
        }
        match &self.runtime {
            Some(runtime) => {
                let state = Arc::clone(self);
                runtime.spawn(async move {
                    let since = tokio::time::Instant::now();
                    tokio::time::sleep(ACTIVATION_WINDOW).await;
                    state.expire(blip, false);
                    while let Some(deadline) = state.blip_deadline(blip, since) {
                        if tokio::time::Instant::now() >= deadline {
                            state.expire(blip, true);
                            break;
                        }
                        tokio::time::sleep_until(deadline).await;
                    }
                });
            }
            None => self.expire(blip, true),
        }
    }

    /// When focus away in `blip` since `since` becomes a loss: the grace from
    /// then or from the last activation, whichever is later, so a held key's
    /// repeats keep it open. `None` once the blip is over.
    fn blip_deadline(
        &self,
        blip: u64,
        since: tokio::time::Instant,
    ) -> Option<tokio::time::Instant> {
        let lease = self.lease.borrow();
        (lease.away() == Some(blip)).then(|| {
            let last = lease.activated_at.map_or(since, |at| at.max(since));
            last + FOCUS_BLIP_GRACE
        })
    }

    /// End the lease if it is still away in `blip`, and, short of the grace,
    /// `even_activated`.
    fn expire(&self, blip: u64, even_activated: bool) {
        self.lease.send_if_modified(|lease| {
            let expired = lease.away() == Some(blip)
                && (even_activated
                    || matches!(
                        lease.binding,
                        Binding::Away {
                            activated: false,
                            ..
                        }
                    ));
            if expired {
                lease.binding = Binding::Lost;
            }
            expired
        });
    }

    /// Lease `id` put text or a preedit in front of the daemon.
    fn wrote(&self, id: u64) {
        self.lease.send_if_modified(|lease| {
            if lease.held_by(id) {
                lease.wrote = true;
            }
            false
        });
    }

    /// The user's activation continues, whichever lease is current.
    fn activated_current(&self) {
        self.lease.send_if_modified(|lease| {
            let held = lease.binding != Binding::Lost;
            if held {
                lease.activated();
            }
            held
        });
    }

    /// The user used Myna's activation while lease `id` is held.
    fn activated(&self, id: u64) {
        self.lease.send_if_modified(|lease| {
            let held = lease.held_by(id);
            if held {
                lease.activated();
            }
            held
        });
    }

    /// Where lease `id` stands for a write, waiting out a blip that came with
    /// an activation; with `sure`, any blip.
    async fn standing(&self, id: u64, sure: bool) -> Standing {
        let mut rx = self.lease.subscribe();
        let mut standing = Standing::Lost;
        let _ = rx
            .wait_for(|lease| match lease.standing(id) {
                Some(Standing::Unsure) if sure => false,
                Some(now) => {
                    standing = now;
                    true
                }
                None => false,
            })
            .await;
        standing
    }

    /// Record the focused field's surrounding text, reduced at once to the
    /// one character the spacing needs. Ignored unless a lease is focused:
    /// anything earlier belongs to a field no lease writes to.
    fn surrounding(&self, before_cursor: Option<char>) {
        self.lease.send_if_modified(|lease| {
            if matches!(lease.binding, Binding::Unnamed | Binding::Context(_)) {
                lease.before_cursor = before_cursor;
            }
            false
        });
    }

    /// The character before the cursor, while lease `id` is held.
    fn before_cursor(&self, id: u64) -> Option<char> {
        let lease = self.lease.borrow();
        lease.before_cursor.filter(|_| lease.held_by(id))
    }

    /// End lease `id`, saying what ending it found.
    fn retire(&self, id: u64) -> Retired {
        let mut retired = Retired::Superseded;
        self.lease.send_if_modified(|lease| {
            if lease.id != id {
                return false;
            }
            let held = lease.binding != Binding::Lost;
            retired = if held { Retired::Held } else { Retired::Lost };
            lease.binding = Binding::Lost;
            held
        });
        retired
    }

    /// Record the engine an activation displaces, `current` being what
    /// `GetGlobalEngine` named just before the switch. Ours means an earlier
    /// activation is still standing: what it displaced is what the user is
    /// owed, so it is kept. `myna-stt` is never something to restore.
    fn displace(&self, current: Option<String>) {
        self.displaced.send_if_modified(|displaced| match current {
            Some(name) if !name.is_empty() && name != ENGINE_NAME => {
                *displaced = Some(name);
                true
            }
            _ => false,
        });
    }

    /// The engine a retiring target hands back, consuming the responsibility
    /// so no later release switches again. A superseded target takes nothing:
    /// the lease that displaced it in turn owes the restore.
    fn reclaim(&self, retired: Retired) -> Option<String> {
        if !retired.restores() {
            return None;
        }
        self.displaced.send_replace(None)
    }

    /// Whether the daemon focused the engine for lease `id`, waiting up to
    /// `FOCUS_WAIT`, or on the fake context up to `FOCUS_BLIP_GRACE` from the
    /// start or the last activation. If it did not, the lease becomes
    /// `Unfocused`.
    async fn focus_arrived(&self, id: u64) -> bool {
        let mut rx = self.lease.subscribe();
        let arrived = |l: &Lease| l.id != id || l.binding != Binding::Pending;
        let started = tokio::time::Instant::now();
        let _ = tokio::time::timeout(FOCUS_WAIT, rx.wait_for(arrived)).await;
        // A key grab, as starting by key makes on X11, keeps the field's focus
        // until the key is released, and a held key repeats until then.
        while self.on_fake(id) {
            let last = self
                .lease
                .borrow()
                .activated_at
                .map_or(started, |at| at.max(started));
            let deadline = last + FOCUS_BLIP_GRACE;
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            let _ = tokio::time::timeout_at(deadline, rx.wait_for(arrived)).await;
        }
        !self.lease.send_if_modified(|lease| {
            let pending = lease.id == id && lease.binding == Binding::Pending;
            if pending {
                lease.binding = Binding::Unfocused;
            }
            pending
        })
    }

    /// Whether lease `id` never got past the daemon's fake context.
    fn on_fake(&self, id: u64) -> bool {
        let lease = self.lease.borrow();
        lease.id == id
            && matches!(lease.binding, Binding::Pending | Binding::Unfocused)
            && lease.on_fake
    }

    /// Yields `FocusOut` once lease `id` is no longer held.
    fn loss(&self, id: u64) -> BoxStream<'static, FocusEvent> {
        let mut rx = self.lease.subscribe();
        stream::once(async move {
            let _ = rx.wait_for(|l| !l.held_by(id)).await;
            FocusEvent::FocusOut
        })
        .boxed()
    }

    fn content_type(&self) -> ContentType {
        *self.content_type.borrow()
    }
}

/// What [`IbusInjector::activation`] hands out: it marks whatever lease is
/// current, the one being acquired included.
fn activation_for(state: &Arc<EngineState>) -> Activation {
    let state = Arc::clone(state);
    Activation::new(move || state.activated_current())
}

/// The `org.freedesktop.IBus.Engine` object the daemon drives. Most callbacks
/// are inert (commit-only MVP); focus and content type are relayed to the
/// injector. Not spawned per call, so focus calls apply in the order the daemon
/// sent them.
struct EngineObject {
    state: Arc<EngineState>,
}

#[zbus::interface(name = "org.freedesktop.IBus.Engine", spawn = false)]
impl EngineObject {
    async fn focus_in(&self) {
        myna_core::dbg_log!("inject", "IBus FocusIn received");
        self.state.focus(FocusChange::In(None));
    }

    #[zbus(name = "FocusInId")]
    async fn focus_in_id(&self, object_path: String, client: String) {
        myna_core::dbg_log!("inject", "IBus FocusInId {object_path} ({client})");
        self.state.focus(FocusChange::In(Some(Context {
            path: &object_path,
            client: &client,
        })));
    }

    async fn focus_out(&self) {
        myna_core::dbg_log!("inject", "IBus FocusOut received");
        self.state.focus(FocusChange::Out(None));
    }

    #[zbus(name = "FocusOutId")]
    async fn focus_out_id(&self, object_path: String) {
        myna_core::dbg_log!("inject", "IBus FocusOutId {object_path}");
        self.state.focus(FocusChange::Out(Some(&object_path)));
    }

    /// Read-only, as in `ibus-engine-simple`: asks the daemon to name the input
    /// context in `FocusInId`/`FocusOutId`.
    #[zbus(property(emits_changed_signal = "const"))]
    async fn focus_id(&self) -> bool {
        true
    }

    /// Read-only. True makes the daemon ask the field for its surrounding
    /// text on every focus, which separates one dictation from the text
    /// already before the cursor. The daemon re-sends the first focus as
    /// `FocusInId` only once it has read this.
    #[zbus(property(emits_changed_signal = "const"))]
    async fn active_surrounding_text(&self) -> bool {
        true
    }

    /// The field's text around the cursor. Only the character before the
    /// cursor (or the selection the next commit replaces) is kept, and the
    /// text is never logged.
    async fn set_surrounding_text(&self, text: OwnedValue, cursor_pos: u32, anchor_pos: u32) {
        let before = char_before(&text, cursor_pos.min(anchor_pos));
        self.state.surrounding(before);
    }

    /// Write-only, as in `ibus-engine-simple`: the daemon only ever
    /// `Properties.Set`s it, and discards the error. Metadata only, so safe to
    /// debug-log.
    #[zbus(property)]
    async fn set_content_type(&self, value: (u32, u32)) {
        let content_type = ContentType {
            purpose: value.0,
            hints: value.1,
        };
        myna_core::dbg_log!(
            "inject",
            "IBus ContentType: {content_type:?}{}",
            if content_type.is_secure() {
                " (SECURE)"
            } else {
                ""
            }
        );
        self.state.content_type.send_replace(content_type);
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

/// The character before char offset `pos` of a serialized `IBusText`.
fn char_before(text: &Value<'_>, pos: u32) -> Option<char> {
    let Value::Structure(text) = text else {
        return None;
    };
    let Some(Value::Str(text)) = text.fields().get(2) else {
        return None;
    };
    let pos = usize::try_from(pos).ok()?.checked_sub(1)?;
    text.chars().nth(pos)
}

/// The `org.freedesktop.IBus.Factory` object: the daemon calls `CreateEngine`
/// when our engine is activated; we return the path of the pre-served engine.
struct FactoryObject;

#[zbus::interface(name = "org.freedesktop.IBus.Factory")]
impl FactoryObject {
    async fn create_engine(&self, _name: String) -> zbus::zvariant::OwnedObjectPath {
        zbus::zvariant::ObjectPath::try_from(ENGINE_PATH)
            .unwrap()
            .into()
    }
}

// ── The injector ────────────────────────────────────────────────────────────

/// Map a failed IBus call to the [`InjectError`] arm that says the right thing
/// about the *connection*. A transport failure (the socket is closed: IBus
/// restarted under us, which every input-source change and GNOME Shell
/// replace does) is `Unavailable`, so `LazyInjector` drops this connection
/// and opens a fresh one. Anything the daemon answered - an unknown engine,
/// a refused component - says nothing about the socket and stays `Backend`.
fn classify(member: &str, e: zbus::Error) -> InjectError {
    match e {
        zbus::Error::InputOutput(_) => {
            InjectError::Unavailable(format!("{member} failed: {e} (IBus connection lost)"))
        }
        e => InjectError::Backend(format!("{member} failed: {e}")),
    }
}

async fn call(
    conn: &Connection,
    member: &str,
    body: &(impl serde::Serialize + zbus::zvariant::DynamicType),
) -> Result<zbus::Message, InjectError> {
    conn.call_method(
        Some(IBUS_SERVICE),
        IBUS_PATH,
        Some(IBUS_IFACE),
        member,
        body,
    )
    .await
    .map_err(|e| classify(member, e))
}

/// IBus engine-over-`zbus` injector (the shipped backend).
pub struct IbusInjector {
    conn: Connection,
    state: Arc<EngineState>,
    objects_served: bool,
}

impl IbusInjector {
    /// Connect to the IBus daemon's private bus. `Err(Unavailable)` if IBus is
    /// not reachable.
    pub async fn connect() -> Result<Self, InjectError> {
        Self::connect_to(discover_address()?).await
    }

    /// Connect to the IBus daemon at `address`, as `connect` does to the
    /// session's.
    pub async fn connect_to(address: Address) -> Result<Self, InjectError> {
        let conn = zbus::conn::Builder::address(address)
            .map_err(|e| InjectError::Unavailable(format!("bad IBus address: {e}")))?
            .build()
            .await
            .map_err(|e| InjectError::Unavailable(format!("cannot connect to IBus: {e}")))?;
        Ok(Self {
            conn,
            state: Arc::new(EngineState::new()),
            objects_served: false,
        })
    }

    /// Read the currently active global engine's name (to restore later).
    async fn global_engine_name(&self) -> Option<String> {
        let msg = call(&self.conn, "GetGlobalEngine", &()).await.ok()?;
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
            .at(
                ENGINE_PATH,
                EngineObject {
                    state: self.state.clone(),
                },
            )
            .await
            .map_err(|e| InjectError::Backend(format!("serve engine: {e}")))?;
        self.objects_served = true;
        Ok(())
    }
}

/// IBus has a replacement-safe preedit region (R9); whether it is used is the
/// controller's call. The daemon asks every field for its surrounding text
/// (`ActiveSurroundingText`) and delivers its content type (`ContentType`).
pub const CAPABILITIES: TextInputCapabilities = TextInputCapabilities {
    preedit: true,
    surrounding_text: true,
    secure_field_detection: Support::Supported,
};

#[async_trait]
impl Injector for IbusInjector {
    /// Acquiring switches the global engine to ours. The rollback on error
    /// can only restore an input method the connection was able to read.
    async fn acquire(&mut self) -> Result<Box<dyn Target>, InjectError> {
        // Read before the switch; recorded once it has succeeded.
        let displaced = self.global_engine_name().await;

        // Register our component + serve the factory/engine, then become active.
        call(&self.conn, "RegisterComponent", &(ibus_component(),)).await?;
        self.serve_objects().await?;
        self.state.content_type.send_replace(ContentType::default());
        // Before the switch, which focuses the engine before it returns.
        let lease = self.state.mint();
        if let Err(err) = call(&self.conn, "SetGlobalEngine", &(ENGINE_NAME,)).await {
            self.state.retire(lease);
            return Err(err);
        }
        self.state.displace(displaced);
        let target = Box::new(IbusTarget {
            conn: self.conn.clone(),
            state: self.state.clone(),
            lease,
            preedit_active: false,
            held: Vec::new(),
            held_preedit: None,
        });

        // The daemon writes ContentType after FocusIn to a newly activated
        // engine, so wait for focus and give the write a grace to land.
        let focus_received = self.state.focus_arrived(lease).await;
        if focus_received {
            tokio::time::sleep(CONTENT_TYPE_GRACE).await;
        }

        if self.state.on_fake(lease) {
            myna_core::dbg_log!("inject", "acquire refused: no field took focus back");
            target.release().await;
            return Err(InjectError::NoTarget);
        }
        if !self.state.holds(lease) {
            myna_core::dbg_log!("inject", "acquire refused: focus moved while acquiring");
            target.release().await;
            return Err(InjectError::FocusLost);
        }
        let content_type = self.state.content_type();
        if content_type.is_secure() {
            myna_core::dbg_log!("inject", "acquire refused: secure field {content_type:?}");
            target.release().await;
            return Err(InjectError::SecureField);
        }

        myna_core::dbg_log!(
            "inject",
            "acquire ok: focus_received={focus_received} {content_type:?}"
        );
        Ok(target)
    }

    fn capabilities(&self) -> TextInputCapabilities {
        CAPABILITIES
    }

    fn activation(&self) -> Activation {
        activation_for(&self.state)
    }
}

/// One utterance's hold on the engine: writes only while its lease is held,
/// and on release hands back the input method the connection displaced, unless
/// a newer lease superseded it.
struct IbusTarget {
    conn: Connection,
    state: Arc<EngineState>,
    lease: u64,
    /// True while a preedit region is showing in the target (so `commit` and
    /// release clear it exactly when needed, never emitting redundant
    /// `HidePreeditText` signals).
    preedit_active: bool,
    /// Commits made while focus was away with no activation yet, in order.
    held: Vec<String>,
    /// The preedit asked for then, shown once they land.
    held_preedit: Option<String>,
}

impl std::fmt::Debug for IbusTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IbusTarget")
            .field("lease", &self.lease)
            .finish_non_exhaustive()
    }
}

impl IbusTarget {
    async fn emit(
        &self,
        member: &str,
        body: &(impl serde::Serialize + zbus::zvariant::DynamicType),
    ) -> zbus::Result<()> {
        self.conn
            .emit_signal(None::<&str>, ENGINE_PATH, ENGINE_IFACE, member, body)
            .await
    }

    /// Emit `HidePreeditText` if a preedit region is up and still ours.
    /// Best-effort; focus-out already discards it (`PREEDIT_MODE_CLEAR`).
    async fn hide_preedit(&mut self) {
        if std::mem::take(&mut self.preedit_active) && self.state.holds(self.lease) {
            let _ = self.emit("HidePreeditText", &()).await;
        }
    }
}

impl IbusTarget {
    /// Where this target stands for a write, holding `text` while that is
    /// unsure. Emitted while focus is away, it would reach the fake context.
    async fn standing(&mut self, held: Option<&str>, preedit: bool) -> Standing {
        let standing = self.state.standing(self.lease, false).await;
        match standing {
            Standing::Unsure if preedit => self.held_preedit = held.map(str::to_owned),
            Standing::Unsure => {
                self.held.extend(held.map(str::to_owned));
                self.held_preedit = None;
            }
            Standing::Lost => {
                self.held.clear();
                self.held_preedit = None;
            }
            Standing::Held => {}
        }
        standing
    }

    /// Write what a blip held, now that focus is back.
    async fn flush(&mut self) -> Result<(), InjectError> {
        for text in std::mem::take(&mut self.held) {
            self.write(&text).await?;
        }
        if let Some(text) = self.held_preedit.take() {
            self.show_preedit(&text).await;
        }
        Ok(())
    }

    async fn write(&mut self, text: &str) -> Result<(), InjectError> {
        // A commit clears the preedit region (contract injector.md): the
        // volatile tail is superseded by stable text.
        self.hide_preedit().await;
        if !self.state.holds(self.lease) {
            myna_core::dbg_log!("inject", "commit REFUSED: focus lost");
            return Err(InjectError::FocusLost);
        }
        if text.is_empty() {
            return Ok(());
        }
        // The content type can change after `acquire` (I5, FR-021).
        let content_type = self.state.content_type();
        if content_type.is_secure() {
            myna_core::dbg_log!("inject", "commit REFUSED: secure field {content_type:?}");
            return Err(InjectError::SecureField);
        }
        self.state.wrote(self.lease);
        self.emit("CommitText", &(ibus_text(text),))
            .await
            .map_err(|e| classify("CommitText", e))
    }

    async fn show_preedit(&mut self, text: &str) {
        if !self.state.holds(self.lease) {
            myna_core::dbg_log!("inject", "preedit REFUSED: focus lost");
            return;
        }
        // Same guard as `commit` (F2/I5): never render even volatile text into
        // a known-secure field — preedit is still text in the target.
        let content_type = self.state.content_type();
        if content_type.is_secure() {
            myna_core::dbg_log!("inject", "preedit REFUSED: secure field {content_type:?}");
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
        self.state.wrote(self.lease);
        match self
            .emit(
                "UpdatePreeditText",
                &(ibus_preedit_text(text), cursor, true, PREEDIT_MODE_CLEAR),
            )
            .await
        {
            Ok(()) => self.preedit_active = true,
            Err(e) => myna_core::dbg_log!("inject", "UpdatePreeditText failed: {e}"),
        }
    }
}

#[async_trait]
impl Target for IbusTarget {
    async fn commit(&mut self, text: &str) -> Result<(), InjectError> {
        match self.standing(Some(text), false).await {
            Standing::Unsure => return Ok(()),
            Standing::Lost => {
                myna_core::dbg_log!("inject", "commit REFUSED: focus lost");
                return Err(InjectError::FocusLost);
            }
            Standing::Held => {}
        }
        self.flush().await?;
        self.write(text).await
    }

    async fn set_preedit(&mut self, text: &str) {
        match self.standing(Some(text), true).await {
            Standing::Held => {}
            Standing::Unsure | Standing::Lost => return,
        }
        if self.flush().await.is_ok() {
            self.show_preedit(text).await;
        }
    }

    fn activated(&self) {
        self.state.activated(self.lease);
    }

    fn char_before_cursor(&self) -> Option<char> {
        self.state.before_cursor(self.lease)
    }

    fn focus_events(&self) -> BoxStream<'static, FocusEvent> {
        self.state.loss(self.lease)
    }

    /// Clears the preedit and restores the input method the engine switch
    /// displaced, unless a newer lease superseded this one.
    async fn release(mut self: Box<Self>) {
        // A blip under way decides whether what it held lands.
        if self.state.standing(self.lease, true).await == Standing::Held {
            let _ = self.flush().await;
        }
        // Retired first: the restore focuses our engine out.
        let retired = self.state.retire(self.lease);
        if std::mem::take(&mut self.preedit_active) && retired == Retired::Held {
            let _ = self.emit("HidePreeditText", &()).await;
        }
        match self.state.reclaim(retired) {
            Some(displaced) => {
                let _ = call(&self.conn, "SetGlobalEngine", &(displaced,)).await;
            }
            None => myna_core::dbg_log!("inject", "release: nothing to restore ({retired:?})"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::FutureExt;

    /// The transport dying under a held connection (IBus restarted: an
    /// input-source change, `ibus restart`, a GNOME Shell replace, logout)
    /// must read as `Unavailable`, or `LazyInjector` keeps the corpse and
    /// every later press fails the same way.
    #[test]
    fn a_closed_socket_is_unavailable_not_a_backend_error() {
        let io = std::io::Error::from(std::io::ErrorKind::BrokenPipe);
        let err = classify("RegisterComponent", zbus::Error::InputOutput(Arc::new(io)));
        assert!(matches!(err, InjectError::Unavailable(_)), "{err:?}");
        assert!(err.to_string().contains("RegisterComponent"), "{err}");

        let err = classify(
            "SetGlobalEngine",
            zbus::Error::Failure("no such engine".into()),
        );
        assert!(matches!(err, InjectError::Backend(_)), "{err:?}");
    }

    /// R9: the preedit `IBusText` carries one semantic WHOLE hint spanning the
    /// string, and the cursor/end index counts **chars**, not bytes.
    #[test]
    fn preedit_text_has_whole_hint_and_char_indexed() {
        let v = ibus_preedit_text("héllo w");
        let Value::Structure(s) = &v else {
            panic!("IBusText must be a structure")
        };
        assert_eq!(s.fields()[0], Value::from("IBusText"));
        assert_eq!(s.fields()[2], Value::from("héllo w"));
        // The attribute list is a variant (`v`) wrapping the IBusAttrList
        // structure — not an inline structure (daemon compatibility).
        let Value::Value(attrs_box) = &s.fields()[3] else {
            panic!("attrs must be variant-wrapped")
        };
        let Value::Structure(attrs) = &**attrs_box else {
            panic!("attrs structure")
        };
        assert_eq!(attrs.fields()[0], Value::from("IBusAttrList"));
        let Value::Array(list) = &attrs.fields()[2] else {
            panic!("attr array")
        };
        assert_eq!(list.len(), 1, "exactly one (whole-preedit) attribute");
        // `av` elements are variant-wrapped.
        let Value::Value(inner) = &list[0] else {
            panic!("attr variant")
        };
        let Value::Structure(attr) = &**inner else {
            panic!("attr structure")
        };
        assert_eq!(attr.fields()[0], Value::from("IBusAttribute"));
        assert_eq!(attr.fields()[2], Value::from(ATTR_TYPE_HINT));
        assert_eq!(attr.fields()[3], Value::from(ATTR_PREEDIT_WHOLE));
        assert_eq!(attr.fields()[4], Value::from(0u32));
        // "héllo w" is 7 chars but 8 bytes — the span must be 7.
        assert_eq!(attr.fields()[5], Value::from(7u32));
    }

    fn engine_state() -> Arc<EngineState> {
        Arc::new(EngineState::new())
    }

    fn engine(state: &Arc<EngineState>) -> EngineObject {
        EngineObject {
            state: Arc::clone(state),
        }
    }

    const FIELD: &str = "/org/freedesktop/IBus/InputContext_7";
    const OTHER: &str = "/org/freedesktop/IBus/InputContext_8";
    /// The daemon's own placeholder context, which it focuses after every
    /// `FocusOut`. It is created before any application's, so it holds the
    /// first path; the client string is what names it (traced against
    /// ibus-daemon 1.5.34: every restore delivered
    /// `FocusInId('/org/freedesktop/IBus/InputContext_1', 'fake')`).
    const FAKE: &str = "/org/freedesktop/IBus/InputContext_1";

    /// The two calls our own `SetGlobalEngine(prior)` delivers on release.
    async fn restore_focus_calls(engine: &EngineObject) {
        engine.focus_out().await;
        engine.focus_in_id(FAKE.into(), "fake".into()).await;
    }

    /// The daemon delivers `FocusIn` *during* the `SetGlobalEngine` round trip
    /// that triggers it, so `acquire` only starts waiting after the event has
    /// already been dispatched. That must still count as focus received: with
    /// the earlier `Notify::notify_waiters` this notification was dropped on
    /// the floor (no task was parked yet), `focus_received` was false on every
    /// acquire, and the content-type grace never ran.
    #[tokio::test(start_paused = true)]
    async fn focus_in_delivered_before_the_wait_starts_is_not_lost() {
        let state = engine_state();
        let lease = state.mint();

        // Happens inside `SetGlobalEngine`, before anyone awaits.
        engine(&state).focus_in_id(FIELD.into(), "app".into()).await;

        let started = tokio::time::Instant::now();
        assert!(
            state.focus_arrived(lease).await,
            "FocusIn dispatched before the wait began must still be observed"
        );
        // Not just the right answer: focus already in hand must be seen at
        // once, or every utterance pays the whole grace before it starts.
        assert!(
            started.elapsed() < FOCUS_WAIT,
            "focus already in hand must not wait out FOCUS_WAIT"
        );
        assert!(state.holds(lease));
    }

    /// The flip side: with no `FocusIn` the wait times out rather than
    /// reporting focus, so `acquire` keeps treating a silent context as the
    /// ordinary-field case instead of claiming the grace period ran. The lease
    /// never learned its context, so the next focus call of any kind ends it.
    #[tokio::test(start_paused = true)]
    async fn no_focus_in_times_out_and_any_later_focus_ends_the_lease() {
        let state = engine_state();
        let lease = state.mint();
        let started = tokio::time::Instant::now();
        assert!(!state.focus_arrived(lease).await);
        // Given up only after the whole grace, never early: a field the daemon
        // is slow to focus is still an ordinary field.
        assert!(
            started.elapsed() >= FOCUS_WAIT,
            "a silent context must be given the whole grace before giving up"
        );
        assert!(state.holds(lease));

        engine(&state).focus_in_id(FIELD.into(), "app".into()).await;
        assert!(!state.holds(lease));
    }

    /// The daemon's first focus on a freshly activated engine arrives as plain
    /// `FocusIn`, before it has read `ActiveSurroundingText`. The method has to
    /// relay it: otherwise `acquire` waits out `FOCUS_WAIT` and treats a
    /// focused ordinary field as a silent one.
    #[tokio::test(start_paused = true)]
    async fn a_plain_focus_in_is_relayed_by_the_engine_object() {
        let state = engine_state();
        let lease = state.mint();
        engine(&state).focus_in().await;

        let started = tokio::time::Instant::now();
        assert!(
            state.focus_arrived(lease).await,
            "a plain FocusIn is still focus"
        );
        assert!(started.elapsed() < FOCUS_WAIT);
        assert!(state.holds(lease));
    }

    /// The property *values*, not just their declarations: `FocusId` is what
    /// makes ibus-daemon name the context in `FocusInId`/`FocusOutId`, and
    /// `ActiveSurroundingText` is what makes it re-send that first focus as
    /// `FocusInId`. The daemon discards error replies, so a wrong value here
    /// costs the lease its context and fails silently.
    #[tokio::test]
    async fn the_engine_asks_the_daemon_to_identify_focus() {
        let engine = engine(&engine_state());
        assert!(
            engine.focus_id().await,
            "FocusId=false leaves every focus unnamed"
        );
        assert!(
            engine.active_surrounding_text().await,
            "ActiveSurroundingText=false leaves dictations unseparated"
        );
    }

    /// A stale `FocusIn` from the previous utterance must not satisfy the next
    /// `acquire`: minting resets the binding before `SetGlobalEngine`.
    #[tokio::test(start_paused = true)]
    async fn focus_from_a_prior_session_does_not_carry_over() {
        let state = engine_state();
        let first = state.mint();
        engine(&state).focus_in_id(FIELD.into(), "app".into()).await;
        assert!(state.focus_arrived(first).await);

        let next = state.mint();
        assert!(!state.focus_arrived(next).await);
    }

    #[tokio::test]
    async fn refocusing_the_bound_context_keeps_the_lease() {
        let state = engine_state();
        let lease = state.mint();
        let engine = engine(&state);
        engine.focus_in_id(FIELD.into(), "app".into()).await;
        engine.focus_in_id(FIELD.into(), "app".into()).await;
        assert!(state.holds(lease));
    }

    /// Focus reaching another context, the daemon's fake one included, means
    /// it left ours; coming back does not restore the right to write.
    #[tokio::test]
    async fn focus_in_on_another_context_ends_the_lease() {
        for (path, client) in [(OTHER, "app"), (FAKE, "fake")] {
            let state = engine_state();
            let lease = state.mint();
            let engine = engine(&state);
            engine.focus_in_id(FIELD.into(), "app".into()).await;
            engine.focus_in_id(path.into(), client.into()).await;
            assert!(!state.holds(lease), "{client} took focus from the lease");

            engine.focus_in_id(FIELD.into(), "app".into()).await;
            assert!(!state.holds(lease), "a lost lease stays lost");
        }
    }

    /// The first activation after ibus-daemon starts, by a key on X11: the
    /// grab holds the fake context focused, and the daemon has not read
    /// `FocusId` yet, so it focuses the engine with a plain `FocusIn`. ibus
    /// 1.5.29 (noble) then names nothing until the key's release:
    /// `FocusOutId(fake)`, `FocusInId(field)` (traced on Xubuntu noble).
    #[tokio::test(start_paused = true)]
    async fn an_unnamed_focus_that_leaves_before_any_write_binds_the_next_field() {
        let state = engine_state();
        let lease = state.mint();
        let engine = engine(&state);
        engine.focus_in().await;
        assert!(state.focus_arrived(lease).await);

        engine.focus_out_id(FAKE.into()).await;
        assert_eq!(standing_now(&state, lease), Some(Standing::Unsure));
        engine.focus_in_id(FIELD.into(), APP.into()).await;
        assert!(state.holds(lease));
        assert_eq!(standing_now(&state, lease), Some(Standing::Held));

        // Bound to the field now: focus elsewhere ends it.
        engine.focus_out_id(FIELD.into()).await;
        engine.focus_in_id(OTHER.into(), APP.into()).await;
        assert!(!state.holds(lease));
    }

    /// ibus 1.5.34 re-sends the first focus as `FocusInId` once it has read
    /// both properties, which names the fake context; the field comes at the
    /// key's release.
    #[tokio::test(start_paused = true)]
    async fn an_unnamed_focus_named_the_fake_context_waits_for_the_field() {
        let state = engine_state();
        let lease = state.mint();
        let engine = engine(&state);
        engine.focus_in().await;
        engine.focus_in_id(FAKE.into(), "fake".into()).await;
        assert_eq!(standing_now(&state, lease), Some(Standing::Unsure));

        tokio::time::sleep(FOCUS_BLIP_GRACE - TICK).await;
        ungrab(&engine).await;
        assert!(state.holds(lease));
        assert_eq!(standing_now(&state, lease), Some(Standing::Held));
    }

    /// While the lease waits for a field, the fake context passes, and a
    /// focus the daemon still cannot name is the field's.
    #[tokio::test(start_paused = true)]
    async fn a_lease_waiting_after_an_unnamed_focus_skips_the_fake_context() {
        for named in [true, false] {
            let state = engine_state();
            let lease = state.mint();
            let engine = engine(&state);
            engine.focus_in().await;
            engine.focus_out().await;
            engine.focus_in_id(FAKE.into(), "fake".into()).await;
            engine.focus_out_id(FAKE.into()).await;
            if named {
                engine.focus_in_id(FIELD.into(), APP.into()).await;
            } else {
                engine.focus_in().await;
            }
            assert!(state.holds(lease), "named: {named}");
            assert_eq!(standing_now(&state, lease), Some(Standing::Held));
        }
    }

    /// The wait for the field is bounded like a key grab's.
    #[tokio::test(start_paused = true)]
    async fn no_field_after_an_unnamed_focus_left_is_a_loss() {
        for named_fake in [false, true] {
            let state = engine_state();
            let lease = state.mint();
            let engine = engine(&state);
            engine.focus_in().await;
            if named_fake {
                engine.focus_in_id(FAKE.into(), "fake".into()).await;
            } else {
                engine.focus_out_id(FAKE.into()).await;
            }
            tokio::time::sleep(FOCUS_BLIP_GRACE + TICK).await;
            assert!(!state.holds(lease), "named fake: {named_fake}");
            engine.focus_in_id(FIELD.into(), APP.into()).await;
            assert!(!state.holds(lease), "a lost lease stays lost");
        }
    }

    /// Once something was written, the unnamed focus was a field: focus
    /// leaving it with no activation, or turning out to be the fake context,
    /// ends the lease.
    #[tokio::test(start_paused = true)]
    async fn an_unnamed_focus_written_to_does_not_move() {
        for named_fake in [false, true] {
            let state = engine_state();
            let lease = state.mint();
            let engine = engine(&state);
            engine.focus_in().await;
            state.wrote(lease);
            if named_fake {
                engine.focus_in_id(FAKE.into(), "fake".into()).await;
            } else {
                engine.focus_out_id(FIELD.into()).await;
                engine.focus_in_id(OTHER.into(), APP.into()).await;
            }
            assert!(!state.holds(lease), "named fake: {named_fake}");
        }
    }

    /// With no activation around it, once the window is over.
    #[tokio::test(start_paused = true)]
    async fn focus_out_of_the_bound_context_ends_the_lease() {
        let state = engine_state();
        let lease = state.mint();
        let engine = engine(&state);
        engine.focus_in_id(FIELD.into(), "app".into()).await;
        engine.focus_out_id(FIELD.into()).await;
        tokio::time::sleep(ACTIVATION_WINDOW + TICK).await;
        assert!(!state.holds(lease));
    }

    /// A `FocusOut` before any focus arrived is never the user's: the daemon
    /// handles a focus change it sees before our engine is attached without
    /// telling the engine at all, and delivers no `FocusOut` during our own
    /// `SetGlobalEngine(myna-stt)` (traced, ibus 1.5.34). What does arrive
    /// there is the previous release's restore, so the pending lease ignores
    /// it and still binds the focus it is waiting for.
    #[tokio::test(start_paused = true)]
    async fn focus_out_while_pending_is_our_own_restore_and_is_ignored() {
        let state = engine_state();
        let lease = state.mint();
        let engine = engine(&state);
        engine.focus_out().await;
        assert!(state.holds(lease), "a pending lease survives it");

        engine.focus_in_id(FIELD.into(), "app".into()).await;
        assert!(state.holds(lease), "the real focus still binds the lease");
        // Plain `FocusOut`, which is what the daemon sends our engine: the
        // identified form has its own case below.
        engine.focus_out().await;
        tokio::time::sleep(ACTIVATION_WINDOW + TICK).await;
        assert!(
            !state.holds(lease),
            "once focused, a FocusOut ends the lease"
        );
    }

    /// Both halves of the restore, dispatched *after* the next press minted
    /// its lease - nothing orders the object server's dispatch against
    /// `mint`. Neither may be taken for this lease's own focus: the
    /// `FocusOut` would report a focus loss on a good press, and binding the
    /// fake context would kill the lease at the daemon's next call.
    #[tokio::test]
    async fn a_restore_dispatched_after_the_next_mint_leaves_that_lease_alone() {
        let state = engine_state();
        let engine = engine(&state);
        let first = state.mint();
        engine.focus_in_id(FIELD.into(), "app".into()).await;
        state.retire(first);

        let next = state.mint();
        restore_focus_calls(&engine).await;
        assert!(
            state.holds(next),
            "our own restore must not end a fresh lease"
        );

        engine.focus_in_id(OTHER.into(), "app".into()).await;
        engine.focus_in_id(OTHER.into(), "app".into()).await;
        assert!(
            state.holds(next),
            "the lease must have bound the real context, not the fake one"
        );
    }

    /// ibus-daemon reads `FocusId` asynchronously the first time it activates
    /// an engine name: that focus arrives plain, then again as `FocusInId`
    /// with no `FocusOut` between. The resend names the context once.
    #[tokio::test]
    async fn plain_focus_in_is_named_by_the_daemons_resend() {
        let state = engine_state();
        let lease = state.mint();
        let engine = engine(&state);
        engine.focus_in().await;
        engine.focus_in_id(FIELD.into(), "app".into()).await;
        assert!(state.holds(lease));

        engine.focus_in_id(OTHER.into(), "app".into()).await;
        assert!(!state.holds(lease));
    }

    /// A field focused unnamed and written to, stopped by key on X11: by then
    /// the daemon names contexts, so the grab's `FocusOutId` names the field,
    /// and the blip around the activation is ridden out as for a named one.
    #[tokio::test(start_paused = true)]
    async fn a_blip_on_an_unnamed_field_written_to_is_ridden_out() {
        let state = engine_state();
        let lease = state.mint();
        let engine = engine(&state);
        engine.focus_in().await;
        state.wrote(lease);

        grab(&engine).await;
        state.activated(lease);
        tokio::time::sleep(EDGE_AFTER).await;
        engine.focus_out_id(FAKE.into()).await;
        engine.focus_in_id(FIELD.into(), APP.into()).await;
        tokio::time::sleep(FOCUS_BLIP_GRACE).await;
        assert!(state.holds(lease));
        assert_eq!(standing_now(&state, lease), Some(Standing::Held));
    }

    /// A second plain focus is a focus change the daemon could not name, as
    /// is a plain `FocusOut` once something was written (unwritten, it may be
    /// the fake context's: see the unnamed focus cases above).
    #[tokio::test]
    async fn plain_focus_calls_after_the_first_end_the_lease() {
        for (second, wrote) in [
            (FocusChange::In(None), false),
            (FocusChange::Out(None), true),
        ] {
            let state = engine_state();
            let lease = state.mint();
            state.focus(FocusChange::In(None));
            if wrote {
                state.wrote(lease);
            }
            state.focus(second);
            assert!(!state.holds(lease), "{second:?}");
        }
    }

    #[tokio::test]
    async fn a_newer_lease_ends_the_older() {
        let state = engine_state();
        let older = state.mint();
        engine(&state).focus_in_id(FIELD.into(), "app".into()).await;
        let newer = state.mint();
        assert!(!state.holds(older));
        assert!(state.holds(newer));
    }

    /// Release retires its lease before restoring the prior engine, whose
    /// switch focuses our engine out and then focuses its fake context in;
    /// neither call may reach the lease the next acquire mints.
    #[tokio::test]
    async fn restore_does_not_trip_a_live_lease() {
        let state = engine_state();
        let engine = engine(&state);
        let first = state.mint();
        engine.focus_in_id(FIELD.into(), "app".into()).await;
        state.retire(first);
        restore_focus_calls(&engine).await;
        let next = state.mint();
        assert!(state.holds(next));
    }

    /// Release has to know why a lease ended: only a superseded target leaves
    /// the engine to the target that displaced it.
    #[tokio::test(start_paused = true)]
    async fn retire_says_whether_the_lease_was_held_lost_or_superseded() {
        let state = engine_state();
        let engine = engine(&state);

        let held = state.mint();
        engine.focus_in_id(FIELD.into(), "app".into()).await;
        assert_eq!(state.retire(held), Retired::Held);

        let lost = state.mint();
        engine.focus_in_id(FIELD.into(), "app".into()).await;
        engine.focus_out_id(FIELD.into()).await;
        tokio::time::sleep(ACTIVATION_WINDOW + TICK).await;
        assert_eq!(state.retire(lost), Retired::Lost);

        let older = state.mint();
        let newer = state.mint();
        assert_eq!(state.retire(older), Retired::Superseded);
        assert!(
            state.holds(newer),
            "retiring a superseded lease must not end the live one"
        );
    }

    #[test]
    fn only_a_superseded_target_leaves_the_engine_to_its_successor() {
        assert!(Retired::Held.restores());
        assert!(
            Retired::Lost.restores(),
            "focus left, but this target still displaced the engine"
        );
        assert!(!Retired::Superseded.restores());
    }

    const USER_ENGINE: &str = "xkb:us::eng";

    /// The user's input method is displaced once per connection, not once per
    /// utterance: the activation that supersedes a live lease reads `myna-stt`
    /// from `GetGlobalEngine`, and recording that would lose the user's engine
    /// for the rest of the session.
    #[test]
    fn a_superseding_activation_keeps_what_the_first_one_displaced() {
        let state = engine_state();
        state.displace(Some(USER_ENGINE.into()));
        state.displace(Some(ENGINE_NAME.into()));
        assert_eq!(
            state.reclaim(Retired::Held).as_deref(),
            Some(USER_ENGINE),
            "the second activation recorded our own engine over the user's"
        );
    }

    /// Restoring `myna-stt` restores nothing, so no path may record it, nor
    /// the empty name a daemon with no global engine answers with.
    #[test]
    fn our_own_engine_is_never_recorded_as_something_to_restore() {
        for current in [Some(ENGINE_NAME.to_owned()), Some(String::new()), None] {
            let state = engine_state();
            state.displace(current.clone());
            assert_eq!(state.reclaim(Retired::Held), None, "{current:?}");
        }
    }

    /// One displacement, one restore: the release that hands the engine back
    /// consumes the record, so a later release does not switch again.
    #[test]
    fn the_restoring_release_consumes_the_displaced_engine_once() {
        let state = engine_state();
        state.displace(Some(USER_ENGINE.into()));
        assert_eq!(state.reclaim(Retired::Lost).as_deref(), Some(USER_ENGINE));
        assert_eq!(state.reclaim(Retired::Held), None);
    }

    /// A superseded release restores nothing and keeps the record for the live
    /// target, which is the one still writing into the user's field.
    #[test]
    fn a_superseded_release_leaves_the_displaced_engine_for_the_live_target() {
        let state = engine_state();
        state.displace(Some(USER_ENGINE.into()));
        assert_eq!(state.reclaim(Retired::Superseded), None);
        assert_eq!(state.reclaim(Retired::Held).as_deref(), Some(USER_ENGINE));
    }

    const APP: &str = "gtk4-im:mousepad";
    /// When the key's command reaches the daemon after its grab's FocusOut.
    const EDGE_AFTER: Duration = Duration::from_millis(20);
    const TICK: Duration = Duration::from_millis(1);

    /// A lease focused on `FIELD`.
    async fn focused() -> (Arc<EngineState>, EngineObject, u64) {
        let state = engine_state();
        let lease = state.mint();
        let engine = engine(&state);
        engine.focus_in_id(FIELD.into(), APP.into()).await;
        (state, engine, lease)
    }

    /// The field out, then the daemon's fake context in (traced, 1.5.34).
    async fn grab(engine: &EngineObject) {
        engine.focus_out_id(FIELD.into()).await;
        engine.focus_in_id(FAKE.into(), "fake".into()).await;
    }

    /// The fake context out, the field in: the key's release.
    async fn ungrab(engine: &EngineObject) {
        engine.focus_out_id(FAKE.into()).await;
        engine.focus_in_id(FIELD.into(), APP.into()).await;
    }

    fn standing_now(state: &EngineState, lease: u64) -> Option<Standing> {
        state.lease.borrow().standing(lease)
    }

    /// How often a held key repeats on Xubuntu.
    const REPEAT: Duration = Duration::from_millis(50);

    /// A key held for longer than the grace repeats its activation all the
    /// while, and the blip lasts as long.
    #[tokio::test(start_paused = true)]
    async fn repeats_keep_a_blip_open_past_the_grace() {
        let (state, engine, lease) = focused().await;
        grab(&engine).await;
        state.activated(lease);
        let held = tokio::time::Instant::now();
        while held.elapsed() < 3 * FOCUS_BLIP_GRACE {
            tokio::time::sleep(REPEAT).await;
            assert!(
                state.holds(lease),
                "lost {:?} into the hold",
                held.elapsed()
            );
            state.activated(lease);
        }
        ungrab(&engine).await;
        assert!(state.holds(lease));
        assert_eq!(standing_now(&state, lease), Some(Standing::Held));
    }

    /// Once the repeats stop, focus has the grace from the last one.
    #[tokio::test(start_paused = true)]
    async fn a_blip_ends_a_grace_after_the_last_repeat() {
        let (state, engine, lease) = focused().await;
        grab(&engine).await;
        state.activated(lease);
        for _ in 0..30 {
            tokio::time::sleep(REPEAT).await;
            state.activated(lease);
        }
        tokio::time::sleep(FOCUS_BLIP_GRACE - TICK).await;
        assert!(
            state.holds(lease),
            "ended before the grace from the last repeat"
        );
        tokio::time::sleep(2 * TICK).await;
        assert!(
            !state.holds(lease),
            "outlived the grace from the last repeat"
        );
    }

    /// The Toggle reaches the daemon after the grab's FocusOut, and the key
    /// may be released before or after it.
    #[tokio::test(start_paused = true)]
    async fn a_blip_around_an_activation_keeps_the_lease() {
        for released_first in [false, true] {
            let (state, engine, lease) = focused().await;
            let mut loss = state.loss(lease);
            grab(&engine).await;
            tokio::time::sleep(EDGE_AFTER).await;
            if released_first {
                ungrab(&engine).await;
                assert_eq!(standing_now(&state, lease), Some(Standing::Unsure));
                state.activated(lease);
            } else {
                state.activated(lease);
                assert_eq!(standing_now(&state, lease), None, "writes wait");
                ungrab(&engine).await;
            }
            assert_eq!(standing_now(&state, lease), Some(Standing::Held));
            tokio::time::sleep(FOCUS_BLIP_GRACE * 2).await;
            assert!(state.holds(lease), "released first: {released_first}");
            assert!(loss.next().now_or_never().is_none(), "a blip was reported");
        }
    }

    /// An activation just before the FocusOut counts as well.
    #[tokio::test(start_paused = true)]
    async fn an_activation_just_before_the_focus_out_counts() {
        let (state, engine, lease) = focused().await;
        state.activated(lease);
        tokio::time::sleep(ACTIVATION_WINDOW - TICK).await;
        grab(&engine).await;
        ungrab(&engine).await;
        assert_eq!(standing_now(&state, lease), Some(Standing::Held));

        tokio::time::sleep(ACTIVATION_WINDOW + TICK).await;
        grab(&engine).await;
        assert_eq!(standing_now(&state, lease), Some(Standing::Unsure));
    }

    /// Without an activation the blip may be an application moving focus
    /// between fields that share one context: a loss once the window is over.
    #[tokio::test(start_paused = true)]
    async fn a_blip_without_an_activation_is_a_loss_when_the_window_ends() {
        let (state, engine, lease) = focused().await;
        grab(&engine).await;
        ungrab(&engine).await;
        tokio::time::sleep(ACTIVATION_WINDOW - TICK).await;
        assert_eq!(standing_now(&state, lease), Some(Standing::Unsure));
        tokio::time::sleep(TICK * 2).await;
        assert!(!state.holds(lease));

        state.activated(lease);
        assert!(!state.holds(lease), "a late activation revived it");
    }

    #[tokio::test(start_paused = true)]
    async fn focus_away_past_the_grace_ends_the_lease_even_after_an_activation() {
        let (state, engine, lease) = focused().await;
        grab(&engine).await;
        state.activated(lease);
        tokio::time::sleep(FOCUS_BLIP_GRACE - TICK).await;
        assert!(state.holds(lease), "ended before the grace ran out");
        tokio::time::sleep(TICK * 2).await;
        assert!(!state.holds(lease));

        ungrab(&engine).await;
        assert!(!state.holds(lease), "focus back too late revived it");
    }

    #[tokio::test(start_paused = true)]
    async fn focus_arriving_anywhere_else_during_a_blip_ends_the_lease() {
        let others = [
            FocusChange::In(Some(Context {
                path: OTHER,
                client: APP,
            })),
            FocusChange::In(None),
        ];
        for activated in [false, true] {
            for other in others {
                let (state, engine, lease) = focused().await;
                grab(&engine).await;
                if activated {
                    state.activated(lease);
                }
                state.focus(other);
                assert!(!state.holds(lease), "{other:?}, activated: {activated}");
            }
        }
    }

    /// Focus out again before the activation: it is not back any more.
    #[tokio::test(start_paused = true)]
    async fn focus_out_again_before_the_activation_is_still_away() {
        let (state, engine, lease) = focused().await;
        grab(&engine).await;
        ungrab(&engine).await;
        grab(&engine).await;
        state.activated(lease);
        assert_eq!(standing_now(&state, lease), None, "writes wait");
        ungrab(&engine).await;
        assert_eq!(standing_now(&state, lease), Some(Standing::Held));
    }

    /// The grace of an earlier blip must not cut a later one short.
    #[tokio::test(start_paused = true)]
    async fn a_grace_ends_only_its_own_blip() {
        let (state, engine, lease) = focused().await;
        let half = FOCUS_BLIP_GRACE / 2;
        grab(&engine).await;
        state.activated(lease);
        tokio::time::sleep(half).await;
        ungrab(&engine).await;
        grab(&engine).await;
        state.activated(lease);
        tokio::time::sleep(half + TICK).await;
        assert!(
            state.holds(lease),
            "the first blip's grace ended the second"
        );
        tokio::time::sleep(half).await;
        assert!(!state.holds(lease));
    }

    #[tokio::test(start_paused = true)]
    async fn writes_hold_until_the_activation_then_wait_until_focus_is_back() {
        for back in [true, false] {
            let (state, engine, lease) = focused().await;
            grab(&engine).await;
            assert_eq!(
                state.standing(lease, false).now_or_never(),
                Some(Standing::Unsure),
                "an unsure write must not wait: the loop has the Toggle to read"
            );
            assert!(state.standing(lease, true).now_or_never().is_none());
            state.activated(lease);
            let waiting = tokio::spawn({
                let state = Arc::clone(&state);
                async move { state.standing(lease, false).await }
            });
            tokio::time::sleep(FOCUS_BLIP_GRACE / 2).await;
            assert!(!waiting.is_finished(), "a write went out during the blip");
            if back {
                ungrab(&engine).await;
            }
            tokio::time::sleep(FOCUS_BLIP_GRACE).await;
            let expected = if back { Standing::Held } else { Standing::Lost };
            assert_eq!(waiting.await.unwrap(), expected);
        }
        let (state, _engine, lease) = focused().await;
        assert_eq!(
            state.standing(lease, true).now_or_never(),
            Some(Standing::Held)
        );
    }

    /// A superseded target's write is refused at once, whatever the newer
    /// lease is waiting out.
    #[tokio::test(start_paused = true)]
    async fn a_superseded_lease_never_waits_on_the_newer_ones_blip() {
        let (state, engine, older) = focused().await;
        let newer = state.mint();
        engine.focus_in_id(FIELD.into(), APP.into()).await;
        grab(&engine).await;
        state.activated(older);
        assert_eq!(
            state.standing(older, true).now_or_never(),
            Some(Standing::Lost)
        );
        assert_eq!(standing_now(&state, newer), Some(Standing::Unsure));
    }

    #[tokio::test(start_paused = true)]
    async fn surrounding_text_from_elsewhere_during_a_blip_is_ignored() {
        let (state, engine, lease) = focused().await;
        state.surrounding(Some('a'));
        grab(&engine).await;
        state.surrounding(Some('z'));
        state.activated(lease);
        ungrab(&engine).await;
        assert_eq!(state.before_cursor(lease), Some('a'));
    }

    /// Starting by key: the grab has moved focus to the daemon's fake context
    /// when the engine is switched, and the field gets it back only when the
    /// key is released, which may be past `FOCUS_WAIT`.
    #[tokio::test(start_paused = true)]
    async fn a_start_key_held_past_the_focus_wait_still_binds_the_field() {
        let state = engine_state();
        let lease = state.mint();
        let engine = engine(&state);
        engine.focus_in_id(FAKE.into(), "fake".into()).await;
        let arrived = tokio::spawn({
            let state = Arc::clone(&state);
            async move { state.focus_arrived(lease).await }
        });
        tokio::time::sleep(FOCUS_WAIT + Duration::from_millis(100)).await;
        ungrab(&engine).await;
        assert!(arrived.await.unwrap(), "the field's focus was missed");
        assert!(state.holds(lease));
        assert_eq!(standing_now(&state, lease), Some(Standing::Held));
    }

    /// A start key held for longer than the grace: its repeats, signalled
    /// through the injector while it acquires, keep the wait for the field
    /// open until the key is released.
    #[tokio::test(start_paused = true)]
    async fn repeats_keep_an_acquire_waiting_for_the_field() {
        let state = engine_state();
        let lease = state.mint();
        let engine = engine(&state);
        engine.focus_in_id(FAKE.into(), "fake".into()).await;
        let arrived = tokio::spawn({
            let state = Arc::clone(&state);
            async move { state.focus_arrived(lease).await }
        });
        let activation = activation_for(&state);
        let held = tokio::time::Instant::now();
        while held.elapsed() < 3 * FOCUS_BLIP_GRACE {
            tokio::time::sleep(REPEAT).await;
            activation.signal();
        }
        ungrab(&engine).await;
        assert!(
            arrived.await.unwrap(),
            "the wait ended while the key was held"
        );
        assert!(state.holds(lease));
    }

    /// Once the repeats stop, the field has the grace from the last one.
    #[tokio::test(start_paused = true)]
    async fn an_acquire_waits_a_grace_after_the_last_repeat() {
        let state = engine_state();
        let lease = state.mint();
        let engine = engine(&state);
        engine.focus_in_id(FAKE.into(), "fake".into()).await;
        let started = tokio::time::Instant::now();
        let arrived = tokio::spawn({
            let state = Arc::clone(&state);
            async move { state.focus_arrived(lease).await }
        });
        let activation = activation_for(&state);
        for _ in 0..30 {
            tokio::time::sleep(REPEAT).await;
            activation.signal();
        }
        let last = tokio::time::Instant::now();
        assert!(!arrived.await.unwrap());
        let waited = started.elapsed();
        let expected = last - started + FOCUS_BLIP_GRACE;
        assert!(
            expected <= waited && waited <= expected + TICK,
            "{waited:?}"
        );
    }

    /// A key never released, or a desktop with no field focused: the fake
    /// context keeps focus, which is no field to dictate into.
    #[tokio::test(start_paused = true)]
    async fn focus_left_on_the_fake_context_is_no_field() {
        let state = engine_state();
        let lease = state.mint();
        let engine = engine(&state);
        engine.focus_in_id(FAKE.into(), "fake".into()).await;
        let started = tokio::time::Instant::now();
        assert!(!state.focus_arrived(lease).await);
        let waited = started.elapsed();
        assert!(FOCUS_BLIP_GRACE <= waited && waited <= FOCUS_BLIP_GRACE + TICK);
        assert!(state.on_fake(lease));

        // Focus gone from the fake context again: the usual wait.
        let next = state.mint();
        engine.focus_in_id(FAKE.into(), "fake".into()).await;
        engine.focus_out().await;
        let started = tokio::time::Instant::now();
        assert!(!state.focus_arrived(next).await);
        assert!(started.elapsed() < FOCUS_BLIP_GRACE);
        assert!(!state.on_fake(next));
    }

    /// No timer to end a blip, so none is begun.
    #[test]
    fn without_a_runtime_focus_leaving_is_a_loss_at_once() {
        let state = engine_state();
        let lease = state.mint();
        let field = Context {
            path: FIELD,
            client: APP,
        };
        state.focus(FocusChange::In(Some(field)));
        state.activated(lease);
        state.focus(FocusChange::Out(None));
        assert!(!state.holds(lease));
    }

    /// A focus stream taken after the loss still reports it.
    #[tokio::test]
    async fn loss_is_reported_to_a_stream_taken_afterwards() {
        let state = engine_state();
        let lease = state.mint();
        let engine = engine(&state);
        engine.focus_in_id(FIELD.into(), "app".into()).await;
        let mut live = state.loss(lease);
        assert!(futures_util::FutureExt::now_or_never(live.next()).is_none());

        engine.focus_out_id(FIELD.into()).await;
        let mut late = state.loss(lease);
        assert_eq!(late.next().await, Some(FocusEvent::FocusOut));
        assert_eq!(live.next().await, Some(FocusEvent::FocusOut));
    }

    /// I5/FR-021: which content types `acquire`, `commit` and `set_preedit`
    /// refuse.
    #[test]
    fn secure_content_type_classification() {
        let ct = |purpose, hints| ContentType { purpose, hints };
        for refused in [ct(8, 0), ct(9, 0), ct(0, 4096), ct(0, 6144), ct(1, 4096)] {
            assert!(refused.is_secure(), "{refused:?} must be refused");
        }
        let mut accepted = vec![ct(0, 0), ct(0, 2048), ct(0, u32::MAX ^ 4096)];
        accepted.extend((0..=7).chain([10, 15]).map(|purpose| ct(purpose, 0)));
        for accepted in accepted {
            assert!(!accepted.is_secure(), "{accepted:?} must be injectable");
        }
    }

    /// The daemon only `Properties.Set`s `ContentType (uu)` and drops the
    /// error, so a method of that name is never called.
    #[test]
    fn engine_takes_content_type_as_a_write_only_property() {
        let engine = EngineObject {
            state: engine_state(),
        };
        let mut xml = String::new();
        zbus::object_server::Interface::introspect_to_writer(&engine, &mut xml, 0);
        assert!(
            xml.contains(r#"<property name="ContentType" type="(uu)" access="write">"#),
            "{xml}"
        );
        assert!(!xml.contains("SetContentType"), "{xml}");
    }

    /// Declared as `ibus-engine-simple` does (`readonly (b) FocusId`,
    /// `FocusInId(s object_path, s client)`, `FocusOutId(s object_path)`), and
    /// dispatched in order: zbus spawns a task per call by default, which could
    /// apply a `FocusOutId` after the `FocusInId` that followed it.
    #[test]
    fn engine_asks_for_identified_focus_and_handles_it_in_order() {
        let engine = EngineObject {
            state: engine_state(),
        };
        let mut xml = String::new();
        zbus::object_server::Interface::introspect_to_writer(&engine, &mut xml, 0);
        for property in ["FocusId", "ActiveSurroundingText"] {
            assert!(
                xml.contains(&format!(
                    r#"<property name="{property}" type="b" access="read""#
                )),
                "{xml}"
            );
        }
        let method = |name: &str| {
            let start = xml
                .find(&format!(r#"<method name="{name}">"#))
                .unwrap_or_else(|| panic!("{name} missing: {xml}"));
            let end = start + xml[start..].find("</method>").expect("method end");
            xml[start..end].to_string()
        };
        let focus_in = method("FocusInId");
        assert!(
            focus_in.contains(r#"name="object_path" type="s""#),
            "{focus_in}"
        );
        assert!(focus_in.contains(r#"name="client" type="s""#), "{focus_in}");
        let focus_out = method("FocusOutId");
        assert!(
            focus_out.contains(r#"name="object_path" type="s""#),
            "{focus_out}"
        );
        assert!(!zbus::object_server::Interface::spawn_tasks_for_methods(
            &engine
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn the_character_before_the_cursor_is_kept_for_the_focused_lease() {
        let state = engine_state();
        let lease = state.mint();
        let engine = engine(&state);
        engine
            .set_surrounding_text(ibus_text("stale").try_into().unwrap(), 5, 5)
            .await;
        assert_eq!(
            state.before_cursor(lease),
            None,
            "text before focus is another field's"
        );

        engine.focus_in_id(FIELD.into(), "app".into()).await;
        engine
            .set_surrounding_text(ibus_text("Hi there. ").try_into().unwrap(), 9, 9)
            .await;
        assert_eq!(state.before_cursor(lease), Some('.'));
        engine
            .set_surrounding_text(ibus_text("Hi there. ").try_into().unwrap(), 10, 10)
            .await;
        assert_eq!(state.before_cursor(lease), Some(' '));
        engine
            .set_surrounding_text(ibus_text("Hi there. ").try_into().unwrap(), 8, 3)
            .await;
        assert_eq!(
            state.before_cursor(lease),
            Some(' '),
            "a commit replaces the selection, so what precedes it counts"
        );

        engine.focus_out_id(FIELD.into()).await;
        tokio::time::sleep(ACTIVATION_WINDOW + TICK).await;
        assert_eq!(
            state.before_cursor(lease),
            None,
            "a lost lease says nothing"
        );
        let next = state.mint();
        assert_eq!(state.before_cursor(next), None, "nor carries over");
    }

    #[test]
    fn the_character_before_is_counted_in_characters() {
        let text = ibus_text("ça va");
        assert_eq!(char_before(&text, 0), None, "start of the field");
        assert_eq!(char_before(&text, 1), Some('ç'));
        assert_eq!(char_before(&text, 2), Some('a'));
        assert_eq!(char_before(&text, 5), Some('a'));
        assert_eq!(char_before(&text, 6), None, "past the end");
        assert_eq!(char_before(&Value::from("not IBusText"), 1), None);
    }

    #[tokio::test]
    async fn content_type_write_updates_the_snapshot() {
        let state = engine_state();
        let engine = EngineObject {
            state: Arc::clone(&state),
        };
        engine.set_content_type((8, 1 << 11)).await;
        assert_eq!(
            state.content_type(),
            ContentType {
                purpose: 8,
                hints: 1 << 11
            }
        );
        engine.set_content_type((0, 0)).await;
        assert_eq!(state.content_type(), ContentType::default());
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
        let dir =
            std::env::temp_dir().join(format!("myna-ibus-test-{}-{}", std::process::id(), tag));
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

    /// Under confinement the real home comes from snapd, not the user
    /// database: an SSSD/AD account has no /etc/passwd line, and NSS does not
    /// resolve it inside the snap either.
    #[test]
    fn snap_finds_address_under_real_home_of_nss_only_user() {
        let root = temp_dir("nss-only");
        let real_home = root.join("first.last@example.com");
        let snap_home = real_home.join("snap/myna/x1");
        let file = fake_address_file(
            &real_home.join(".config/ibus/bus"),
            "abc-unix-wayland-0",
            "real",
        );
        let vars: HashMap<&str, String> = HashMap::from([
            ("HOME", snap_home.display().to_string()),
            (
                "XDG_CONFIG_HOME",
                real_home
                    .join("snap/myna/common/.config")
                    .display()
                    .to_string(),
            ),
            ("SNAP_REAL_HOME", real_home.display().to_string()),
            ("WAYLAND_DISPLAY", "wayland-0".to_owned()),
        ]);

        let addr = discover_address_in(&|k| vars.get(k).cloned()).unwrap();

        let want = std::fs::read_to_string(&file).unwrap();
        assert!(want.contains(&addr), "{addr} not from {}", file.display());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Naming only the first dir pointed at the snap-private one, which never
    /// holds the file, and hid that the real home was not searched at all.
    #[test]
    fn missing_address_names_every_dir_searched() {
        let vars: HashMap<&str, String> = HashMap::from([
            ("XDG_CONFIG_HOME", "/nonexistent/common/.config".to_owned()),
            ("HOME", "/nonexistent/x1".to_owned()),
            ("SNAP_REAL_HOME", "/nonexistent/real".to_owned()),
        ]);

        let err = discover_address_in(&|k| vars.get(k).cloned())
            .unwrap_err()
            .to_string();

        for dir in [
            "/nonexistent/common/.config/ibus/bus",
            "/nonexistent/x1/.config/ibus/bus",
            "/nonexistent/real/.config/ibus/bus",
        ] {
            assert!(err.contains(dir), "{dir} missing from: {err}");
        }
    }

    /// A dead daemon PID is reported as such, naming the file and the PID -
    /// the socket it left behind is still on disk, so only the PID check can
    /// catch this (the common case: ibus exited without cleaning up).
    #[test]
    fn stale_daemon_is_not_picked() {
        let dir = temp_dir("stale");
        let sock = dir.join("sock-left-behind");
        std::fs::write(&sock, []).unwrap();
        std::fs::write(
            dir.join("abc-unix-wayland-0"),
            // 4194303 is above the default pid_max: it cannot be alive. (PID 2
            // would be - kthreadd - which is why a "small pid" is no test.)
            format!(
                "IBUS_ADDRESS=unix:path={},guid=x\nIBUS_DAEMON_PID=4194303\n",
                sock.display()
            ),
        )
        .unwrap();
        let files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        let err = pick_address(files, Some("unix-wayland-0"), &dir).unwrap_err();
        let msg = err.to_string();
        // The message must name the check that failed and the file it failed
        // on: `ibus restart` is the fix for a dead PID, and nothing but a
        // rewritten address file fixes a socket that is not there.
        assert!(msg.contains("PID 4194303 is gone"), "{msg}");
        assert!(msg.contains("abc-unix-wayland-0"), "{msg}");
        assert!(!msg.contains("socket"), "the socket is there: {msg}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The socket path in an address file is D-Bus percent-encoded: a home with
    /// an `@` in it (every AD login) reaches us as `%40` and must be decoded
    /// before it is looked for on disk, or a live daemon is rejected forever.
    #[test]
    fn percent_encoded_socket_path_is_decoded() {
        let dir = temp_dir("percent");
        // The real thing: `/home/didier.roche@canonical.com/.cache/ibus/...`
        let home = dir.join("didier.roche@canonical.com");
        std::fs::create_dir_all(&home).unwrap();
        let sock = home.join("dbus-E7P10tya");
        std::fs::write(&sock, []).unwrap();
        std::fs::write(
            dir.join("abc-unix-wayland-0"),
            format!(
                "IBUS_ADDRESS=unix:path={},guid=x\nIBUS_DAEMON_PID={}\n",
                sock.display().to_string().replace('@', "%40"),
                std::process::id()
            ),
        )
        .unwrap();
        let files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file())
            .collect();
        let addr = pick_address(files, Some("unix-wayland-0"), &dir)
            .expect("a live daemon whose socket is there must be picked");
        // Handed to zbus still encoded: it unescapes the address itself.
        assert!(addr.contains("%40"), "{addr}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The address handed to zbus carries the DECODED path: zbus 5.18 does not
    /// unescape `path=` itself, so an `@` home reaches `connect(2)` as `%40`
    /// and fails with ENOENT (reproduced against a live ibus-daemon 1.5.34 in a
    /// `/tmp/d.r@c` home, 2026-09-04). The guid survives the rebuild.
    #[test]
    fn zbus_address_carries_the_decoded_path() {
        let addr = to_zbus_address(
            "unix:path=/home/didier.roche%40canonical.com/.cache/ibus/dbus-E7P10tya,guid=c13b8599eb3c6db4e1e4a9006a9a78ca",
        )
        .expect("a valid address");
        let Transport::Unix(unix) = addr.transport() else {
            panic!("expected a unix transport");
        };
        let UnixSocket::File(path) = unix.path() else {
            panic!("expected a socket file path");
        };
        assert_eq!(
            path,
            &PathBuf::from("/home/didier.roche@canonical.com/.cache/ibus/dbus-E7P10tya")
        );
        assert_eq!(
            addr.guid().map(|g| g.to_string()),
            Some("c13b8599eb3c6db4e1e4a9006a9a78ca".to_owned())
        );
    }

    /// An address with nothing to decode is passed through untouched.
    #[test]
    fn zbus_address_without_encoding_is_unchanged() {
        let raw = "unix:abstract=/tmp/ibus/dbus-abcdef,guid=c13b8599eb3c6db4e1e4a9006a9a78ca";
        assert_eq!(to_zbus_address(raw).unwrap().to_string(), raw);
    }

    /// Percent-decoding is byte-wise, and leaves a bare `%` alone.
    #[test]
    fn address_path_decodes_bytes_not_characters() {
        assert_eq!(address_path("/home/a%40b/x"), PathBuf::from("/home/a@b/x"));
        assert_eq!(address_path("/tmp/100%"), PathBuf::from("/tmp/100%"));
        assert_eq!(address_path("/tmp/%2"), PathBuf::from("/tmp/%2"));
        assert_eq!(address_path("/tmp/%c3%a9"), PathBuf::from("/tmp/\u{e9}"));
    }

    /// A live daemon whose socket path is not there (an address file left by a
    /// session with a different home) is reported as a missing socket, not as a
    /// dead daemon - the two have different fixes.
    #[test]
    fn missing_socket_is_named_without_blaming_the_pid() {
        let dir = temp_dir("nosock");
        std::fs::write(
            dir.join("abc-unix-wayland-0"),
            format!(
                "IBUS_ADDRESS=unix:path={}/gone,guid=x\nIBUS_DAEMON_PID={}\n",
                dir.display(),
                std::process::id()
            ),
        )
        .unwrap();
        let files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        let err = pick_address(files, Some("unix-wayland-0"), &dir).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("its socket"), "{msg}");
        assert!(msg.contains("/gone"), "{msg}");
        assert!(
            !msg.contains("is gone ("),
            "must not blame the live PID: {msg}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
