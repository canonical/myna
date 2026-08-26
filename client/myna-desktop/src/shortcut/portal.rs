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
    /// Nothing owns the portal's bus name, and this daemon will not be the one
    /// to start it (see [`portal_is_up`]). Distinct from
    /// [`Self::PortalUnavailable`] because nobody was asked for anything: it
    /// costs one bus round trip and is worth re-checking often.
    #[error("no portal running yet: {0}")]
    PortalNotRunning(String),
}

/// A raw portal activation edge (before dedup). Public so the hermetic test can
/// script a signal stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortalSignal {
    Activated,
    Deactivated,
}

/// How portal activations map to dictation edges.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ActivationMode {
    /// Each full keypress flips start/stop (the default — matches the
    /// control-socket toggle; hold-to-talk is the uncomfortable outlier).
    #[default]
    Toggle,
    /// Hold-to-talk: key down = start, key up = stop.
    Hold,
}

/// The autorepeat-dedup state machine. Hold: first `Activated` wins until a
/// `Deactivated`; repeats in between are ignored (FR-008). Toggle: the first
/// `Activated` of each physical press flips the session edge; `Deactivated`
/// only rearms the next press.
#[derive(Debug, Default)]
struct Dedup {
    mode: ActivationMode,
    /// Hold: the key is currently down. Toggle: a dictation session is active.
    pressed: bool,
    /// Toggle only: the key is physically down (autorepeat guard).
    key_down: bool,
}

impl Dedup {
    fn with_mode(mode: ActivationMode) -> Self {
        Self {
            mode,
            ..Default::default()
        }
    }

    /// Force `pressed` back to "not recording" — a no-op in `Hold` mode,
    /// where `pressed` tracks the real physical key-down state (not session
    /// state) and can't desync from the controller's own view of the
    /// session; only `Toggle` mode's session-decoupled `pressed` bit needs
    /// resyncing (see `Trigger::resync`'s doc comment).
    fn resync(&mut self) {
        if self.mode == ActivationMode::Toggle {
            self.pressed = false;
        }
    }

    fn on(&mut self, signal: PortalSignal) -> Option<TriggerEdge> {
        match self.mode {
            ActivationMode::Hold => match signal {
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
            },
            ActivationMode::Toggle => match signal {
                PortalSignal::Activated if !self.key_down => {
                    self.key_down = true;
                    self.pressed = !self.pressed;
                    Some(if self.pressed {
                        TriggerEdge::Press
                    } else {
                        TriggerEdge::Release
                    })
                }
                PortalSignal::Activated => None, // autorepeat while held — ignore
                PortalSignal::Deactivated => {
                    self.key_down = false; // rearm; never an edge in toggle mode
                    None
                }
            },
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

/// A [`Trigger`] backed by the GlobalShortcuts portal.
pub struct GlobalShortcutTrigger {
    signals: BoxStream<'static, PortalSignal>,
    dedup: Dedup,
    _keepalive: Keepalive,
}

impl GlobalShortcutTrigger {
    /// Build a trigger from a pre-made [`PortalSignal`] stream — the hermetic
    /// test seam (no D-Bus / portal). Uses the default [`ActivationMode`].
    pub fn from_signals(signals: BoxStream<'static, PortalSignal>) -> Self {
        Self::from_signals_with_mode(signals, ActivationMode::default())
    }

    /// [`Self::from_signals`] with an explicit [`ActivationMode`].
    pub fn from_signals_with_mode(
        signals: BoxStream<'static, PortalSignal>,
        mode: ActivationMode,
    ) -> Self {
        Self {
            signals,
            dedup: Dedup::with_mode(mode),
            _keepalive: Keepalive::None,
        }
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
        mode: ActivationMode,
    ) -> Result<Self, TriggerError> {
        let conn = crate::dbus::serve::connect_session()
            .await
            .map_err(|e| TriggerError::PortalUnavailable(e.to_string()))?;
        Self::bind_with_connection(conn, shortcut_id, preferred_trigger, mode).await
    }

    /// As [`Self::bind`] but on a caller-provided session-bus connection.
    #[cfg(not(test))]
    pub async fn bind_with_connection(
        conn: zbus::Connection,
        shortcut_id: &str,
        preferred_trigger: Option<&str>,
        mode: ActivationMode,
    ) -> Result<Self, TriggerError> {
        use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
        use futures_util::future;

        if !portal_is_up(&conn).await? {
            return Err(TriggerError::PortalNotRunning(format!(
                "nothing owns {PORTAL_BUS_NAME}; waiting rather than starting one"
            )));
        }

        // Cloned, not moved: `portal_owner_changed` below needs the same
        // connection (a zbus `Connection` clone is a handle to the one socket).
        let shortcuts = GlobalShortcuts::with_connection(conn.clone())
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
        // The portal can restart under a long-lived daemon (a package
        // upgrade, a crash, `systemctl --user restart`). Its session dies with
        // it, but these are *bus-level* signal matches, so the streams above
        // stay happily open and this trigger would go on listening to a
        // session that no longer exists: the hotkey silently stops working and
        // nothing says so. Ending the stream when the portal's bus name
        // changes owner turns that into a plain rebind, which
        // `retry::RetryingTrigger` already knows how to do.
        let restarted = portal_owner_changed(&conn).await?;
        let signals = stream::select(activated, deactivated)
            .take_until(restarted)
            .boxed();

        myna_core::info_log!(
            "portal",
            "bound '{shortcut_id}' (preferred {}, {mode:?}); session live",
            preferred_trigger.unwrap_or("portal default")
        );
        Ok(Self {
            signals,
            dedup: Dedup::with_mode(mode),
            _keepalive: Keepalive::Portal(shortcuts, session),
        })
    }
}

/// The bus name the portal serves on; owning it is what makes a portal *the*
/// portal, so a change of owner is exactly "the portal I bound against is not
/// the portal any more".
#[cfg(not(test))]
const PORTAL_BUS_NAME: &str = "org.freedesktop.portal.Desktop";

/// Whether a portal is already running.
///
/// Not a courtesy check. Every call below auto-starts `xdg-desktop-portal` if
/// nothing owns the name, and a portal started before the compositor has
/// exported `XDG_CURRENT_DESKTOP` resolves its backends against an empty
/// desktop: `gtk.portal` as a last-resort fallback for every interface, never
/// `gnome.portal`, which is the only one implementing GlobalShortcuts. That
/// map is cached for the life of the session and breaks the portal for every
/// app on the desktop, not just this one. A user daemon is reached by
/// `default.target`, which is inside exactly that window.
///
/// So wait for a portal instead of asking for one: an unowned name is
/// [`TriggerError::PortalUnavailable`], which [`crate::shortcut::retry`]
/// already treats as the ordinary PAM-login race.
#[cfg(not(test))]
async fn portal_is_up(conn: &zbus::Connection) -> Result<bool, TriggerError> {
    let name = zbus::names::BusName::try_from(PORTAL_BUS_NAME)
        .expect("PORTAL_BUS_NAME is a valid bus name");
    zbus::fdo::DBusProxy::new(conn)
        .await
        .map_err(|e| TriggerError::PortalUnavailable(e.to_string()))?
        .name_has_owner(name)
        .await
        .map_err(|e| TriggerError::PortalUnavailable(e.to_string()))
}

/// Wait for a portal to appear, or give up after `limit`.
///
/// The counterpart to [`portal_is_up`]: having declined to start a portal, the
/// daemon has to find out when someone else does. Polling for that is what it
/// looks like to have no answer - and on a machine whose desktop is never
/// coming, polling is the whole cost of running here at all. The bus already
/// offers the answer as a signal, so take it and sleep.
///
/// Subscribe *first*, then re-check: a portal that appears between the caller's
/// [`portal_is_up`] and the match being installed would otherwise be missed,
/// and the next thing to wake this daemon would be `limit` - the exact stall
/// this exists to remove.
#[cfg(not(test))]
pub async fn await_portal(limit: std::time::Duration) {
    use futures_util::future::{select, Either};

    let conn = match crate::dbus::serve::connect_session().await {
        Ok(conn) => conn,
        // No session bus to watch. The caller's next attempt fails the same
        // way this one did, so a plain sleep is the whole of the fallback.
        Err(_) => return tokio::time::sleep(limit).await,
    };
    let appeared = async {
        let dbus = zbus::fdo::DBusProxy::new(&conn).await?;
        let mut changes = dbus
            .receive_name_owner_changed_with_args(&[(0, PORTAL_BUS_NAME)])
            .await?;
        if portal_is_up(&conn).await.unwrap_or(false) {
            return Ok(());
        }
        while let Some(change) = changes.next().await {
            // Owner *lost* also arrives here (a portal restarting sends both
            // halves); only an acquisition means there is something to bind.
            if let Ok(args) = change.args() {
                if args.new_owner().is_some() {
                    myna_core::info_log!("portal", "{PORTAL_BUS_NAME} appeared");
                    return Ok(());
                }
            }
        }
        Err(zbus::Error::InvalidReply)
    };
    futures_util::pin_mut!(appeared);
    match select(appeared, Box::pin(tokio::time::sleep(limit))).await {
        // A portal appeared, or the net expired. Either way, try again now.
        Either::Left((Ok(()), _)) | Either::Right(_) => {}
        // The watch itself broke. Serve out the net rather than returning:
        // this function is the retry loop's entire delay, so an early return
        // here is an unbounded spin against a bus that just failed us.
        Either::Left((Err(e), net)) => {
            myna_core::dbg_log!(
                "portal",
                "cannot watch for a portal ({e}); falling back to the net"
            );
            net.await;
        }
    }
}

/// A future that resolves the first time `org.freedesktop.portal.Desktop`
/// changes owner. Both halves of a restart (owner lost, then owner acquired)
/// resolve it; either is a good enough reason to rebind, and the rebind's own
/// backoff absorbs the race with a portal that is still starting.
#[cfg(not(test))]
async fn portal_owner_changed(
    conn: &zbus::Connection,
) -> Result<impl std::future::Future<Output = ()>, TriggerError> {
    let dbus = zbus::fdo::DBusProxy::new(conn)
        .await
        .map_err(|e| TriggerError::PortalUnavailable(e.to_string()))?;
    let mut changes = dbus
        .receive_name_owner_changed_with_args(&[(0, PORTAL_BUS_NAME)])
        .await
        .map_err(|e| TriggerError::PortalUnavailable(e.to_string()))?;
    Ok(async move {
        let _ = changes.next().await;
        myna_core::info_log!(
            "portal",
            "{PORTAL_BUS_NAME} changed owner; session is stale"
        );
    })
}

#[async_trait]
impl Trigger for GlobalShortcutTrigger {
    async fn next_edge(&mut self) -> Option<TriggerEdge> {
        // Pull signals until one yields an edge; a dropped/ended stream (session
        // closed / shortcut unbound) ends the trigger (`None`).
        loop {
            match self.signals.next().await {
                Some(sig) => {
                    myna_core::dbg_log!("portal", "signal {sig:?}");
                    if let Some(edge) = self.dedup.on(sig) {
                        myna_core::info_log!("portal", "activation -> {edge:?}");
                        return Some(edge);
                    }
                }
                None => {
                    myna_core::info_log!("portal", "signal stream ended; shortcut is gone");
                    return None;
                }
            }
        }
    }

    async fn resync(&mut self) {
        self.dedup.resync();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trigger(signals: Vec<PortalSignal>) -> GlobalShortcutTrigger {
        GlobalShortcutTrigger::from_signals_with_mode(
            stream::iter(signals).boxed(),
            ActivationMode::Hold,
        )
    }

    fn toggle_trigger(signals: Vec<PortalSignal>) -> GlobalShortcutTrigger {
        GlobalShortcutTrigger::from_signals_with_mode(
            stream::iter(signals).boxed(),
            ActivationMode::Toggle,
        )
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
        let edges = drain(trigger(vec![
            PortalSignal::Activated,
            PortalSignal::Deactivated,
        ]))
        .await;
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
        let edges = drain(trigger(vec![
            PortalSignal::Deactivated,
            PortalSignal::Activated,
        ]))
        .await;
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

    // ── Toggle mode (the default): each physical press flips the session ──

    // A full press (Activated+Deactivated) = one edge; the next full press
    // produces the opposite edge — tap-to-start, tap-to-stop.
    #[tokio::test]
    async fn toggle_presses_alternate_press_release() {
        let edges = drain(toggle_trigger(vec![
            PortalSignal::Activated,
            PortalSignal::Deactivated,
            PortalSignal::Activated,
            PortalSignal::Deactivated,
        ]))
        .await;
        assert_eq!(edges, vec![TriggerEdge::Press, TriggerEdge::Release]);
    }

    // Autorepeat while the key is held still collapses to a single toggle —
    // a long hold must NOT stop the session (the hold-to-talk failure mode
    // toggle mode exists to avoid).
    #[tokio::test]
    async fn toggle_hold_does_not_stop_the_session() {
        let edges = drain(toggle_trigger(vec![
            PortalSignal::Activated,
            PortalSignal::Activated,
            PortalSignal::Activated,
            PortalSignal::Deactivated,
            PortalSignal::Activated,
            PortalSignal::Deactivated,
        ]))
        .await;
        assert_eq!(edges, vec![TriggerEdge::Press, TriggerEdge::Release]);
    }

    // A spurious Deactivated without a press yields nothing and doesn't
    // desync the next real press.
    #[tokio::test]
    async fn toggle_spurious_deactivated_is_ignored() {
        let edges = drain(toggle_trigger(vec![
            PortalSignal::Deactivated,
            PortalSignal::Activated,
            PortalSignal::Deactivated,
        ]))
        .await;
        assert_eq!(edges, vec![TriggerEdge::Press]);
    }
}
