//! The `atspi`-backed [`crate::accessibility::AccessibilityAnnouncer`]
//! implementation: connects to `org.a11y.Bus`, registers a responsive
//! accessible object for the dictation session, and emits AT-SPI
//! `Announcement` events (research.md R1). Headless — no GTK dependency
//! required.
//!
//! ## Strict snap confinement allows a narrow member allowlist, not a prefix
//!
//! **Found 2026-09-01, corrected 2026-09-17 against the generated profile on
//! snapd 2.78** (`/var/lib/snapd/apparmor/profiles/snap.myna.myna`), probing
//! from inside `snap run --shell myna.myna`. An earlier revision of this
//! comment claimed snapd's `desktop` interface grants a blanket
//! `dbus (receive, send) bus=accessibility path=/org/a11y/atspi/**`. **It does
//! not.** The only blanket rule is *receive*:
//!
//! ```text
//! # Allow the accessibility services in the user session to send us any events
//! dbus (receive)
//!     bus=accessibility
//!     peer=(label=unconfined),
//! ```
//!
//! Every *send* is an explicit `(path, interface, member)` triple from
//! `desktop-legacy`, plus `{Hello,AddMatch,RemoveMatch,GetNameOwner,
//! NameHasOwner,StartServiceByName}` on `/org/freedesktop/DBus` from
//! `abstractions/dbus-accessibility-strict`, plus `org.a11y.Bus.GetAddress` on
//! the *session* bus. Staying under `/org/a11y/atspi/` is therefore necessary
//! but nowhere near sufficient — the member has to be on the list too.
//!
//! The practical consequence for this module is that
//! `org.freedesktop.DBus.Properties.GetAll` is permitted **only** on
//! `/org/a11y/atspi/accessible/[0-9]*`. Probed under confinement, it is denied
//! on every path a connection bootstrap would otherwise touch:
//!
//! | path | interface | result |
//! | --- | --- | --- |
//! | `/org/freedesktop/DBus` | `org.freedesktop.DBus` | `AccessDenied` |
//! | `/org/a11y/atspi/registry` | `org.a11y.atspi.Registry` | `AccessDenied` |
//! | `/org/a11y/atspi/accessible/root` | `org.a11y.atspi.Accessible` | `AccessDenied` |
//!
//! (The `member="Get*"` rule that does exist on the application root is scoped
//! to `org.a11y.atspi.Accessible`, so it does not cover `Properties.GetAll`.)
//!
//! This is why [`AtspiAnnouncer::connect`] builds its own [`zbus::Connection`]
//! rather than calling `AccessibilityConnection::new()`. That constructor
//! (`atspi-connection` 0.14, `from_address`) builds a `RegistryProxy` *and* a
//! `zbus::fdo::DBusProxy` with zbus's default property caching; the raw
//! `Builder::address(..).build()` underneath them is fine, because `Hello` is
//! allowed. Every proxy this module does construct sets
//! [`CacheProperties::No`], and the session-bus address lookup is a direct
//! `call_method` rather than a proxy, so no property read ever happens.
//! `tests/atspi_confined.rs` pins this against a `dbus-daemon` whose policy
//! mirrors those denials, so the packaged-snap failure reproduces on a
//! developer machine without a snap build.
//!
//! ## Connecting is necessary but not sufficient: `Announcement` is not allowed
//!
//! **Found 2026-09-17.** Fixing the bootstrap above lets the daemon reach the
//! accessibility bus under confinement. It does not yet make an announcement
//! *audible*, because `Announcement` is the one `Event.Object` member
//! `desktop-legacy` does not grant:
//!
//! ```text
//! dbus (send)
//!     bus=accessibility
//!     path=/org/a11y/atspi/accessible/root
//!     interface=org.a11y.atspi.Event.Object
//!     member="{ChildrenChanged,PropertyChange,StateChanged,TextCaretMoved}"
//!     peer=(name=org.freedesktop.DBus, label=unconfined),
//! ```
//!
//! Measured by A/B against that rule, sending from `snap run --shell myna.myna`:
//!
//! | member | on the allowlist? | outcome |
//! | --- | --- | --- |
//! | `ChildrenChanged` | yes | delivered; no denial logged |
//! | `Announcement` | no | `apparmor="DENIED" operation="dbus_signal" mask="send"` |
//!
//! Two traps make this easy to measure wrongly, and this investigation fell
//! into both before landing on the table above:
//!
//! - **`dbus-monitor` is not a policy oracle.** It joins with `BecomeMonitor`
//!   and is handed traffic regardless of whether policy would have delivered
//!   it, so it reports everything as arriving. The giveaway was a control
//!   signal on a path matched by no rule at all, which it also "received". Use
//!   a real subscriber — a `Gio.DBusConnection` calling `signal_subscribe`.
//! - **The subscriber has to be genuinely unconfined**, because the rule ends
//!   `peer=(label=unconfined)`. A subscriber started from a VS Code terminal
//!   inherits the `vscode` label, and then *every* member is denied — briefly
//!   making this look like a much broader problem than it is. `systemd-run
//!   --user` gives an unconfined one.
//!
//! The denials are logged by `dbus-broker`, in userspace, via the *user*
//! journal — not by the kernel. `journalctl -k` shows nothing, which is not
//! evidence of permission:
//!
//! ```text
//! journalctl --user | grep 'apparmor="DENIED".*accessibility'
//! ```
//!
//! So the remaining work is a one-member addition to snapd's `desktop-legacy`
//! interface, not a change here. Until it lands, a packaged build connects
//! cleanly and stays silent, and `announce` cannot detect that: a signal is
//! fire-and-forget, so the send reports success either way. Callers who need a
//! guarantee that something was actually spoken should use the
//! speech-dispatcher path, whose socket
//! (`/run/user/[0-9]*/speech-dispatcher/speechd.sock`) the profile does grant
//! and which was verified end-to-end from inside confinement on the same day.
//!
//! This module also used to export at `/org/myna/dictation`, outside
//! `/org/a11y/atspi/` entirely. [`OBJECT_PATH`] is now the conventional
//! application root, which is both what every real toolkit uses and inside the
//! permitted prefix.
//!
//! The path constraint is also why `myna-snap`'s `hud` app sets
//! `NO_AT_BRIDGE=1`: GTK's bridge derives its path from the app's bus name
//! (`/com/canonical/Myna/Hud/a11y/**`), outside the prefix, with no way to
//! override the path from application code. A daemon speaking AT-SPI directly,
//! as this module does, *can* choose its path.
//!
//! ## The object `AnnouncementEvent.item` points at must actually answer
//!
//! **Found 2026-08-31, first real manual verification against a live Orca
//! session (not the AppArmor-sandboxed dev container this module's earlier
//! revision was validated in — see below).** `atspi_common`'s own docs
//! describe `AnnouncementEvent.item` as "the [`ObjectRef`] which the event
//! applies to" — i.e. a real, already-registered accessible object a client
//! can introspect, not merely a stable label. Orca's event-handling path
//! unconditionally makes a *synchronous* `GetRole`/`GetParent`(`Parent`
//! property)/`Name`(property)/`GetAttributes` D-Bus round trip back to
//! whatever object an incoming `Announcement` names, as part of ordinary
//! event bookkeeping — this is not optional AT-SPI client behavior we can
//! design around. An earlier revision of this module pointed `item` at a
//! synthetic path (`/org/myna/dictation`) that nothing answered; every
//! single announcement hung Orca's event thread until its own systemd
//! watchdog (6s) killed it with `SIGABRT` — reproduced repeatedly on real
//! hardware, confirmed via `journalctl --user` showing
//! `orca.service: Failed with result 'watchdog'`.
//!
//! The fix: [`AtspiAnnouncer::connect`] now exports a minimal but genuinely
//! responsive `org.a11y.atspi.Accessible` + `org.a11y.atspi.Application`
//! pair ([`AccessibleObject`], [`ApplicationObject`]) at the exact object
//! path every `Announcement`'s `item` references, and registers with the
//! AT-SPI registry via `Socket.Embed` (best-effort — a failure there does
//! not prevent `item` from answering directly, since Orca addresses the
//! event's own `item` field, not the registry's application list). We are
//! our own application root: no parent, no children, `GetApplication`
//! returns ourselves — the same minimal shape a toolkit-free service is
//! expected to expose (mirrors what GTK's own AT-SPI bridge automatically
//! provides for a GTK app, which is why `gtk_accessible_announce()` never
//! has this problem: GTK apps already export a full, responsive tree).
//!
//! Connection bootstrap and `AnnouncementEvent` construction were
//! separately confirmed, during earlier development, to compile and produce
//! a well-formed AT-SPI protocol message in a sandboxed dev container whose
//! `dbus-broker` AppArmor policy denied the actual bus round-trip
//! (`apparmor="DENIED" ... bus="accessibility"`). That was written off at the
//! time as an environment limitation unrelated to correctness. It was not:
//! that denial was the confinement bug documented above, reproducing outside
//! a snap, and it stayed latent for as long as it did precisely because it is
//! invisible in an unconfined session. It confirmed the wire format; it could
//! not have caught the unresponsive-`item` bug, since nothing in that
//! container ever received the event at all.

use async_trait::async_trait;
use atspi::events::object::AnnouncementEvent;
use atspi::events::{DBusInterface, DBusMember, MessageConversion};
use atspi::proxy::socket::SocketProxy;
use atspi::{
    Interface, InterfaceSet, ObjectRef, ObjectRefOwned, Politeness, RelationType, Role, StateSet,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use zbus::proxy::CacheProperties;
use zbus::Address;

use super::{AccessibilityAnnouncer, AnnounceError, AnnouncementText, Severity};

/// The object path this daemon's announcements are attributed to: the
/// conventional AT-SPI *application root*, which every toolkit exports on its
/// own connection (verified against a live registry — every registered
/// application answers at this exact path, distinguished from every other by
/// its unique bus name, not by its object path). Scoped per-connection, so
/// sharing the path with the registry's own root and with every other
/// application is correct rather than a collision.
///
/// **Must stay under `/org/a11y/atspi/`** (see module docs, "Strict snap
/// confinement allows a narrow member allowlist, not a prefix"): a bespoke
/// path matches no `desktop-legacy` rule at all, making every announcement a
/// silent AppArmor denial in the packaged build.
const OBJECT_PATH: &str = "/org/a11y/atspi/accessible/root";

/// Resolves the accessibility bus address the way at-spi2-core itself does:
/// `AT_SPI_BUS_ADDRESS` if set, otherwise `org.a11y.Bus.GetAddress` on the
/// session bus.
///
/// Uses [`zbus::Connection::call_method`] rather than a generated proxy on
/// purpose: snapd permits exactly `member=GetAddress` on `/org/a11y/bus`, so a
/// proxy that cached properties there would be denied before it ever asked for
/// the address (see module docs).
async fn accessibility_bus_address() -> Result<String, AnnounceError> {
    if let Ok(address) = std::env::var("AT_SPI_BUS_ADDRESS") {
        if !address.is_empty() {
            return Ok(address);
        }
    }

    let session = zbus::Connection::session()
        .await
        .map_err(|e| AnnounceError(format!("could not reach the session bus: {e}")))?;
    let reply = session
        .call_method(
            Some("org.a11y.Bus"),
            "/org/a11y/bus",
            Some("org.a11y.Bus"),
            "GetAddress",
            &(),
        )
        .await
        .map_err(|e| AnnounceError(format!("org.a11y.Bus.GetAddress failed: {e}")))?;
    reply
        .body()
        .deserialize()
        .map_err(|e| AnnounceError(format!("org.a11y.Bus.GetAddress returned no address: {e}")))
}

/// Maps this crate's [`Severity`] to AT-SPI's [`Politeness`] (ARIA live-region
/// convention): a critical failure interrupts (`Assertive`), everything else
/// queues behind current speech (`Polite`) so it is never lost but also never
/// interrupts the user (contract A6).
fn politeness_for(severity: Option<Severity>) -> Politeness {
    match severity {
        Some(Severity::Critical) => Politeness::Assertive,
        Some(Severity::Recoverable) | None => Politeness::Polite,
    }
}

/// `Name`/`Description` shared between the [`AtspiAnnouncer`] (which updates
/// it on `set_state()`) and the exported [`AccessibleObject`] (which answers
/// `Name`/`Description` property reads with the latest value) — the module
/// doc comment's "responsive accessible object" fix.
#[derive(Debug, Default)]
struct SharedAccessibleState {
    name: String,
    description: String,
}

/// A minimal, always-fast-answering `org.a11y.atspi.Accessible` server for
/// the object every `Announcement` event's `item` points at (see module doc
/// comment). We are our own application root: no parent, no children,
/// `GetApplication` returns ourselves. Every method here answers from
/// in-process state only — never a further D-Bus round trip — so a client
/// introspecting us as part of handling our own `Announcement` event gets an
/// answer in microseconds, not a hang.
struct AccessibleObject {
    unique_name: zbus::names::OwnedUniqueName,
    path: zbus::zvariant::ObjectPath<'static>,
    state: Arc<Mutex<SharedAccessibleState>>,
}

impl AccessibleObject {
    fn self_ref(&self) -> ObjectRefOwned {
        ObjectRef::new_owned(self.unique_name.clone(), self.path.clone())
    }
}

#[zbus::interface(name = "org.a11y.atspi.Accessible")]
impl AccessibleObject {
    async fn get_application(&self) -> ObjectRefOwned {
        self.self_ref()
    }

    async fn get_attributes(&self) -> HashMap<String, String> {
        HashMap::new()
    }

    async fn get_child_at_index(&self, _index: i32) -> zbus::fdo::Result<ObjectRefOwned> {
        // No children at all: every index is out of range. Per the AT-SPI
        // Accessible interface's own documented convention ("implementors
        // to return a DBus Error when the index is out of range"), not the
        // GTK4/atk-adaptor `/org/a11y/atspi/null` fallback.
        Err(zbus::fdo::Error::Failed(
            "the myna dictation accessible object has no children".to_string(),
        ))
    }

    async fn get_children(&self) -> Vec<ObjectRefOwned> {
        Vec::new()
    }

    async fn get_index_in_parent(&self) -> i32 {
        -1
    }

    async fn get_interfaces(&self) -> InterfaceSet {
        InterfaceSet::new(Interface::Accessible | Interface::Application)
    }

    async fn get_localized_role_name(&self) -> String {
        "application".to_string()
    }

    async fn get_relation_set(&self) -> Vec<(RelationType, Vec<ObjectRefOwned>)> {
        Vec::new()
    }

    async fn get_role(&self) -> Role {
        Role::Application
    }

    async fn get_role_name(&self) -> String {
        "application".to_string()
    }

    async fn get_state(&self) -> StateSet {
        StateSet::empty()
    }

    #[zbus(property)]
    async fn accessible_id(&self) -> String {
        "myna-dictation".to_string()
    }

    #[zbus(property)]
    async fn child_count(&self) -> i32 {
        0
    }

    #[zbus(property)]
    async fn description(&self) -> String {
        self.state
            .lock()
            .expect("accessible state poisoned")
            .description
            .clone()
    }

    #[zbus(property)]
    async fn locale(&self) -> String {
        String::new()
    }

    #[zbus(property)]
    async fn name(&self) -> String {
        self.state
            .lock()
            .expect("accessible state poisoned")
            .name
            .clone()
    }

    #[zbus(property)]
    async fn parent(&self) -> ObjectRefOwned {
        // We are the application root: no parent, per the Accessible
        // interface's own documented "Null parent" convention.
        ObjectRefOwned::from(ObjectRef::Null)
    }

    #[zbus(property)]
    async fn help_text(&self) -> String {
        String::new()
    }
}

/// The `org.a11y.atspi.Application` interface, implemented on the same
/// object path as [`AccessibleObject`] — an application's root object
/// implements both (`Socket::embed`'s own doc comment: "the application's
/// root object... must support the org.a11y.atspi.Application interface").
struct ApplicationObject {
    /// Set by the registry as part of the `Socket.Embed` handshake
    /// (`Socket.xml`: "The registry sets the 'Id' property... on the @plug
    /// object"). Not read anywhere else in this codebase; stored only so the
    /// property genuinely round-trips rather than silently discarding the
    /// registry's write.
    id: Mutex<i32>,
}

#[zbus::interface(name = "org.a11y.atspi.Application")]
impl ApplicationObject {
    async fn get_locale(&self, _lctype: u32) -> String {
        String::new()
    }

    #[zbus(property)]
    async fn atspi_version(&self) -> String {
        "2.1".to_string()
    }

    #[zbus(property)]
    async fn id(&self) -> i32 {
        *self.id.lock().expect("application id poisoned")
    }

    #[zbus(property)]
    async fn set_id(&self, value: i32) {
        *self.id.lock().expect("application id poisoned") = value;
    }

    #[zbus(property)]
    async fn toolkit_name(&self) -> String {
        "myna".to_string()
    }

    #[zbus(property)]
    async fn version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }
}

/// The real, bus-connected announcer. `state` (name/description) is shared
/// with the exported [`AccessibleObject`] (updated by `set_state`) so
/// FR-001's on-demand query is answered identically whether read through
/// this struct's own accessors or through the real
/// `org.a11y.atspi.Accessible` D-Bus interface a generic AT can discover
/// unprompted — see the module doc comment.
pub struct AtspiAnnouncer {
    connection: zbus::Connection,
    item: ObjectRefOwned,
    state: Arc<Mutex<SharedAccessibleState>>,
}

impl AtspiAnnouncer {
    /// Connect to `org.a11y.Bus` (FR-007: this is the only place a real bus
    /// round-trip happens; construction failing is the caller's cue to fall
    /// back to a no-op/fake announcer rather than blocking dictation).
    ///
    /// Exports a responsive [`AccessibleObject`]/[`ApplicationObject`] pair
    /// at the same path every `Announcement` this instance emits points at
    /// (see module doc comment — required, not optional, for a client
    /// handling the event not to hang), then best-effort registers with the
    /// AT-SPI registry via `Socket.Embed`. A failed `Embed` is logged, not
    /// fatal: Orca and other clients address `item` directly from the
    /// event, not via the registry's application list, so `announce()`
    /// still works correctly without it — only cross-application discovery
    /// (e.g. a "list running accessible applications" query) would miss us.
    pub async fn connect() -> Result<Self, AnnounceError> {
        let address = accessibility_bus_address().await?;
        Self::connect_to(&address).await
    }

    /// [`connect`](Self::connect) against an explicit bus address, skipping
    /// the `org.a11y.Bus` lookup.
    ///
    /// This is the whole of `connect()` bar address discovery, so
    /// `tests/atspi_confined.rs` can point it at a `dbus-daemon` whose policy
    /// mirrors the snap's AppArmor denials and exercise the real bootstrap.
    pub async fn connect_to(address: &str) -> Result<Self, AnnounceError> {
        let address: Address = address
            .parse()
            .map_err(|e| AnnounceError(format!("malformed accessibility bus address: {e}")))?;
        // Deliberately not `AccessibilityConnection::new()`: it builds a
        // `RegistryProxy` and a `zbus::fdo::DBusProxy` with zbus's default
        // property caching, and `Properties.GetAll` is denied on both their
        // paths under strict confinement (see module docs). The raw builder
        // only sends `Hello`, which is allowed.
        let connection = zbus::connection::Builder::address(address)
            .map_err(|e| AnnounceError(format!("could not connect to org.a11y.Bus: {e}")))?
            .build()
            .await
            .map_err(|e| AnnounceError(format!("could not connect to org.a11y.Bus: {e}")))?;
        let unique_name = connection
            .unique_name()
            .ok_or_else(|| AnnounceError("accessibility connection has no unique name".into()))?
            .to_owned();
        let path = zbus::zvariant::ObjectPath::try_from(OBJECT_PATH)
            .expect("OBJECT_PATH is a valid object path")
            .to_owned();
        let item = ObjectRef::new_owned(unique_name.clone(), path.clone());

        let state = Arc::new(Mutex::new(SharedAccessibleState::default()));

        let object_server = connection.object_server();
        object_server
            .at(
                path.clone(),
                AccessibleObject {
                    unique_name: unique_name.clone(),
                    path: path.clone(),
                    state: Arc::clone(&state),
                },
            )
            .await
            .map_err(|e| AnnounceError(format!("could not export the accessible object: {e}")))?;
        object_server
            .at(path.clone(), ApplicationObject { id: Mutex::new(-1) })
            .await
            .map_err(|e| AnnounceError(format!("could not export the application object: {e}")))?;

        match SocketProxy::builder(&connection)
            .cache_properties(CacheProperties::No)
            .build()
            .await
        {
            Ok(socket) => {
                if let Err(e) = socket.embed(&(unique_name.as_str(), path.as_ref())).await {
                    eprintln!(
                        "accessibility: Socket.Embed with org.a11y.atspi.Registry failed \
                         ({e}); announcements still work, but this session won't appear \
                         in a cross-application accessible-object listing"
                    );
                }
            }
            Err(e) => {
                eprintln!(
                    "accessibility: could not build a Socket proxy for \
                     org.a11y.atspi.Registry ({e}); announcements still work"
                );
            }
        }

        Ok(Self {
            connection,
            item,
            state,
        })
    }

    /// The current accessible name (FR-001), as last set by `set_state`.
    pub fn name(&self) -> String {
        self.state
            .lock()
            .expect("accessible state poisoned")
            .name
            .clone()
    }

    /// The current accessible description (FR-001), as last set by
    /// `set_state`.
    pub fn description(&self) -> String {
        self.state
            .lock()
            .expect("accessible state poisoned")
            .description
            .clone()
    }

    /// The `(bus name, object path)` every `Announcement` this instance
    /// emits is attributed to — exposed for `tests/atspi_hw.rs` (T055-class
    /// regression coverage), which needs it to build a second client
    /// connection that introspects this exact object and asserts it answers
    /// quickly rather than hanging (see module doc comment's root-cause
    /// writeup).
    pub fn item(&self) -> &ObjectRefOwned {
        &self.item
    }
}

#[async_trait]
impl AccessibilityAnnouncer for AtspiAnnouncer {
    async fn announce(
        &mut self,
        text: AnnouncementText,
        severity: Option<Severity>,
    ) -> Result<(), AnnounceError> {
        let event = AnnouncementEvent {
            item: self.item.clone(),
            text: text.as_str().to_string(),
            live: politeness_for(severity),
        };
        // The hand-rolled equivalent of `AccessibilityConnection::send_event`,
        // which is unavailable here because this module owns a plain
        // `zbus::Connection` (see module docs on why).
        let message = zbus::Message::signal(
            OBJECT_PATH,
            <AnnouncementEvent as DBusInterface>::DBUS_INTERFACE,
            <AnnouncementEvent as DBusMember>::DBUS_MEMBER,
        )
        .and_then(|builder| {
            builder.sender(
                self.connection
                    .unique_name()
                    .expect("a bus-connected announcer always has a unique name"),
            )
        })
        .and_then(|builder| builder.build(&event.body()))
        .map_err(|e| AnnounceError(format!("failed to build Announcement: {e}")))?;
        self.connection
            .send(&message)
            .await
            .map_err(|e| AnnounceError(format!("failed to emit Announcement: {e}")))
    }

    async fn set_state(&mut self, name: AnnouncementText, description: AnnouncementText) {
        let mut state = self.state.lock().expect("accessible state poisoned");
        state.name = name.as_str().to_string();
        state.description = description.as_str().to_string();
    }
}
