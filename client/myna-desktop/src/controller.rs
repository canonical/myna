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
/// up"); `Done` hides the indicator; an `Error` shows its message.
/// `Snippet`/`Final` carry transcript text and never touch the indicator
/// (privacy, N8). The `Finalizing` state is controller-driven — set on the
/// `Release`/focus-out edge, not derivable from an event — so it has no row
/// here (see [`DesktopController`]).
///
/// `Transcribing` also shows `Recording` (listening): in push-to-talk the model
/// streams / re-decodes *while the key is held and the user is still speaking*,
/// so a mid-capture decode event must NOT flip the user-facing indicator to a
/// "finishing / please wait" look — that belongs only *after* the release edge
/// (`Finalizing`). The visible phase is trigger-driven: Listening while held,
/// Finishing once released. (The internal `DictationState::Transcribing` still
/// advances in `route_event` to record that decoding began; it just isn't
/// projected to the indicator during capture.)
pub fn event_to_indicator(event: &OrchestratorEvent) -> Option<IndicatorState> {
    match event {
        OrchestratorEvent::Loading
        | OrchestratorEvent::Ready
        | OrchestratorEvent::Transcribing => Some(IndicatorState::Recording),
        OrchestratorEvent::Done(_) => Some(IndicatorState::Hidden),
        OrchestratorEvent::Error { message, .. } => Some(IndicatorState::Error(message.clone())),
        OrchestratorEvent::Snippet(_)
        | OrchestratorEvent::Final(_)
        | OrchestratorEvent::AudioDropped(_) => None,
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
}

/// Builder for [`DesktopController`] — injects the three boundaries + a session
/// factory (mocks in tests, real portal/IBus/GTK in the binary).
#[derive(Default)]
pub struct DesktopControllerBuilder {
    trigger: Option<Box<dyn Trigger>>,
    injector: Option<Box<dyn Injector>>,
    indicator: Option<Box<dyn Indicator>>,
    session: Option<Box<dyn SessionFactory>>,
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
        // Buffered committed text not yet inserted. Consecutive `Final`s (a
        // commit-on-finalize adapter emits them in one burst) are coalesced
        // here and inserted as ONE `CommitText`: rapid successive IBus commits
        // race and only the last lands, so we join the burst. Spaced streaming
        // finals still flush individually (see `route_event`).
        let mut pending = String::new();

        let outcome = loop {
            tokio::select! {
                // Biased: drain buffered orchestrator events (commit `Final`, drive
                // the indicator) before noticing a coincident Release/focus edge,
                // so liveness is never dropped and the indicator walks its states
                // in order even when a release arrives mid-stream.
                biased;
                Some(ev) = events_rx.recv() => {
                    route_event(ev, injector.as_mut(), indicator.as_mut(), state, !commits_suppressed, &mut pending).await;
                }
                // Session finished: drain any still-buffered events, then return.
                result = &mut run => {
                    while let Some(ev) = events_rx.recv().await {
                        route_event(ev, injector.as_mut(), indicator.as_mut(), state, !commits_suppressed, &mut pending).await;
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
                        enter_finalizing(state, indicator.as_mut()).await;
                        // A lost target ends this utterance; leave later edges
                        // for the next session.
                        trigger_open = false;
                    }
                    Some(FocusEvent::TargetGone) => {
                        myna_core::dbg_log!("ctrl", "TargetGone: cancelling utterance");
                        stop.stop();
                        commits_suppressed = true;
                        cancelled = true;
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
            self.injector.cancel().await;
            self.indicator
                .set_state(IndicatorState::Error("dictation target closed".into()))
                .await;
            finalize_state(&mut self.state, DictationState::Cancelled);
        } else {
            match outcome {
                Ok(SessionOutcome::Completed { .. }) => {
                    ensure_finalizing(&mut self.state);
                    // Safety flush: normally the terminal `done` already flushed
                    // the buffered burst in `route_event` (leaving `pending`
                    // empty); this catches a completed run whose last event was
                    // a `Final` with nothing after it. Never double-commits
                    // (the flush takes the buffer).
                    if !commits_suppressed && !pending.is_empty() {
                        myna_core::dbg_log!(
                            "inject",
                            "flushing {} buffered chars on complete",
                            pending.len()
                        );
                        let _ = self.injector.commit(&pending).await;
                    }
                    self.injector.set_activity(false).await;
                    self.injector.end().await;
                    self.indicator.set_state(IndicatorState::Hidden).await;
                    finalize_state(&mut self.state, DictationState::Completed);
                }
                Ok(SessionOutcome::Aborted) => {
                    self.injector.cancel().await;
                    finalize_state(&mut self.state, DictationState::Cancelled);
                }
                Ok(SessionOutcome::Failed { message, .. }) => {
                    self.injector.cancel().await;
                    self.indicator
                        .set_state(IndicatorState::Error(message))
                        .await;
                    finalize_state(&mut self.state, DictationState::Error);
                }
                Err(err) => {
                    self.injector.cancel().await;
                    self.indicator
                        .set_state(IndicatorState::Error(err.to_string()))
                        .await;
                    finalize_state(&mut self.state, DictationState::Error);
                }
            }
        }

        advance(&mut self.state, DictationState::Idle);
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
        self.injector.cancel().await; // idempotent; releases if anything stuck
        self.indicator
            .set_state(IndicatorState::Error(message))
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
/// `Snippet`) for coalesced insertion, and drive the indicator via
/// [`event_to_indicator`]. Advances `Recording → Transcribing` on the first
/// decoding event.
///
/// Committed text is buffered in `pending` rather than inserted immediately:
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
    commit_allowed: bool,
    pending: &mut String,
) {
    // A non-Final event is a boundary: flush the buffered final burst as one
    // commit before handling it (so ordering with `done`/indicator holds).
    if !matches!(event, OrchestratorEvent::Final(_)) {
        flush_commit(injector, pending, commit_allowed).await;
    }

    if let OrchestratorEvent::Transcribing = event {
        if *state == DictationState::Recording {
            advance(state, DictationState::Transcribing);
        }
    }
    if let Some(indicator_state) = event_to_indicator(&event) {
        indicator.set_state(indicator_state).await;
    }
    if let OrchestratorEvent::Final(text) = event {
        // Commit-only: stable committed text is buffered; unstable `Snippet`
        // never is (FR-012). Suppressed after focus-loss so nothing lands in the
        // wrong surface (FR-014, SC-007).
        myna_core::dbg_log!(
            "inject",
            "final(len={}) buffered; commit_allowed={commit_allowed}",
            text.len()
        );
        if commit_allowed {
            if !pending.is_empty() {
                pending.push(' ');
            }
            pending.push_str(&text);
        }
    }
}

/// Insert the buffered committed text as a single `CommitText`, then clear the
/// buffer. A no-op when empty; discards (does not insert) when commits are
/// suppressed. A commit failure is best-effort.
async fn flush_commit(injector: &mut dyn Injector, pending: &mut String, commit_allowed: bool) {
    if pending.is_empty() {
        return;
    }
    if commit_allowed {
        let text = std::mem::take(pending);
        match injector.commit(&text).await {
            Ok(()) => myna_core::dbg_log!("inject", "committed {} chars: {:?}", text.len(), text),
            Err(e) => myna_core::dbg_log!("inject", "commit FAILED: {e}"),
        }
    } else {
        pending.clear();
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
            event_to_indicator(&OrchestratorEvent::Loading),
            Some(IndicatorState::Recording)
        );
        assert_eq!(
            event_to_indicator(&OrchestratorEvent::Ready),
            Some(IndicatorState::Recording)
        );
    }

    #[test]
    fn transcribing_maps_to_recording_during_capture() {
        // Push-to-talk: a decode event that arrives while the key is held
        // (streaming / re-decode overlaps capture) keeps the indicator on
        // "listening" — the finishing look is release-driven (`Finalizing`),
        // not decode-driven.
        assert_eq!(
            event_to_indicator(&OrchestratorEvent::Transcribing),
            Some(IndicatorState::Recording)
        );
    }

    #[test]
    fn done_maps_to_hidden() {
        assert_eq!(
            event_to_indicator(&OrchestratorEvent::Done("all done".into())),
            Some(IndicatorState::Hidden)
        );
    }

    #[test]
    fn error_maps_to_error_with_message() {
        assert_eq!(
            event_to_indicator(&OrchestratorEvent::Error {
                code: "x".into(),
                message: "boom".into()
            }),
            Some(IndicatorState::Error("boom".into()))
        );
    }

    #[test]
    fn text_events_do_not_touch_the_indicator() {
        // Snippet/Final carry transcript text and must never drive the indicator
        // (privacy, N8); AudioDropped is a capture-side signal, not a UI state.
        assert_eq!(
            event_to_indicator(&OrchestratorEvent::Snippet("hi".into())),
            None
        );
        assert_eq!(
            event_to_indicator(&OrchestratorEvent::Final("hello".into())),
            None
        );
        assert_eq!(
            event_to_indicator(&OrchestratorEvent::AudioDropped(
                myna_orchestrator::DropReason::NotResident
            )),
            None
        );
    }
}
