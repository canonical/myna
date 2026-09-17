//! The `atspi`-backed [`crate::accessibility::AccessibilityAnnouncer`]
//! implementation: connects to `org.a11y.Bus`, registers a responsive
//! accessible object for the dictation session, and emits AT-SPI
//! `Announcement` events (research.md R1). Headless — no GTK dependency
//! required.
//!
//! ## Strict snap confinement only permits `/org/a11y/atspi/**`
//!
//! **Found 2026-09-01.** snapd's `desktop` interface grants exactly one
//! blanket accessibility-bus rule (quoting the generated profile: *"unfortunate,
//! but org.a11y.atspi is not designed for separation"*):
//!
//! ```text
//! dbus (receive, send)
//!     bus=accessibility
//!     path=/org/a11y/atspi/**
//!     peer=(label=unconfined),
//! ```
//!
//! `desktop-legacy` adds only a narrower allowlist of specific members on
//! `/org/a11y/atspi/accessible/{root,[0-9]*}` — `Announcement` is not among
//! them. So `/org/a11y/atspi/**` is the *entire* accessibility-bus surface a
//! confined snap has, and an object path outside it is denied outright.
//!
//! This module used to export at `/org/myna/dictation`, which is outside that
//! prefix — every announcement would have been a silent AppArmor denial in the
//! packaged build, while continuing to work perfectly in unconfined dev runs.
//! [`OBJECT_PATH`] is now the conventional application root
//! (`/org/a11y/atspi/accessible/root`), which is both what every real toolkit
//! uses and inside the permitted prefix.
//!
//! The same rule is why `myna-snap`'s `hud` app sets `NO_AT_BRIDGE=1`: GTK's
//! bridge derives its path from the app's bus name
//! (`/com/canonical/Myna/Hud/a11y/**`), also outside the prefix, and is denied
//! with no way to override the path from application code. A daemon speaking
//! AT-SPI directly, as this module does, *can* choose its path — which is what
//! makes the confined path viable here and not there.
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
use atspi::connection::AccessibilityConnection;
use atspi::events::object::AnnouncementEvent;
use atspi::proxy::socket::SocketProxy;
use atspi::{
    Interface, InterfaceSet, ObjectRef, ObjectRefOwned, Politeness, RelationType, Role, StateSet,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

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
/// confinement only permits `/org/a11y/atspi/**`"): snapd's `desktop`
/// interface grants `dbus (receive, send) bus=accessibility
/// path=/org/a11y/atspi/**` and nothing outside it, so a bespoke path makes
/// every announcement a silent AppArmor denial in the packaged build.
const OBJECT_PATH: &str = "/org/a11y/atspi/accessible/root";

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
    connection: AccessibilityConnection,
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
        let connection = AccessibilityConnection::new()
            .await
            .map_err(|e| AnnounceError(format!("could not connect to org.a11y.Bus: {e}")))?;
        let unique_name = connection
            .connection()
            .unique_name()
            .ok_or_else(|| AnnounceError("accessibility connection has no unique name".into()))?
            .to_owned();
        let path = zbus::zvariant::ObjectPath::try_from(OBJECT_PATH)
            .expect("OBJECT_PATH is a valid object path")
            .to_owned();
        let item = ObjectRef::new_owned(unique_name.clone(), path.clone());

        let state = Arc::new(Mutex::new(SharedAccessibleState::default()));

        let object_server = connection.connection().object_server();
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

        match SocketProxy::builder(connection.connection()).build().await {
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
        self.connection
            .send_event(event)
            .await
            .map_err(|e| AnnounceError(format!("failed to emit Announcement: {e}")))
    }

    async fn set_state(&mut self, name: AnnouncementText, description: AnnouncementText) {
        let mut state = self.state.lock().expect("accessible state poisoned");
        state.name = name.as_str().to_string();
        state.description = description.as_str().to_string();
    }
}
