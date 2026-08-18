//! The desktop session controller (plan T21) — owns the multi-session
//! push-to-talk lifecycle, composing the three boundary seams
//! ([`Trigger`], [`Injector`], [`Indicator`]) over the *unchanged*
//! `myna-orchestrator` `run_dictation` session (capture-at-press,
//! push-gated-on-`ready`).
//!
//! It is the production analogue of `runner::run_dictation`, specialized for the
//! desktop: a persistent loop that, per hotkey Press, acquires the focused
//! target, runs one utterance, routes committed transcripts to the injector and
//! liveness to the indicator, and returns to Idle on Release / focus-loss /
//! terminal event — never capturing audio outside an active session
//! (push-to-talk, FR-004).
//!
//! Everything here is hermetic: the boundaries are trait objects, so tests drive
//! the whole lifecycle with mocks (no D-Bus / IBus / portal / display).

use futures_util::stream::{BoxStream, StreamExt};
use tokio::sync::mpsc;

use crate::indicator::{Indicator, IndicatorState};
use crate::inject::{FocusEvent, InjectError, Injector};
use async_trait::async_trait;
use myna_orchestrator::{
    BackendError, OrchestratorEvent, SessionOutcome, StopHandle, TextSink, Trigger, TriggerEdge,
};

// ── State model ───────────────────────────────────────────────────────────────

/// The controller's dictation state (data-model.md), carried into Rust from the
/// retired Python `DictationState` and extended with UD129's explicit
/// `Cancelled`/`Completed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictationState {
    /// No capture; waiting for a `Press`.
    Idle,
    /// Acquiring target + mic + inference session.
    Starting,
    /// Capturing; audio streaming; awaiting/receiving events.
    Recording,
    /// Inference decoding (may overlap Recording in streaming).
    Transcribing,
    /// `Release` seen; no new audio; awaiting the terminal event.
    Finalizing,
    /// Terminal event received; committed text done.
    Completed,
    /// Session aborted (focus lost / target gone / user cancel).
    Cancelled,
    /// Unrecoverable failure; user feedback owed.
    Error,
}

impl DictationState {
    /// Whether `self → to` is a legal transition (data-model.md state model).
    /// Anything else is a controller bug — [`advance`] panics on it, and the
    /// legal/illegal tables are asserted in tests (T005).
    pub fn can_transition(self, to: DictationState) -> bool {
        use DictationState::*;
        matches!(
            (self, to),
            (Idle, Starting)
                | (Starting, Recording)
                | (Starting, Cancelled)
                | (Starting, Error)
                | (Starting, Idle)
                | (Recording, Transcribing)
                | (Recording, Finalizing)
                | (Recording, Cancelled)
                | (Recording, Error)
                | (Transcribing, Recording)
                | (Transcribing, Finalizing)
                | (Transcribing, Cancelled)
                | (Transcribing, Error)
                | (Finalizing, Completed)
                | (Finalizing, Cancelled)
                | (Finalizing, Error)
                | (Completed, Idle)
                | (Cancelled, Idle)
                | (Error, Idle)
        )
    }
}

/// Advance `state` to `to`, panicking on an illegal transition (a controller
/// bug — the state model is a contract, not advice).
fn advance(state: &mut DictationState, to: DictationState) {
    assert!(
        state.can_transition(to),
        "illegal dictation-state transition {:?} → {:?}",
        *state,
        to
    );
    *state = to;
}

// ── OrchestratorEvent → IndicatorState mapping (T009) ──────────────────────────

/// Map an [`OrchestratorEvent`] to the [`IndicatorState`] it should show, or
/// `None` when the event drives no indicator change (commit-only text events).
///
/// `Loading`/`Ready` both show `Recording` (a cold load is "listening, warming
/// up"); `Done` hides the indicator unless nothing was captured (see
/// [`completion_indicator_state`]); an `Error` shows its message.
/// `Snippet`/`Final` carry transcript text and never touch the indicator
/// (privacy, N8). The `Finalizing` state is controller-driven — set on the
/// `Release`/focus-out edge, not derivable from an event — so it has no row
/// here (see [`DesktopController`]).
///
/// **`Transcribing` maps to `Recording` (listening).** Streaming / re-decode
/// adapters emit `transcribing` progress *while the key is still held and the
/// user is still speaking* — projecting that to the UI flips the indicator to
/// the "working" look mid-utterance, which reads wrong in push-to-talk. The
/// visible phase is trigger-driven: `Recording` (listening) while held,
/// `Finalizing` (finishing) after release. The internal
/// `DictationState::Transcribing` still advances (see [`route_event`]); it just
/// isn't projected to the indicator during capture. Lifecycle: Recording →
/// [release] → Finalizing → Hidden (or, since 2026-07-30, a recoverable
/// `notice` — see below).
///
/// `state` is the controller's *current* `DictationState` at the moment this
/// event is being routed (after any state advance `route_event` itself makes
/// for this same event). `Loading`/`Ready`/`Transcribing` only project
/// `Recording` while `state` is still `Recording` or `Transcribing` — i.e.
/// while actually still capturing. Regression (manual test report,
/// 2026-07-31): a `Transcribing` liveness ping can arrive in the event
/// channel just *after* a `Release`/`FocusOut` has already moved `state` to
/// `Finalizing` (the adapter's progress ping and the release edge race, and
/// `events_rx.recv()` is polled with priority over the trigger/focus edges —
/// see the `biased` select in [`DesktopController::run_one_utterance`]).
/// Without this guard, that stale ping unconditionally remapped to
/// `Recording`, briefly clobbering the correct `Finalizing` indicator state
/// with a spurious `finalizing → recording → idle` flicker at the *end* of
/// every utterance. `Loading`/`Ready` are guarded the same way — while less
/// likely to race this way in practice, the same staleness argument applies.
///
/// `focus_lost` should be `true` when this utterance is ending because the
/// injection target lost focus (`FocusEvent::FocusOut`) — it changes the
/// message [`completion_indicator_state`] picks for an empty transcript (see
/// there).
pub fn event_to_indicator(
    event: &OrchestratorEvent,
    state: DictationState,
    focus_lost: bool,
) -> Option<IndicatorState> {
    let still_listening = matches!(
        state,
        DictationState::Recording | DictationState::Transcribing
    );
    match event {
        OrchestratorEvent::Loading | OrchestratorEvent::Ready | OrchestratorEvent::Transcribing => {
            // Listening, not "working": stay on Recording while the user
            // speaks — but only while we ARE still listening (see doc
            // comment above); a stale post-release ping must not clobber
            // Finalizing (or any later state) with Recording.
            still_listening.then_some(IndicatorState::Recording)
        }
        OrchestratorEvent::Done(text) => Some(completion_indicator_state(text, focus_lost)),
        OrchestratorEvent::Error { message, .. } => Some(IndicatorState::critical(message.clone())),
        OrchestratorEvent::Snippet(_)
        | OrchestratorEvent::Final(_)
        | OrchestratorEvent::Unstable(_)
        | OrchestratorEvent::AudioDropped(_) => None,
    }
}

/// The indicator state for a completed session's transcript (feature 004,
/// 2026-07-30 HUD redesign, data-model E1a, research R13, contract C10/C11).
///
/// An empty/blank transcript means nothing was (usably) captured — a
/// **recoverable**, non-blocking issue, not a failure: the session completed
/// successfully, so this is NOT an `OrchestratorEvent::Error`. A non-empty
/// transcript hides the indicator exactly as before.
///
/// `focus_lost` distinguishes *why* the transcript is empty: when the
/// injection target lost focus mid-utterance (`FocusEvent::FocusOut`, see
/// [`DesktopController`]'s focus-loss handling) the session was deliberately
/// cut short, so "No speech detected" would misreport a focus change as
/// silence (manual test report, 2026-07-31); the message becomes "Focus lost"
/// instead. Without a focus-loss, an empty transcript means the user simply
/// didn't speak, so it stays "No speech detected".
///
/// This single helper is called from **both** the live per-event path
/// ([`event_to_indicator`]'s `Done` arm, above) and the finalize-block safety
/// net (this module's `Ok(SessionOutcome::Completed{transcript})` handler,
/// below) so the two can never disagree (C11) — whichever fires first
/// publishes the state; the other's call is a no-op under
/// `DbusIndicator::publish`'s existing per-wire-state dedup (C2). Both call
/// sites are threaded the same `focus_lost` value for the same utterance.
///
/// This is an interim, client-inferred classification, not a true wire-level
/// error disposition — that remains T31/T62's job (spec Assumptions).
pub fn completion_indicator_state(transcript: &str, focus_lost: bool) -> IndicatorState {
    if transcript.trim().is_empty() {
        if focus_lost {
            IndicatorState::recoverable("Focus lost")
        } else {
            IndicatorState::recoverable("No speech detected")
        }
    } else {
        IndicatorState::Hidden
    }
}

// ── Session seam ───────────────────────────────────────────────────────────────

/// A single running dictation utterance: the boxed future of
/// `run_dictation` (capture + inference), forwarding events on the channel the
/// factory was handed.
pub type SessionRun =
    futures_util::future::BoxFuture<'static, Result<SessionOutcome, BackendError>>;

/// Builds one dictation utterance per Press (fresh backend + capture source),
/// returning the running future and a [`StopHandle`] that ends capture early
/// (Release / focus-out → graceful finalize). The controller never starts a
/// session — hence never captures audio — outside a Press→Release window
/// (FR-004).
pub trait SessionFactory: Send {
    fn start(&mut self, events: mpsc::Sender<OrchestratorEvent>) -> (SessionRun, StopHandle);
}

impl<F> SessionFactory for F
where
    F: FnMut(mpsc::Sender<OrchestratorEvent>) -> (SessionRun, StopHandle) + Send,
{
    fn start(&mut self, events: mpsc::Sender<OrchestratorEvent>) -> (SessionRun, StopHandle) {
        (self)(events)
    }
}

/// A [`TextSink`] that forwards every orchestrator event onto a channel — the
/// adapter between `run_dictation` (which owns its sink) and the controller's
/// select loop (which routes events to the injector/indicator).
pub struct ChannelSink(pub mpsc::Sender<OrchestratorEvent>);

#[async_trait]
impl TextSink for ChannelSink {
    async fn emit(&mut self, event: OrchestratorEvent) {
        let _ = self.0.send(event).await;
    }
}

// ── Controller ─────────────────────────────────────────────────────────────────

/// The desktop session controller. Build with [`DesktopController::builder`].
pub struct DesktopController {
    trigger: Box<dyn Trigger>,
    injector: Box<dyn Injector>,
    indicator: Box<dyn Indicator>,
    session: Box<dyn SessionFactory>,
    state: DictationState,
    /// Opt-in (R9): route `Unstable` hypotheses to the injector's preedit
    /// region. Default false — commit-only (FR-012).
    preedit: bool,
}

/// Builder for [`DesktopController`] — injects the three boundaries + a session
/// factory (mocks in tests, real portal/IBus/GTK in the binary).
#[derive(Default)]
pub struct DesktopControllerBuilder {
    trigger: Option<Box<dyn Trigger>>,
    injector: Option<Box<dyn Injector>>,
    indicator: Option<Box<dyn Indicator>>,
    session: Option<Box<dyn SessionFactory>>,
    preedit: bool,
}

impl DesktopControllerBuilder {
    pub fn trigger(mut self, trigger: impl Trigger + 'static) -> Self {
        self.trigger = Some(Box::new(trigger));
        self
    }

    pub fn injector(mut self, injector: impl Injector + 'static) -> Self {
        self.injector = Some(Box::new(injector));
        self
    }

    pub fn indicator(mut self, indicator: impl Indicator + 'static) -> Self {
        self.indicator = Some(Box::new(indicator));
        self
    }

    pub fn session(mut self, session: impl SessionFactory + 'static) -> Self {
        self.session = Some(Box::new(session));
        self
    }

    /// Enable streaming preedit (R9): `Unstable` hypotheses are rendered in
    /// the target's preedit region (volatile, replaced per update, cleared by
    /// the next commit) when the injector `supports_preedit()`. Off by default
    /// — the commit-only guarantee (FR-012) holds unless explicitly relaxed.
    pub fn preedit(mut self, on: bool) -> Self {
        self.preedit = on;
        self
    }

    /// Finish the controller. Panics if any boundary is missing (a wiring bug).
    pub fn build(self) -> DesktopController {
        DesktopController {
            trigger: self.trigger.expect("DesktopController needs a Trigger"),
            injector: self.injector.expect("DesktopController needs an Injector"),
            indicator: self
                .indicator
                .expect("DesktopController needs an Indicator"),
            session: self
                .session
                .expect("DesktopController needs a SessionFactory"),
            state: DictationState::Idle,
            preedit: self.preedit,
        }
    }
}

impl DesktopController {
    pub fn builder() -> DesktopControllerBuilder {
        DesktopControllerBuilder::default()
    }

    /// The current dictation state (for tests / diagnostics).
    pub fn state(&self) -> DictationState {
        self.state
    }

    /// The persistent push-to-talk loop: await a `Press`, run one utterance,
    /// return to Idle, repeat. Ends when the trigger is exhausted at Idle
    /// (`None` — stdin EOF / shortcut unbound).
    pub async fn run(&mut self) {
        loop {
            // Idle: wait for a Press. Stray Releases are ignored; `None` quits.
            let pressed = loop {
                match self.trigger.next_edge().await {
                    Some(TriggerEdge::Press) => break true,
                    Some(TriggerEdge::Release) => continue,
                    None => break false,
                }
            };
            if !pressed {
                break;
            }
            self.run_one_utterance().await;
        }
    }

    /// Run exactly one Press→(Release|terminal|focus-loss) utterance.
    async fn run_one_utterance(&mut self) {
        advance(&mut self.state, DictationState::Starting);
        myna_core::dbg_log!("ctrl", "press: starting utterance");

        // Acquire the target focused *now*. Secure/no-target/unavailable →
        // surface an error and abort without ever capturing audio (FR-021/023).
        match self.injector.acquire().await {
            Ok(_target) => {}
            Err(err) => {
                self.abort_before_capture(err).await;
                return;
            }
        }

        // Own the focus stream so we can select on it while still driving the
        // injector (`commit`/`end`) — the stream is `'static`.
        let mut focus: BoxStream<'static, FocusEvent> = self.injector.focus_events();

        // Start the session (capture begins at press, inside the factory).
        let (events_tx, mut events_rx) = mpsc::channel::<OrchestratorEvent>(64);
        let (run, stop) = self.session.start(events_tx);

        advance(&mut self.state, DictationState::Recording);
        self.indicator.set_state(IndicatorState::Recording).await;
        self.injector.set_activity(true).await;

        // Reborrow disjoint fields as locals so the select loop can poll the
        // trigger/focus futures and route to the injector/indicator without
        // aliasing `self`.
        let injector = &mut self.injector;
        let indicator = &mut self.indicator;
        let trigger = &mut self.trigger;
        let state = &mut self.state;
        let preedit = self.preedit;

        tokio::pin!(run);
        let mut trigger_open = true;
        let mut focus_open = true;
        let mut quit_after = false;
        let mut cancelled = false;
        // After focus-loss we must not commit further text (it would land in the
        // now-focused wrong surface): finalize what's already committed, discard
        // the rest (FR-014/FR-022, SC-007). A normal Release does NOT suppress
        // (the commit-drain tail is still ours to insert).
        let mut commits_suppressed = false;
        // Set only by `FocusEvent::FocusOut` — distinguishes an empty
        // transcript caused by a deliberately-cut-short session (the target
        // field lost focus) from one where the user simply said nothing, so
        // the indicator can say "Focus lost" instead of misreporting it as
        // "No speech detected" (manual test report, 2026-07-31). Deliberately
        // NOT set by `TargetGone`, which already gets its own distinct
        // "dictation target closed" message via the `cancelled` terminal
        // branch below.
        let mut focus_lost = false;
        // Committed text not yet inserted. Consecutive `Final`s (a
        // commit-on-finalize adapter emits them in one burst) are coalesced
        // here and inserted as ONE `CommitText`: rapid successive IBus commits
        // race and only the last lands, so we join the burst. Spaced streaming
        // finals still flush individually (see `route_event`).
        let mut buffer = CommitBuffer::default();

        let outcome = loop {
            tokio::select! {
                // Biased: drain buffered orchestrator events (commit `Final`, drive
                // the indicator) before noticing a coincident Release/focus edge,
                // so liveness is never dropped and the indicator walks its states
                // in order even when a release arrives mid-stream.
                biased;
                Some(ev) = events_rx.recv() => {
                    route_event(
                        ev,
                        injector.as_mut(),
                        indicator.as_mut(),
                        state,
                        RouteFlags { commit_allowed: !commits_suppressed, focus_lost, preedit },
                        &mut buffer,
                    )
                    .await;
                }
                // Session finished: drain any still-buffered events, then return.
                result = &mut run => {
                    while let Some(ev) = events_rx.recv().await {
                        route_event(
                        ev,
                        injector.as_mut(),
                        indicator.as_mut(),
                        state,
                        RouteFlags { commit_allowed: !commits_suppressed, focus_lost, preedit },
                        &mut buffer,
                    )
                    .await;
                    }
                    break result;
                }
                // A focus-loss event: FocusOut finalizes safely (no more
                // commits); TargetGone cancels (discard uncommitted). Checked
                // before the trigger so focus-loss takes precedence (end safely).
                fe = focus.next(), if focus_open => match fe {
                    Some(FocusEvent::FocusOut) => {
                        myna_core::dbg_log!("ctrl", "FocusOut: suppressing further commits, finalizing");
                        stop.stop();
                        commits_suppressed = true;
                        focus_lost = true;
                        enter_finalizing(state, indicator.as_mut()).await;
                        // A lost target ends this utterance; leave later edges
                        // for the next session. We never read a matching edge
                        // off `trigger` for this utterance's end (unlike a
                        // normal Release, which IS that edge), so the
                        // trigger's own press/release parity would otherwise
                        // be left desynced from the controller's Idle state —
                        // resync it now so the next physical hotkey press
                        // delivers Press, not a swallowed stray Release
                        // (manual test report, 2026-07-31: "have to press the
                        // hotkey twice").
                        trigger.resync().await;
                        trigger_open = false;
                    }
                    Some(FocusEvent::TargetGone) => {
                        myna_core::dbg_log!("ctrl", "TargetGone: cancelling utterance");
                        stop.stop();
                        commits_suppressed = true;
                        cancelled = true;
                        // Same trigger-parity resync as FocusOut, above.
                        trigger.resync().await;
                        trigger_open = false;
                    }
                    None => focus_open = false,
                },
                // A trigger edge: `Release` finalizes (graceful stop); a `None`
                // means the trigger ended — stop capture and quit after.
                edge = trigger.next_edge(), if trigger_open => match edge {
                    Some(TriggerEdge::Release) => {
                        myna_core::dbg_log!("ctrl", "release: graceful stop, finalizing");
                        stop.stop();
                        enter_finalizing(state, indicator.as_mut()).await;
                        // Stop reading the trigger for this utterance: any
                        // further edges (the next push-to-talk cycle) belong to
                        // the next session, not this finalizing one.
                        trigger_open = false;
                    }
                    Some(TriggerEdge::Press) => {} // ignore an extra press while recording
                    None => {
                        trigger_open = false;
                        quit_after = true;
                        stop.stop();
                        enter_finalizing(state, indicator.as_mut()).await;
                    }
                },
            }
        };

        // Terminal disposition.
        if cancelled {
            myna_core::dbg_log!("ctrl", "utterance cancelled: dictation target closed");
            self.injector.cancel().await;
            self.indicator
                .set_state(IndicatorState::critical("dictation target closed"))
                .await;
            finalize_state(&mut self.state, DictationState::Cancelled);
        } else {
            match outcome {
                Ok(SessionOutcome::Completed { transcript }) => {
                    myna_core::dbg_log!("ctrl", "utterance completed");
                    ensure_finalizing(&mut self.state);
                    // Safety flush: normally the terminal `done` already flushed
                    // the buffered burst in `route_event` (leaving the buffer
                    // empty); this catches a completed run whose last event was
                    // a `Final` with nothing after it. Never double-commits
                    // (the flush takes the buffer), and discards rather than
                    // inserts when commits are suppressed.
                    buffer.flush(&mut *self.injector, !commits_suppressed).await;
                    self.injector.set_activity(false).await;
                    self.injector.end().await;
                    // C11: agrees with event_to_indicator's Done arm — both
                    // call completion_indicator_state (with the same
                    // focus_lost) so a Hidden vs. notice disagreement, or a
                    // "No speech detected" vs. "Focus lost" disagreement, can
                    // never happen; a redundant repeat here is a no-op under
                    // DbusIndicator::publish's dedup (C2).
                    self.indicator
                        .set_state(completion_indicator_state(&transcript, focus_lost))
                        .await;
                    finalize_state(&mut self.state, DictationState::Completed);
                }
                Ok(SessionOutcome::Aborted) => {
                    myna_core::dbg_log!("ctrl", "utterance aborted");
                    self.injector.cancel().await;
                    finalize_state(&mut self.state, DictationState::Cancelled);
                }
                Ok(SessionOutcome::Failed { message, .. }) => {
                    myna_core::dbg_log!("ctrl", "utterance FAILED: {message}");
                    self.injector.cancel().await;
                    self.indicator
                        .set_state(IndicatorState::critical(message))
                        .await;
                    finalize_state(&mut self.state, DictationState::Error);
                }
                Err(err) => {
                    myna_core::dbg_log!("ctrl", "utterance backend ERROR: {err}");
                    self.injector.cancel().await;
                    self.indicator
                        .set_state(IndicatorState::critical(err.to_string()))
                        .await;
                    finalize_state(&mut self.state, DictationState::Error);
                }
            }
        }

        advance(&mut self.state, DictationState::Idle);
        // Drain any hotkey pokes that queued while we were in Finalizing (with
        // the trigger paused): otherwise the outer `run()` loop would deliver
        // them one-by-one on next_edge(), each flipping the toggle and driving
        // a ghost Recording→Finalizing cycle per spam poke. No-op for
        // hold-to-talk triggers (portal / stdin) where every edge is real.
        self.trigger.discard_pending().await;
        let _ = quit_after; // the outer `run` loop re-reads the trigger (now None)
    }

    /// A pre-capture failure (secure field / no target / unreachable backend):
    /// show an error, release the engine defensively, never capture.
    async fn abort_before_capture(&mut self, err: InjectError) {
        let message = match &err {
            InjectError::SecureField => "refusing to type into a password field".to_string(),
            InjectError::NoTarget => "no text field is focused".to_string(),
            other => other.to_string(),
        };
        myna_core::dbg_log!("ctrl", "acquire failed, aborting before capture: {message}");
        self.injector.cancel().await; // idempotent; releases if anything stuck
        self.indicator
            .set_state(IndicatorState::critical(message))
            .await;
        advance(&mut self.state, DictationState::Error);
        advance(&mut self.state, DictationState::Idle);
    }
}

/// Enter `Finalizing` from an active state (idempotent — a no-op if already
/// finalizing or past it).
async fn enter_finalizing(state: &mut DictationState, indicator: &mut dyn Indicator) {
    if matches!(
        *state,
        DictationState::Recording | DictationState::Transcribing
    ) {
        advance(state, DictationState::Finalizing);
        indicator.set_state(IndicatorState::Finalizing).await;
    }
}

/// Ensure we are in `Finalizing` before completing (a clip that plays out
/// without an explicit Release still passes through Finalizing).
fn ensure_finalizing(state: &mut DictationState) {
    if matches!(
        *state,
        DictationState::Recording | DictationState::Transcribing
    ) {
        advance(state, DictationState::Finalizing);
    }
}

/// Move to a terminal state (`Completed`/`Cancelled`/`Error`) from wherever the
/// session ended, passing through `Finalizing` if still active.
fn finalize_state(state: &mut DictationState, terminal: DictationState) {
    match terminal {
        DictationState::Completed => {
            ensure_finalizing(state);
            advance(state, DictationState::Completed);
        }
        DictationState::Cancelled | DictationState::Error => {
            advance(state, terminal);
        }
        _ => unreachable!("finalize_state expects a terminal state"),
    }
}

/// Route one orchestrator event: buffer `Final` segments (commit-only — never
/// `Snippet`/`Unstable` text) for coalesced insertion, render `Unstable`
/// hypotheses via the injector's preedit region when the opt-in is on (R9), and
/// drive the indicator via [`event_to_indicator`]. Advances
/// `Recording → Transcribing` on the first decoding event.
///
/// Committed text is buffered in [`CommitBuffer`] rather than inserted immediately:
/// consecutive `Final`s (a commit-on-finalize adapter emits the whole utterance
/// as a back-to-back burst) are joined and flushed as ONE `CommitText`. This is
/// essential because rapid successive IBus commits race and only the last one
/// lands in the target — the "only the last bit gets inserted" bug. Any
/// non-`Final` event (a `done`, a liveness ping between spaced streaming finals)
/// first flushes the buffer, so spaced finals still insert promptly and in
/// order.
async fn route_event(
    event: OrchestratorEvent,
    injector: &mut dyn Injector,
    indicator: &mut dyn Indicator,
    state: &mut DictationState,
    flags: RouteFlags,
    buffer: &mut CommitBuffer,
) {
    let RouteFlags {
        commit_allowed,
        focus_lost,
        preedit,
    } = flags;
    // A non-Final event is a boundary: flush the buffered final burst as one
    // commit before handling it (so ordering with `done`/indicator holds).
    if !matches!(event, OrchestratorEvent::Final(_)) {
        buffer.flush(injector, commit_allowed).await;
    }

    if let OrchestratorEvent::Transcribing = event {
        if *state == DictationState::Recording {
            advance(state, DictationState::Transcribing);
        }
    }
    if let Some(indicator_state) = event_to_indicator(&event, *state, focus_lost) {
        indicator.set_state(indicator_state).await;
    }
    if let OrchestratorEvent::Final(text) = &event {
        // Commit-only: stable committed text is buffered; unstable `Snippet`
        // never is (FR-012). Suppressed after focus-loss so nothing lands in the
        // wrong surface (FR-014, SC-007).
        myna_core::dbg_log!(
            "inject",
            "final(len={}) buffered; commit_allowed={commit_allowed}",
            text.len()
        );
        if commit_allowed {
            buffer.push(text);
        }
    }
    if let OrchestratorEvent::Unstable(text) = &event {
        // Streaming preedit (R9, opt-in): show the volatile hypothesis in the
        // target's preedit region — replaced on each update, cleared by the
        // next `commit`. The flush above runs first, so any pending stable
        // burst is committed (which clears the old preedit) *before* the new
        // preedit tail is drawn after it. Never committed (FR-012); suppressed
        // with commits after focus-loss (FR-014); skipped unless enabled and
        // the backend has a real preedit region (`supports_preedit`).
        if preedit && commit_allowed && injector.supports_preedit() {
            myna_core::dbg_log!("inject", "preedit(len={})", text.len());
            injector.set_preedit(text).await;
        }
    }
}

/// The per-event routing decisions [`route_event`] needs, grouped so the
/// signature stays readable (and so no call site can transpose two bare bools).
#[derive(Clone, Copy)]
struct RouteFlags {
    /// Text may still be inserted. Cleared after focus-loss, when a commit
    /// would land in the wrong surface (FR-014, SC-007).
    commit_allowed: bool,
    /// This utterance is ending because the target lost focus - see the
    /// `focus_lost` local in `run_session` for why an empty transcript must be
    /// reported differently in that case.
    focus_lost: bool,
    /// Streaming-preedit opt-in (R9).
    preedit: bool,
}

/// Committed text buffered for coalesced insertion, for one utterance.
///
/// Consecutive `Final`s are joined here and inserted as ONE `CommitText`:
/// rapid successive IBus commits race and only the last one lands in the
/// target (the "only the last bit gets inserted" bug).
#[derive(Default)]
struct CommitBuffer {
    /// Committed text not yet inserted.
    pending: String,
    /// Whether this utterance has already inserted text. Streaming commits
    /// flush *separately* (spaced by liveness/unstable events), so a later
    /// flush needs a separator from the text already in the field.
    committed_any: bool,
}

impl CommitBuffer {
    /// Append stable committed text to the buffer.
    ///
    /// Whitespace-aware join: servers whose segments carry natural
    /// (leading-space) whitespace concatenate verbatim (contract I2);
    /// stripped-segment servers get a separator - never a double space.
    fn push(&mut self, text: &str) {
        if !self.pending.is_empty()
            && !self.pending.ends_with(char::is_whitespace)
            && !text.starts_with(char::is_whitespace)
        {
            self.pending.push(' ');
        }
        self.pending.push_str(text);
    }

    /// Insert the buffered text as a single `CommitText`, then clear the
    /// buffer. A no-op when empty; discards (does not insert) when commits are
    /// suppressed. A commit failure is best-effort.
    ///
    /// The separator from already-inserted text is prepended here, but only
    /// when the buffered text doesn't carry its own leading whitespace
    /// (contract I2 servers) - never a double space.
    async fn flush(&mut self, injector: &mut dyn Injector, commit_allowed: bool) {
        if self.pending.is_empty() {
            return;
        }
        if commit_allowed {
            let mut text = std::mem::take(&mut self.pending);
            if self.committed_any && !text.starts_with(char::is_whitespace) {
                text.insert(0, ' ');
            }
            match injector.commit(&text).await {
                Ok(()) => {
                    self.committed_any = true;
                    myna_core::dbg_log!("inject", "committed {} chars: {:?}", text.len(), text)
                }
                Err(e) => myna_core::dbg_log!("inject", "commit FAILED: {e}"),
            }
        } else {
            self.pending.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── T005: state-machine legality ─────────────────────────────────────────

    const ALL: [DictationState; 8] = [
        DictationState::Idle,
        DictationState::Starting,
        DictationState::Recording,
        DictationState::Transcribing,
        DictationState::Finalizing,
        DictationState::Completed,
        DictationState::Cancelled,
        DictationState::Error,
    ];

    /// The exact legal edge set from data-model.md.
    fn legal_edges() -> Vec<(DictationState, DictationState)> {
        use DictationState::*;
        vec![
            (Idle, Starting),
            (Starting, Recording),
            (Starting, Cancelled),
            (Starting, Error),
            (Starting, Idle),
            (Recording, Transcribing),
            (Recording, Finalizing),
            (Recording, Cancelled),
            (Recording, Error),
            (Transcribing, Recording),
            (Transcribing, Finalizing),
            (Transcribing, Cancelled),
            (Transcribing, Error),
            (Finalizing, Completed),
            (Finalizing, Cancelled),
            (Finalizing, Error),
            (Completed, Idle),
            (Cancelled, Idle),
            (Error, Idle),
        ]
    }

    #[test]
    fn every_legal_transition_is_accepted() {
        for (from, to) in legal_edges() {
            assert!(from.can_transition(to), "expected {from:?} → {to:?} legal");
        }
    }

    #[test]
    fn every_other_transition_is_rejected() {
        let legal = legal_edges();
        for &from in &ALL {
            for &to in &ALL {
                if !legal.contains(&(from, to)) {
                    assert!(
                        !from.can_transition(to),
                        "expected {from:?} → {to:?} to be illegal"
                    );
                }
            }
        }
    }

    #[test]
    #[should_panic(expected = "illegal dictation-state transition")]
    fn advance_panics_on_illegal_transition() {
        let mut s = DictationState::Idle;
        advance(&mut s, DictationState::Completed); // Idle → Completed is a bug
    }

    #[test]
    fn advance_applies_a_legal_transition() {
        let mut s = DictationState::Idle;
        advance(&mut s, DictationState::Starting);
        assert_eq!(s, DictationState::Starting);
    }

    // ── T009: OrchestratorEvent → IndicatorState mapping ──────────────────────

    #[test]
    fn loading_and_ready_map_to_recording() {
        assert_eq!(
            event_to_indicator(
                &OrchestratorEvent::Loading,
                DictationState::Recording,
                false
            ),
            Some(IndicatorState::Recording)
        );
        assert_eq!(
            event_to_indicator(&OrchestratorEvent::Ready, DictationState::Recording, false),
            Some(IndicatorState::Recording)
        );
    }

    #[test]
    fn transcribing_maps_to_recording_during_capture() {
        // Streaming/re-decode adapters emit `transcribing` while the key is held
        // and the user is still speaking; projecting the distinct "working" look
        // mid-utterance reads wrong. The indicator stays on Recording
        // (listening) during capture; the finishing look arrives only on the
        // release edge (`Finalizing`). Internal state still advances to
        // Transcribing (see `route_event`), it just isn't shown here.
        assert_eq!(
            event_to_indicator(
                &OrchestratorEvent::Transcribing,
                DictationState::Recording,
                false
            ),
            Some(IndicatorState::Recording)
        );
        assert_eq!(
            event_to_indicator(
                &OrchestratorEvent::Transcribing,
                DictationState::Transcribing,
                false
            ),
            Some(IndicatorState::Recording),
            "still listening once state has itself advanced to Transcribing"
        );
    }

    /// Regression (manual test report, 2026-07-31): a `Loading`/`Ready`/
    /// `Transcribing` liveness ping that arrives in the event channel AFTER a
    /// `Release`/`FocusOut` has already moved `state` to `Finalizing` (a real
    /// race — see the doc comment on `event_to_indicator`) must NOT clobber
    /// the correct `Finalizing` indicator with `Recording`. This was causing a
    /// spurious `finalizing → recording → idle` flicker at the end of every
    /// utterance (present even before the 2026-07-31 focus-loss/trigger-parity
    /// fixes — an independent, pre-existing bug).
    #[test]
    fn stale_liveness_events_after_finalizing_do_not_clobber_the_indicator() {
        for event in [
            OrchestratorEvent::Loading,
            OrchestratorEvent::Ready,
            OrchestratorEvent::Transcribing,
        ] {
            assert_eq!(
                event_to_indicator(&event, DictationState::Finalizing, false),
                None,
                "{event:?} arriving once Finalizing must not touch the indicator"
            );
        }
    }

    #[test]
    fn done_maps_to_hidden() {
        assert_eq!(
            event_to_indicator(
                &OrchestratorEvent::Done("all done".into()),
                DictationState::Finalizing,
                false
            ),
            Some(IndicatorState::Hidden)
        );
    }

    /// T013/C10 (2026-07-30): a `Done` with an empty/blank transcript maps to
    /// the recoverable notice, not `Hidden` — this is the live-event half of
    /// the dual-call-site agreement (see `completion_indicator_state` tests
    /// below and `tests/controller.rs` for the finalize-block half).
    #[test]
    fn done_with_empty_transcript_maps_to_recoverable_notice() {
        assert_eq!(
            event_to_indicator(
                &OrchestratorEvent::Done("".into()),
                DictationState::Finalizing,
                false
            ),
            Some(IndicatorState::recoverable("No speech detected"))
        );
        assert_eq!(
            event_to_indicator(
                &OrchestratorEvent::Done("   ".into()),
                DictationState::Finalizing,
                false
            ),
            Some(IndicatorState::recoverable("No speech detected")),
            "whitespace-only transcript counts as empty"
        );
    }

    /// Regression (manual test report, 2026-07-31): a `Done` with an empty
    /// transcript when the utterance ended via focus-loss must say "Focus
    /// lost", not "No speech detected" — the session was cut short, the user
    /// may well have been speaking.
    #[test]
    fn done_with_empty_transcript_and_focus_lost_maps_to_focus_lost_notice() {
        assert_eq!(
            event_to_indicator(
                &OrchestratorEvent::Done("".into()),
                DictationState::Finalizing,
                true
            ),
            Some(IndicatorState::recoverable("Focus lost"))
        );
    }

    /// A non-empty transcript hides the indicator regardless of focus_lost —
    /// text was successfully captured before the focus loss, so there's
    /// nothing to report.
    #[test]
    fn done_with_nonempty_transcript_hides_regardless_of_focus_lost() {
        assert_eq!(
            event_to_indicator(
                &OrchestratorEvent::Done("hello".into()),
                DictationState::Finalizing,
                true
            ),
            Some(IndicatorState::Hidden)
        );
    }

    /// T013: `completion_indicator_state` in isolation — empty/blank →
    /// recoverable notice, non-empty → Hidden.
    #[test]
    fn completion_indicator_state_splits_on_empty_transcript() {
        assert_eq!(
            completion_indicator_state("", false),
            IndicatorState::recoverable("No speech detected")
        );
        assert_eq!(
            completion_indicator_state("   ", false),
            IndicatorState::recoverable("No speech detected")
        );
        assert_eq!(
            completion_indicator_state("hello", false),
            IndicatorState::Hidden
        );
    }

    /// Regression (manual test report, 2026-07-31): `focus_lost` overrides
    /// the empty-transcript message.
    #[test]
    fn completion_indicator_state_focus_lost_overrides_empty_transcript_message() {
        assert_eq!(
            completion_indicator_state("", true),
            IndicatorState::recoverable("Focus lost")
        );
        assert_eq!(
            completion_indicator_state("   ", true),
            IndicatorState::recoverable("Focus lost"),
            "whitespace-only transcript still counts as empty"
        );
        assert_eq!(
            completion_indicator_state("hello", true),
            IndicatorState::Hidden,
            "captured text hides the indicator even if focus was later lost"
        );
    }

    #[test]
    fn error_maps_to_error_with_message() {
        assert_eq!(
            event_to_indicator(
                &OrchestratorEvent::Error {
                    code: "x".into(),
                    message: "boom".into()
                },
                DictationState::Recording,
                false
            ),
            Some(IndicatorState::critical("boom"))
        );
    }

    #[test]
    fn text_events_do_not_touch_the_indicator() {
        // Snippet/Final carry transcript text and must never drive the indicator
        // (privacy, N8); AudioDropped is a capture-side signal, not a UI state.
        assert_eq!(
            event_to_indicator(
                &OrchestratorEvent::Snippet("hi".into()),
                DictationState::Recording,
                false
            ),
            None
        );
        assert_eq!(
            event_to_indicator(
                &OrchestratorEvent::Final("hello".into()),
                DictationState::Recording,
                false
            ),
            None
        );
        assert_eq!(
            event_to_indicator(
                &OrchestratorEvent::AudioDropped(myna_orchestrator::DropReason::NotResident),
                DictationState::Recording,
                false
            ),
            None
        );
    }
}
