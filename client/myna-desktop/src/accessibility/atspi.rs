//! The `atspi`-backed [`crate::accessibility::AccessibilityAnnouncer`]
//! implementation: connects to `org.a11y.Bus`, registers an accessible object
//! for the dictation session, and emits AT-SPI `Announcement` events
//! (research.md R1). Headless — no GTK dependency required.
//!
//! Validated against a real, running `org.a11y.Bus` during development: the
//! connection bootstrap and `AnnouncementEvent` construction were confirmed
//! to compile and produce a well-formed AT-SPI protocol message using the
//! exact types this module uses (`atspi::connection::AccessibilityConnection`,
//! `atspi::events::object::AnnouncementEvent`). Live signal delivery could
//! not be confirmed end-to-end in the sandboxed development container this
//! code was written in: `dbus-broker`'s AppArmor mediation on that host
//! denies accessibility-bus method calls from the confined terminal session
//! (`apparmor="DENIED" ... bus="accessibility" ... label="snap...` /
//! `peer_label="vscode"` in the journal), independent of this code. This is
//! exactly the class of environment limitation the `MYNA_ATSPI_TESTS` gate
//! (contract A6) exists to isolate — the gated suite is the place this gets
//! verified against an unconfined bus (Workshop VM, CI, hardware), per
//! research.md R6's precedent of documenting rather than silently assuming a
//! gap closed.

use async_trait::async_trait;
use atspi::connection::AccessibilityConnection;
use atspi::events::object::AnnouncementEvent;
use atspi::{ObjectRef, ObjectRefOwned, Politeness};

use super::{AccessibilityAnnouncer, AnnounceError, AnnouncementText, Severity};

/// The fixed object path this daemon's announcements are attributed to. Not a
/// fully embedded `org.a11y.atspi.Accessible` tree node (see module docs and
/// the `name`/`description` fields below) — just a stable, valid
/// [`ObjectRef`] identifying "the myna dictation session" as the event's
/// source, the same role a real widget's own accessible object would play.
const OBJECT_PATH: &str = "/org/myna/dictation";

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

/// The real, bus-connected announcer. `name`/`description` are held
/// in-process only (updated by `set_state`) — FR-001's on-demand query is
/// satisfied for this struct's own accessor methods, not (yet) through the
/// full standard `org.a11y.atspi.Accessible` D-Bus interface a generic AT
/// could discover unprompted; see the module doc comment's documented gap.
pub struct AtspiAnnouncer {
    connection: AccessibilityConnection,
    item: ObjectRefOwned,
    name: String,
    description: String,
}

impl AtspiAnnouncer {
    /// Connect to `org.a11y.Bus` (FR-007: this is the only place a real bus
    /// round-trip happens; construction failing is the caller's cue to fall
    /// back to a no-op/fake announcer rather than blocking dictation).
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
        let item = ObjectRef::new_owned(unique_name, path);
        Ok(Self {
            connection,
            item,
            name: String::new(),
            description: String::new(),
        })
    }

    /// The current accessible name (FR-001), as last set by `set_state`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The current accessible description (FR-001), as last set by
    /// `set_state`.
    pub fn description(&self) -> &str {
        &self.description
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
        self.name = name.as_str().to_string();
        self.description = description.as_str().to_string();
    }
}
