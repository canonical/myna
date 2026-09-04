//! `GlobalShortcutTrigger` — hold-to-talk activation over the
//! `org.freedesktop.portal.GlobalShortcuts` portal (plan T21, T024).
//!
//! Maps portal `Activated` → [`TriggerEdge::Press`] (deduped: first wins until
//! `Deactivated`, collapsing compositor autorepeat — FR-008), `Deactivated` →
//! [`TriggerEdge::Release`], and session-end → `None`. The daemon re-establishes
//! a binding the user already asked for ([`GlobalShortcutTrigger::attach`],
//! gated on [`consent`]); asking for one in the first place is an explicit user
//! step ([`configure`], `--bind-shortcut`) that hands the portal's own dialog
//! the job.
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
    /// The backend took the bind request and never answered it within
    /// [`BIND_TIMEOUT`]. Deliberately *not* [`Self::BindRejected`]: nobody
    /// declined anything, there was simply nobody in front of the sheet (a
    /// locked screen, a switched-away session). The two want different retry
    /// policies, and conflating them is what made an unattended machine look
    /// like a user saying no over and over.
    #[error("shortcut bind unanswered: {0}")]
    BindUnanswered(String),
    /// The portal holds no binding for this app and none has been asked for.
    /// Nothing is broken and nothing is pending: the user has not run
    /// `--bind-shortcut`.
    #[error("no dictation shortcut bound: {0}")]
    NoShortcutBound(String),
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

impl ActivationMode {
    /// How to *use* the key, in the words the portal shows the user next to it
    /// (GNOME's Settings → Keyboard list, and the bind dialog). Derived from
    /// the mode rather than written out once, because a fixed string told
    /// every user to hold a key the daemon was treating as a toggle.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Toggle => "myna dictation (tap to start/stop)",
            Self::Hold => "myna dictation (hold to talk)",
        }
    }
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

    /// Re-establish the binding the user asked for, without asking again.
    ///
    /// `ListShortcuts` first, and if that answers we are done. It usually does
    /// not: GNOME reports only what was bound *on this session*, never what it
    /// has stored, so the sole way to activate a stored binding is to issue
    /// `BindShortcuts` again - which is exactly the call that raises the
    /// "Add Keyboard Shortcuts" sheet when nothing is stored.
    ///
    /// [`consent`] is what separates those two cases. Bind only where the user
    /// has already been through that dialog once, because then the portal
    /// answers from its store and shows nothing; with no consent on record,
    /// refuse and say which command to run. That is the reported bug: a fresh
    /// install raised the sheet at every login, and dismissing it stored
    /// nothing, so the next login raised it again.
    #[cfg(not(test))]
    pub async fn attach(shortcut_id: &str, mode: ActivationMode) -> Result<Self, TriggerError> {
        let conn = crate::dbus::serve::connect_session()
            .await
            .map_err(|e| TriggerError::PortalUnavailable(e.to_string()))?;
        Self::attach_with_connection(conn, shortcut_id, mode).await
    }

    /// As [`Self::attach`] but on a caller-provided session-bus connection.
    #[cfg(not(test))]
    pub async fn attach_with_connection(
        conn: zbus::Connection,
        shortcut_id: &str,
        mode: ActivationMode,
    ) -> Result<Self, TriggerError> {
        Self::attach_with_connection_timeout(conn, shortcut_id, mode, BIND_TIMEOUT).await
    }

    /// As [`Self::attach_with_connection`] but with an explicit deadline on the
    /// re-bind, so `tests/portal_leak.rs` need not spend [`BIND_TIMEOUT`].
    #[cfg(not(test))]
    pub async fn attach_with_connection_timeout(
        conn: zbus::Connection,
        shortcut_id: &str,
        mode: ActivationMode,
        answer_within: std::time::Duration,
    ) -> Result<Self, TriggerError> {
        let (shortcuts, session) = open_session(&conn).await?;
        match Self::attach_on_session(
            &conn,
            &shortcuts,
            &session,
            shortcut_id,
            mode,
            answer_within,
        )
        .await
        {
            Ok(signals) => Ok(Self {
                signals,
                dedup: Dedup::with_mode(mode),
                _keepalive: Keepalive::Portal(shortcuts, session),
            }),
            Err(e) => {
                close_session(&session).await;
                Err(e)
            }
        }
    }
    #[cfg(not(test))]
    async fn attach_on_session(
        conn: &zbus::Connection,
        shortcuts: &ashpd::desktop::global_shortcuts::GlobalShortcuts,
        session: &ashpd::desktop::Session<ashpd::desktop::global_shortcuts::GlobalShortcuts>,
        shortcut_id: &str,
        mode: ActivationMode,
        answer_within: std::time::Duration,
    ) -> Result<BoxStream<'static, PortalSignal>, TriggerError> {
        let bound = list_shortcuts(shortcuts, session).await?;
        if let Some(shortcut) = bound.iter().find(|s| s.id() == shortcut_id) {
            let trigger = shortcut.trigger_description().to_string();
            let signals = Self::subscribe(conn, shortcuts, shortcut_id).await?;
            myna_core::info_log!(
                "portal",
                "attached '{shortcut_id}' ({trigger}); session live"
            );
            return Ok(signals);
        }

        if !consent::given() {
            return Err(TriggerError::NoShortcutBound(format!(
                "no '{shortcut_id}' binding, and none has been asked for"
            )));
        }

        // Consent on record, so the portal has a binding to answer from and
        // this is silent. If it is not - the user removed the shortcut in
        // Settings and is now looking at a sheet - that consent is spent, and
        // withdrawing it here is what stops the daemon asking again at every
        // login for the rest of the install's life.
        let result = Self::bind_on_session(
            conn,
            shortcuts,
            session,
            shortcut_id,
            None,
            mode,
            answer_within,
        )
        .await;
        if matches!(
            result,
            Err(TriggerError::BindRejected(_)) | Err(TriggerError::BindUnanswered(_))
        ) {
            consent::withdraw();
        }
        result
    }

    /// Create a portal session and bind `shortcut_id`, offering
    /// `preferred_trigger` to the portal's own confirm UI (FR-009).
    ///
    /// May raise the portal's sheet. [`Self::attach`] calls it only with
    /// [`consent`] on record, where the portal answers from its store instead.
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
        Self::bind_with_connection_timeout(conn, shortcut_id, preferred_trigger, mode, BIND_TIMEOUT)
            .await
    }

    /// As [`Self::bind_with_connection`] but with an explicit answer deadline.
    ///
    /// The deadline is a parameter only so the fake-portal suite
    /// (`tests/portal_leak.rs`) can exercise the abandon-and-clean-up path
    /// without spending [`BIND_TIMEOUT`] per attempt.
    #[cfg(not(test))]
    pub async fn bind_with_connection_timeout(
        conn: zbus::Connection,
        shortcut_id: &str,
        preferred_trigger: Option<&str>,
        mode: ActivationMode,
        answer_within: std::time::Duration,
    ) -> Result<Self, TriggerError> {
        let (shortcuts, session) = open_session(&conn).await?;

        // Past this point a portal session exists, and every failure below has
        // to hand it back: see `close_session`. Returning `?` straight out of
        // here is what left a session - and the unanswered sheet hanging off
        // it - on the desktop once per retry.
        match Self::bind_on_session(
            &conn,
            &shortcuts,
            &session,
            shortcut_id,
            preferred_trigger,
            mode,
            answer_within,
        )
        .await
        {
            Ok(signals) => Ok(Self {
                signals,
                dedup: Dedup::with_mode(mode),
                _keepalive: Keepalive::Portal(shortcuts, session),
            }),
            Err(e) => {
                close_session(&session).await;
                Err(e)
            }
        }
    }

    /// The half of the bind that runs with a live session in hand, split out
    /// so the caller above has exactly one place to clean up.
    #[cfg(not(test))]
    #[allow(clippy::too_many_arguments)]
    async fn bind_on_session(
        conn: &zbus::Connection,
        shortcuts: &ashpd::desktop::global_shortcuts::GlobalShortcuts,
        session: &ashpd::desktop::Session<ashpd::desktop::global_shortcuts::GlobalShortcuts>,
        shortcut_id: &str,
        preferred_trigger: Option<&str>,
        mode: ActivationMode,
        answer_within: std::time::Duration,
    ) -> Result<BoxStream<'static, PortalSignal>, TriggerError> {
        use ashpd::desktop::global_shortcuts::NewShortcut;

        let shortcut =
            NewShortcut::new(shortcut_id, mode.describe()).preferred_trigger(preferred_trigger);
        // `BindShortcuts` resolves on a `Response` *signal*, not on the method
        // reply, so no D-Bus call timeout applies and a backend that raises a
        // sheet nobody answers never returns. The bound wait is what makes
        // that recoverable; closing the session on the way out is what stops
        // the abandoned sheet from staying on the screen.
        let bound = tokio::time::timeout(
            answer_within,
            shortcuts.bind_shortcuts(session, &[shortcut], None, Default::default()),
        )
        .await;
        match bound {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => return Err(TriggerError::BindRejected(e.to_string())),
            Err(_) => {
                return Err(TriggerError::BindUnanswered(format!(
                    "no answer within {}s; closing the session so the sheet does not linger",
                    answer_within.as_secs()
                )));
            }
        }

        let signals = Self::subscribe(conn, shortcuts, shortcut_id).await?;
        myna_core::info_log!(
            "portal",
            "bound '{shortcut_id}' (preferred {}, {mode:?}); session live",
            preferred_trigger.unwrap_or("portal default")
        );
        Ok(signals)
    }

    /// Fold the two edge signals for `shortcut_id` into one stream, ending it
    /// when the portal we bound against stops being the portal.
    #[cfg(not(test))]
    async fn subscribe(
        conn: &zbus::Connection,
        shortcuts: &ashpd::desktop::global_shortcuts::GlobalShortcuts,
        shortcut_id: &str,
    ) -> Result<BoxStream<'static, PortalSignal>, TriggerError> {
        use futures_util::future;

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
        // The portal can restart under a long-lived daemon (a package upgrade,
        // a crash, `systemctl --user restart`). Its session dies with it, but
        // these are *bus-level* signal matches, so the streams above stay
        // happily open and this trigger would go on listening to a session
        // that no longer exists: the hotkey silently stops working and nothing
        // says so. Ending the stream when the portal's bus name changes owner
        // turns that into a plain rebind, which `retry::RetryingTrigger`
        // already knows how to do.
        let restarted = portal_owner_changed(conn).await?;
        Ok(stream::select(activated, deactivated)
            .take_until(restarted)
            .boxed())
    }
}

/// Raise the portal's own shortcut UI: bind `shortcut_id` if it is unbound,
/// otherwise ask the portal to show its rebind dialog for it.
///
/// The whole of `--bind-shortcut`. The binding outlives this process - the
/// portal keys it by app id, not by session - which is what lets the daemon
/// pick it up with [`GlobalShortcutTrigger::attach`].
#[cfg(not(test))]
pub async fn configure(
    shortcut_id: &str,
    preferred_trigger: Option<&str>,
    mode: ActivationMode,
) -> Result<Configured, TriggerError> {
    let conn = crate::dbus::serve::connect_session()
        .await
        .map_err(|e| TriggerError::PortalUnavailable(e.to_string()))?;
    let (shortcuts, session) = open_session(&conn).await?;
    let already = list_shortcuts(&shortcuts, &session)
        .await?
        .iter()
        .any(|s| s.id() == shortcut_id);

    let outcome = if already {
        shortcuts
            .configure_shortcuts(&session, None, Default::default())
            .await
            .map(|()| Configured::DialogOpened)
            .map_err(|e| TriggerError::BindRejected(e.to_string()))
    } else {
        use ashpd::desktop::global_shortcuts::NewShortcut;
        let shortcut =
            NewShortcut::new(shortcut_id, mode.describe()).preferred_trigger(preferred_trigger);
        match tokio::time::timeout(
            BIND_TIMEOUT,
            shortcuts.bind_shortcuts(&session, &[shortcut], None, Default::default()),
        )
        .await
        {
            Ok(Ok(req)) => req
                .response()
                .map(|r| {
                    Configured::Bound(
                        r.shortcuts()
                            .iter()
                            .filter(|s| s.id() == shortcut_id)
                            .map(|s| s.trigger_description().to_string())
                            .collect(),
                    )
                })
                .map_err(|e| TriggerError::BindRejected(e.to_string())),
            Ok(Err(e)) => Err(TriggerError::BindRejected(e.to_string())),
            Err(_) => Err(TriggerError::BindUnanswered(format!(
                "no answer within {}s",
                BIND_TIMEOUT.as_secs()
            ))),
        }
    };

    if outcome.is_ok() {
        consent::give();
    }
    close_session(&session).await;
    outcome
}

/// Whether the user has been through the portal's bind dialog for this app.
///
/// Not a cache of the binding - the portal owns that, and only it knows the
/// key. This records the one thing the portal will not tell us apart: whether
/// a `BindShortcuts` will be answered from the store or will put a dialog on
/// someone's screen. Without it the daemon cannot re-establish a binding at
/// login without also being the thing that begs for one.
#[cfg(not(test))]
pub mod consent {
    use std::path::PathBuf;

    /// `$SNAP_USER_COMMON` under confinement (survives refresh, shared by the
    /// snap's apps), the XDG state dir otherwise.
    fn path() -> Option<PathBuf> {
        if let Some(dir) = std::env::var_os("MYNA_STATE_DIR") {
            return Some(PathBuf::from(dir).join("shortcut-bound"));
        }
        if let Some(dir) = std::env::var_os("SNAP_USER_COMMON") {
            return Some(PathBuf::from(dir).join("shortcut-bound"));
        }
        let state = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
        Some(state.join("myna").join("shortcut-bound"))
    }

    pub fn given() -> bool {
        path().is_some_and(|p| p.exists())
    }

    pub fn give() {
        let Some(p) = path() else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(&p, b"") {
            myna_core::dbg_log!(
                "portal",
                "could not record the bind at {}: {e}",
                p.display()
            );
        }
    }

    pub fn withdraw() {
        let Some(p) = path() else { return };
        let _ = std::fs::remove_file(&p);
    }
}

/// What [`configure`] did.
#[derive(Debug)]
pub enum Configured {
    /// A new binding was made; the portal reported these triggers for it.
    Bound(Vec<String>),
    /// Already bound, so the portal's rebind dialog was raised instead.
    DialogOpened,
}

/// Open a GlobalShortcuts session, refusing to start a portal to do it.
#[cfg(not(test))]
async fn open_session(
    conn: &zbus::Connection,
) -> Result<
    (
        ashpd::desktop::global_shortcuts::GlobalShortcuts,
        ashpd::desktop::Session<ashpd::desktop::global_shortcuts::GlobalShortcuts>,
    ),
    TriggerError,
> {
    use ashpd::desktop::global_shortcuts::GlobalShortcuts;

    if !portal_is_up(conn).await? {
        return Err(TriggerError::PortalNotRunning(format!(
            "nothing owns {PORTAL_BUS_NAME}; waiting rather than starting one"
        )));
    }
    // Cloned, not moved: `portal_owner_changed` needs the same connection (a
    // zbus `Connection` clone is a handle to the one socket).
    let shortcuts = GlobalShortcuts::with_connection(conn.clone())
        .await
        .map_err(|e| TriggerError::PortalUnavailable(e.to_string()))?;
    let session = shortcuts
        .create_session(Default::default())
        .await
        .map_err(|e| TriggerError::PortalUnavailable(e.to_string()))?;
    Ok((shortcuts, session))
}

/// The bindings the portal already holds for this app.
///
/// Bounded like the bind is: `ListShortcuts` also resolves on a `Response`
/// signal, so a backend that takes the request and forgets it would otherwise
/// park the daemon's whole retry loop. Nobody is being asked anything here,
/// so the deadline is short.
#[cfg(not(test))]
async fn list_shortcuts(
    shortcuts: &ashpd::desktop::global_shortcuts::GlobalShortcuts,
    session: &ashpd::desktop::Session<ashpd::desktop::global_shortcuts::GlobalShortcuts>,
) -> Result<Vec<ashpd::desktop::global_shortcuts::Shortcut>, TriggerError> {
    match tokio::time::timeout(
        LIST_TIMEOUT,
        shortcuts.list_shortcuts(session, Default::default()),
    )
    .await
    {
        Ok(Ok(req)) => req
            .response()
            .map(|r| r.shortcuts().to_vec())
            .map_err(|e| TriggerError::PortalUnavailable(e.to_string())),
        Ok(Err(e)) => Err(TriggerError::PortalUnavailable(e.to_string())),
        Err(_) => Err(TriggerError::PortalUnavailable(format!(
            "ListShortcuts unanswered within {}s",
            LIST_TIMEOUT.as_secs()
        ))),
    }
}

/// The bus name the portal serves on; owning it is what makes a portal *the*
/// portal, so a change of owner is exactly "the portal I bound against is not
/// the portal any more".
#[cfg(not(test))]
const PORTAL_BUS_NAME: &str = "org.freedesktop.portal.Desktop";

/// How long one bind attempt may wait for the portal to answer.
///
/// `BindShortcuts` resolves on a `Response` signal rather than on the method
/// reply, so nothing underneath imposes a deadline: a backend that raises a
/// confirm sheet and gets no answer leaves the call pending for as long as the
/// daemon runs. Generous rather than tight, because the wait is legitimately a
/// human one - portal v1 has no persist token, so backends older than
/// xdg-desktop-portal-gnome 51 raise that sheet once per bind. What matters is
/// that it is finite, and that expiring it closes the session (see
/// [`close_session`]) instead of walking away from it.
#[cfg(not(test))]
pub const BIND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// How long a `ListShortcuts` may take. No human in the loop, so unlike
/// [`BIND_TIMEOUT`] this is a machine timeout.
#[cfg(not(test))]
const LIST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Hand a portal session back after a bind that did not work out.
///
/// `ashpd` only surfaces the `Request` object once the response arrives, so a
/// client that gives up waiting has no handle on the pending request and
/// cannot `Close` it directly. The session is the lever it does have: closing
/// it ends the backend's interaction for that session, which is what takes an
/// unanswered "Add Keyboard Shortcuts" sheet off the screen. Without this each
/// abandoned attempt left its sheet up and the next attempt raised another -
/// six of them stacked on an unattended machine in 47 minutes (reported
/// 2026-09-01).
///
/// Best-effort by construction: this runs on the failure path, and a portal
/// that just failed to answer a bind may equally fail to answer this. A
/// refusal here changes nothing the caller can act on, so it is logged and
/// dropped rather than replacing the error being reported.
#[cfg(not(test))]
async fn close_session(
    session: &ashpd::desktop::Session<ashpd::desktop::global_shortcuts::GlobalShortcuts>,
) {
    if let Err(e) = session.close().await {
        myna_core::dbg_log!("portal", "could not close the abandoned session: {e}");
    }
}

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

/// Wait for a *different* portal to show up, or give up after `limit`.
///
/// The counterpart to [`await_portal`] for the other half of the retry policy.
/// After a confirm sheet has been raised and did not become a binding, the
/// portal is up and staying up, so [`await_portal`] returns instantly and the
/// daemon would just re-raise the sheet. What genuinely warrants asking again
/// is a *new* backend - `xdg-desktop-portal` restarting, which is what a new
/// desktop session looks like from here. That is a signal on the bus, so wait
/// for it rather than putting the same question up on a timer.
///
/// Subscribe first, then wait: unlike [`await_portal`] there is no state to
/// re-check afterwards, because "changed owner" is the event itself.
#[cfg(not(test))]
pub async fn await_portal_change(limit: std::time::Duration) {
    use futures_util::future::{select, Either};

    let conn = match crate::dbus::serve::connect_session().await {
        Ok(conn) => conn,
        Err(_) => return tokio::time::sleep(limit).await,
    };
    let changed = async {
        let dbus = zbus::fdo::DBusProxy::new(&conn).await?;
        let mut changes = dbus
            .receive_name_owner_changed_with_args(&[(0, PORTAL_BUS_NAME)])
            .await?;
        // Both halves of a restart resolve this. Binding against a portal that
        // is still starting just fails, and that failure has its own backoff.
        if changes.next().await.is_some() {
            myna_core::info_log!("portal", "{PORTAL_BUS_NAME} changed owner; asking again");
            return Ok(());
        }
        Err(zbus::Error::InvalidReply)
    };
    futures_util::pin_mut!(changed);
    match select(changed, Box::pin(tokio::time::sleep(limit))).await {
        Either::Left((Ok(()), _)) | Either::Right(_) => {}
        // The watch broke; serve out the net rather than returning early and
        // spinning against a bus that just failed us.
        Either::Left((Err(e), net)) => {
            myna_core::dbg_log!(
                "portal",
                "cannot watch for a new portal ({e}); using the net"
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

    // The portal shows this string next to the key in Settings → Keyboard, so
    // it has to describe the gesture the daemon actually implements. A fixed
    // "hold to talk" told every Toggle user (the default) the wrong thing.
    #[test]
    fn description_matches_the_gesture_the_mode_implements() {
        assert!(ActivationMode::Toggle.describe().contains("tap"));
        assert!(ActivationMode::Hold.describe().contains("hold"));
        assert!(!ActivationMode::Toggle.describe().contains("hold"));
    }
}
