//! `GlobalShortcutTrigger` — hold-to-talk activation over the
//! `org.freedesktop.portal.GlobalShortcuts` portal (plan T21, branch 003c/US2).
//!
//! Maps portal `Activated` → [`TriggerEdge::Press`] (deduped: first wins until
//! `Deactivated`, collapsing compositor autorepeat), `Deactivated` →
//! [`TriggerEdge::Release`], and session-end → `None`. Binds via `ashpd`/`zbus`;
//! the app ships no shortcut-config UI (the desktop's portal UI owns rebinding).
//! The real portal wiring + the autorepeat-dedup state machine land in branch
//! 003c (T022/T024); this foundational branch declares the type + error so the
//! module tree compiles. See `specs/003-desktop-injection/contracts/trigger.md`.

use async_trait::async_trait;

use super::{Trigger, TriggerEdge};

/// Why binding the global shortcut failed.
#[derive(Debug, thiserror::Error)]
pub enum TriggerError {
    /// No portal / no GlobalShortcuts backend available (clear failure — T5).
    #[error("global-shortcuts portal unavailable: {0}")]
    PortalUnavailable(String),
    /// The portal rejected the bind request.
    #[error("shortcut bind rejected: {0}")]
    BindRejected(String),
}

/// A hold-to-talk [`Trigger`] backed by the GlobalShortcuts portal
/// (implementation lands in T024).
#[derive(Debug, Default)]
pub struct GlobalShortcutTrigger {
    _private: (),
}

impl GlobalShortcutTrigger {
    /// Create a portal session and bind `shortcut_id`, offering
    /// `preferred_trigger` (e.g. `"Super+d"`) to the portal's own confirm/rebind
    /// UI. Returns `Err(PortalUnavailable)` when no portal is reachable. (Stub:
    /// T024.)
    pub async fn bind(
        _shortcut_id: &str,
        _preferred_trigger: Option<&str>,
    ) -> Result<Self, TriggerError> {
        Err(TriggerError::PortalUnavailable(
            "GlobalShortcutTrigger not yet implemented (T024)".into(),
        ))
    }
}

#[async_trait]
impl Trigger for GlobalShortcutTrigger {
    async fn next_edge(&mut self) -> Option<TriggerEdge> {
        None
    }
}
