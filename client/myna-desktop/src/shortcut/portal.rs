//! `GlobalShortcutTrigger` — hold-to-talk activation over the
//! `org.freedesktop.portal.GlobalShortcuts` portal (plan T21, T024).
//!
//! Maps portal `Activated` → [`TriggerEdge::Press`] (deduped: first wins until
//! `Deactivated`, collapsing compositor autorepeat — FR-008), `Deactivated` →
//! [`TriggerEdge::Release`], and session-end → `None`. Binds via `ashpd`; the app
//! ships no shortcut-config UI (the desktop's portal dialog owns rebinding).
//!
//! ## Testability
//!
//! The activation/autorepeat logic is a pure state machine ([`Dedup`]) fed by a
//! stream of [`PortalSignal`]s, so the full `Trigger` behavior is unit-tested
//! hermetically ([`GlobalShortcutTrigger::from_signals`], T022) with no D-Bus or
//! portal. The real portal binding ([`GlobalShortcutTrigger::bind`]) only exists
//! against a live `xdg-desktop-portal` and is proven by the env-gated suite
//! (`MYNA_PORTAL_TESTS=1`, `tests/portal_hw.rs`, T023). See
//! `specs/003-desktop-injection/contracts/trigger.md`.

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};

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

/// A raw portal activation edge (before dedup). Public so the hermetic test can
/// script a signal stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortalSignal {
    Activated,
    Deactivated,
}

/// The autorepeat-dedup state machine: first `Activated` wins until a
/// `Deactivated`; repeats in between are ignored (FR-008).
#[derive(Debug, Default)]
struct Dedup {
    pressed: bool,
}

impl Dedup {
    fn on(&mut self, signal: PortalSignal) -> Option<TriggerEdge> {
        match signal {
            PortalSignal::Activated if !self.pressed => {
                self.pressed = true;
                Some(TriggerEdge::Press)
            }
            PortalSignal::Activated => None, // autorepeat while held — ignore
            PortalSignal::Deactivated if self.pressed => {
                self.pressed = false;
                Some(TriggerEdge::Release)
            }
            PortalSignal::Deactivated => None, // spurious release — ignore
        }
    }
}

/// Keeps the portal session alive for the lifetime of the trigger (dropping it
/// would tear the session down and stop the signal stream).
#[allow(dead_code)]
enum Keepalive {
    None,
    #[cfg(not(test))]
    Portal(
        ashpd::desktop::global_shortcuts::GlobalShortcuts,
        ashpd::desktop::Session<ashpd::desktop::global_shortcuts::GlobalShortcuts>,
    ),
}

/// A hold-to-talk [`Trigger`] backed by the GlobalShortcuts portal.
pub struct GlobalShortcutTrigger {
    signals: BoxStream<'static, PortalSignal>,
    dedup: Dedup,
    _keepalive: Keepalive,
}

impl GlobalShortcutTrigger {
    /// Build a trigger from a pre-made [`PortalSignal`] stream — the hermetic
    /// test seam (no D-Bus / portal).
    pub fn from_signals(signals: BoxStream<'static, PortalSignal>) -> Self {
        Self { signals, dedup: Dedup::default(), _keepalive: Keepalive::None }
    }

    /// Create a portal session on the crate's shared session-bus connection
    /// (stale-`guid` tolerant — see [`crate::dbus::serve::connect_session`]),
    /// bind `shortcut_id` (offering `preferred_trigger` to the portal's own
    /// confirm/rebind UI — FR-009), and merge the `Activated`/`Deactivated`
    /// signals for that shortcut into one stream.
    #[cfg(not(test))]
    pub async fn bind(
        shortcut_id: &str,
        preferred_trigger: Option<&str>,
    ) -> Result<Self, TriggerError> {
        let conn = crate::dbus::serve::connect_session()
            .await
            .map_err(|e| TriggerError::PortalUnavailable(e.to_string()))?;
        Self::bind_with_connection(conn, shortcut_id, preferred_trigger).await
    }

    /// As [`Self::bind`] but on a caller-provided session-bus connection.
    #[cfg(not(test))]
    pub async fn bind_with_connection(
        conn: zbus::Connection,
        shortcut_id: &str,
        preferred_trigger: Option<&str>,
    ) -> Result<Self, TriggerError> {
        use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
        use futures_util::future;

        let shortcuts = GlobalShortcuts::with_connection(conn)
            .await
            .map_err(|e| TriggerError::PortalUnavailable(e.to_string()))?;
        let session = shortcuts
            .create_session(Default::default())
            .await
            .map_err(|e| TriggerError::PortalUnavailable(e.to_string()))?;

        let shortcut = NewShortcut::new(shortcut_id, "myna dictation (hold to talk)")
            .preferred_trigger(preferred_trigger);
        shortcuts
            .bind_shortcuts(&session, &[shortcut], None, Default::default())
            .await
            .map_err(|e| TriggerError::BindRejected(e.to_string()))?;

        // Subscribe to the two edge signals and fold them, filtered to our
        // shortcut id, into one PortalSignal stream.
        let id_a = shortcut_id.to_string();
        let id_d = shortcut_id.to_string();
        let activated = shortcuts
            .receive_activated()
            .await
            .map_err(|e| TriggerError::PortalUnavailable(e.to_string()))?
            .filter_map(move |e| {
                future::ready((e.shortcut_id() == id_a).then_some(PortalSignal::Activated))
            });
        let deactivated = shortcuts
            .receive_deactivated()
            .await
            .map_err(|e| TriggerError::PortalUnavailable(e.to_string()))?
            .filter_map(move |e| {
                future::ready((e.shortcut_id() == id_d).then_some(PortalSignal::Deactivated))
            });
        let signals = stream::select(activated, deactivated).boxed();

        Ok(Self {
            signals,
            dedup: Dedup::default(),
            _keepalive: Keepalive::Portal(shortcuts, session),
        })
    }
}

#[async_trait]
impl Trigger for GlobalShortcutTrigger {
    async fn next_edge(&mut self) -> Option<TriggerEdge> {
        // Pull signals until one yields an edge; a dropped/ended stream (session
        // closed / shortcut unbound) ends the trigger (`None`).
        loop {
            match self.signals.next().await {
                Some(sig) => {
                    if let Some(edge) = self.dedup.on(sig) {
                        return Some(edge);
                    }
                }
                None => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trigger(signals: Vec<PortalSignal>) -> GlobalShortcutTrigger {
        GlobalShortcutTrigger::from_signals(stream::iter(signals).boxed())
    }

    async fn drain(mut t: GlobalShortcutTrigger) -> Vec<TriggerEdge> {
        let mut out = Vec::new();
        while let Some(e) = t.next_edge().await {
            out.push(e);
        }
        out
    }

    // T1/T2: activate → one Press; deactivate → one Release.
    #[tokio::test]
    async fn activate_then_deactivate_maps_to_press_release() {
        let edges = drain(trigger(vec![PortalSignal::Activated, PortalSignal::Deactivated])).await;
        assert_eq!(edges, vec![TriggerEdge::Press, TriggerEdge::Release]);
    }

    // T3: autorepeat `Activated` before `Deactivated` collapses to one Press.
    #[tokio::test]
    async fn autorepeat_activated_yields_a_single_press() {
        let edges = drain(trigger(vec![
            PortalSignal::Activated,
            PortalSignal::Activated,
            PortalSignal::Activated,
            PortalSignal::Deactivated,
        ]))
        .await;
        assert_eq!(edges, vec![TriggerEdge::Press, TriggerEdge::Release]);
    }

    // Multiple hold-to-talk cycles each produce exactly one Press/Release.
    #[tokio::test]
    async fn repeated_cycles_map_one_to_one() {
        let edges = drain(trigger(vec![
            PortalSignal::Activated,
            PortalSignal::Deactivated,
            PortalSignal::Activated,
            PortalSignal::Deactivated,
        ]))
        .await;
        assert_eq!(
            edges,
            vec![
                TriggerEdge::Press,
                TriggerEdge::Release,
                TriggerEdge::Press,
                TriggerEdge::Release
            ]
        );
    }

    // A spurious Deactivated (no matching Activated) is ignored.
    #[tokio::test]
    async fn spurious_deactivated_is_ignored() {
        let edges = drain(trigger(vec![PortalSignal::Deactivated, PortalSignal::Activated])).await;
        assert_eq!(edges, vec![TriggerEdge::Press]);
    }

    // T4: an ended signal stream (session closed / unbound) ends the trigger.
    #[tokio::test]
    async fn ended_stream_ends_the_trigger() {
        let mut t = trigger(vec![PortalSignal::Activated, PortalSignal::Deactivated]);
        assert_eq!(t.next_edge().await, Some(TriggerEdge::Press));
        assert_eq!(t.next_edge().await, Some(TriggerEdge::Release));
        assert_eq!(t.next_edge().await, None);
    }
}
