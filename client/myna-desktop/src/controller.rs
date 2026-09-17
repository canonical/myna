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

use std::time::Duration;

use futures_util::stream::{BoxStream, StreamExt};
use gettextrs::gettext;
use myna_audio::AudioStats;
use tokio::sync::{mpsc, watch};

use crate::indicator::{Indicator, IndicatorState};
use crate::inject::{FocusEvent, InjectError, Injector, Target};
use crate::live::Live;
use crate::sound::{CueKind, NullSoundCuePlayer, SoundCuePlayer};
use async_trait::async_trait;
use myna_core::failure::{self, FailurePresentation};
use myna_orchestrator::{
    BackendError, OrchestratorEvent, SessionOutcome, StopHandle, TextSink, Trigger, TriggerEdge,
};

// ── Long-operation progress (US4, T066/T070, FR-027) ───────────────────────────

/// How long a cold model load (`Loading`, no `Ready` yet) may run before it
/// is surfaced as an actionable notice rather than silent, indefinite
/// "listening" (FR-027: "so a non-visual user can always distinguish work in
/// progress from a hang"). Not user-configurable (unlike
/// `silence_auto_stop_seconds`) — this is a fixed backstop, not a session
/// policy.
///
/// **Known scope decision**: FR-027 also describes a *periodic* non-visual
/// progress ping before this threshold — deliberately not implemented. A
/// repeat announcement of an unchanged state (there is no new
/// `IndicatorState` to move to while still waiting on `Ready`) would be
/// silently swallowed by `accessibility::AnnouncingIndicator`'s intentional
/// same-state dedup (added for US1 to fix a real double-`set_state` bug),
/// which has no "repeat this on purpose" escape hatch today. Adding one
/// would mean either a new `Indicator` trait method (rippling through every
/// implementor: `dbus`/`gtk`/`notify`/`mock`) or relaxing a dedup guarantee
/// other surfaces rely on — judged out of scope for this pass; tracked as a
/// follow-up in `docs/project-plan.md` rather than worked around here.
const MODEL_LOAD_THRESHOLD: Duration = Duration::from_secs(15);

/// Has `elapsed` (time since a `Loading` phase began, with no `Ready`/
/// terminal event since) crossed [`MODEL_LOAD_THRESHOLD`] (T066)? A pure
/// predicate over a plain `Duration` so it is hermetically testable without
/// a real or even a `tokio::time`-paused clock — the integration wiring
/// (`run_one_utterance`) is what supplies a real elapsed duration.
fn loading_exceeds_threshold(elapsed: Duration) -> bool {
    elapsed >= MODEL_LOAD_THRESHOLD
}

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
/// `delivery` is what became of this utterance's text (see [`Delivery`]): it
/// chooses the message [`completion_indicator_state`] shows.
pub fn event_to_indicator(
    event: &OrchestratorEvent,
    state: DictationState,
    delivery: Delivery,
    quality: InputQuality,
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
        OrchestratorEvent::Done(text) => Some(completion_indicator_state(text, delivery, quality)),
        OrchestratorEvent::Error { code, message } => Some(IndicatorState::from_failure(
            failure::lookup_by_code(code),
            Some(message),
        )),
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
/// `delivery` says whether the text reached the field, and why it may not
/// have. Text the target refused, or that was discarded once the target
/// stopped being ours, landed nowhere: the completion says "Focus lost"
/// however much was transcribed, because hiding the indicator would report an
/// insertion that never happened. An empty transcript after a focus loss
/// reads the same way: the session was deliberately cut short, so "No speech
/// detected" would misreport a focus change as silence (manual test report,
/// 2026-07-31). Without a loss, an empty transcript means the user simply
/// didn't speak, so it stays "No speech detected".
///
/// This single helper is called from **both** the live per-event path
/// ([`event_to_indicator`]'s `Done` arm, above) and the finalize-block safety
/// net (this module's `Ok(SessionOutcome::Completed{transcript})` handler,
/// below) so the two can never disagree (C11) — whichever fires first
/// publishes the state; the other's call is a no-op under
/// `DbusIndicator::publish`'s existing per-wire-state dedup (C2). Both call
/// sites are threaded the same `delivery` value for the same utterance.
///
/// `quality` is the capture's verdict on the input ([`input_quality`]): a
/// session that produced text over a noisy input still gets a recoverable
/// notice, so the user learns why the transcript is worse than it should be.
/// An empty transcript keeps its own, more actionable, message.
///
/// This is an interim, client-inferred classification, not a true wire-level
/// error disposition — that remains T31/T62's job (spec Assumptions).
pub fn completion_indicator_state(
    transcript: &str,
    delivery: Delivery,
    quality: InputQuality,
) -> IndicatorState {
    match (delivery, transcript.trim().is_empty()) {
        (Delivery::Dropped, _) | (Delivery::FocusLost, true) => {
            IndicatorState::recoverable(gettext("Focus lost"))
        }
        (_, true) => IndicatorState::recoverable(gettext("No speech detected")),
        _ if quality == InputQuality::Noisy => {
            IndicatorState::recoverable(gettext("Background noise is high"))
        }
        _ => IndicatorState::Hidden,
    }
}

/// What became of the text one utterance produced. The [`Target`] is the
/// authority on whether a write landed; this is the controller's record of
/// its answers, and the completion notice is drawn from it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Delivery {
    /// Everything the session produced was written into the target.
    #[default]
    Landed,
    /// Focus left the target; whatever it had already written stands.
    FocusLost,
    /// Committed text reached no field: the target refused it, or it was
    /// discarded once the target stopped being ours.
    Dropped,
}

// ── Input quality ─────────────────────────────────────────────────────────────

/// The capture's verdict on the microphone input, read off the stats tap once
/// a session has ended. Energy statistics only, never samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputQuality {
    Ok,
    /// The input's quietest moment is loud, or speech barely clears it:
    /// broadband noise that degrades every model's transcript.
    Noisy,
}

/// A noise floor above this (linear full scale, -50 dBFS) is noisy on its
/// own. A healthy headset sits near -80 dBFS; a laptop with its fan on near
/// -55 dBFS. Prototype calibration, from the HUD meter's headset baseline.
pub const NOISE_FLOOR_LIMIT: f32 = 0.003_16;

/// Speech must clear the floor by this ratio (15 dB) to count as clean.
pub const MIN_SPEECH_TO_NOISE: f32 = 5.6;

/// Classify a session's input from its final stats snapshot.
pub fn input_quality(stats: &AudioStats) -> InputQuality {
    if stats.noise_floor <= 0.0 {
        // Digital silence: a muted or absent input, which the empty
        // transcript already reports as "No speech detected".
        return InputQuality::Ok;
    }
    if stats.noise_floor > NOISE_FLOOR_LIMIT {
        return InputQuality::Noisy;
    }
    if stats.speech_level > 0.0 && stats.speech_level / stats.noise_floor < MIN_SPEECH_TO_NOISE {
        return InputQuality::Noisy;
    }
    InputQuality::Ok
}

/// The verdict for a session: `Ok` where there was no capture to judge, since
/// a closed tap still holds the default snapshot.
fn quality_of(stats: &watch::Receiver<AudioStats>) -> InputQuality {
    input_quality(&stats.borrow())
}

// ── Auto-stop ─────────────────────────────────────────────────────────────────

/// When the controller ends a session on its own. Toggle activation has no
/// release edge: a forgotten session would otherwise stream the room until
/// focus moves. Hold-to-talk keeps the key as the whole authority and runs
/// with the policy off.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AutoStop {
    /// Finalize once this much captured audio has passed since voice was
    /// last heard (or since the start, if it never was). Zero = never.
    pub silence: Duration,
}

impl AutoStop {
    /// Hold-to-talk: never.
    pub fn off() -> Self {
        Self::default()
    }

    /// Toggle: the user's silence timeout, zero = never.
    pub fn toggle(silence: Duration) -> Self {
        Self { silence }
    }
}

/// Whether `policy` says this session is over, given the latest stats.
/// Measured in captured audio, so a device that stops delivering (already
/// its own fault, in capture) cannot look like a silent user.
pub fn auto_stop_due(stats: &AudioStats, policy: AutoStop) -> bool {
    let since_voice = stats
        .captured
        .saturating_sub(stats.last_voice.unwrap_or(Duration::ZERO));
    policy.silence > Duration::ZERO && since_voice >= policy.silence
}

/// A tap for a session that never opened the microphone: never polled, and
/// its snapshot is the default, which classifies as `Ok`.
fn closed_tap() -> watch::Receiver<AudioStats> {
    watch::channel(AudioStats::default()).1
}

// ── Session seam ───────────────────────────────────────────────────────────────

/// A single running dictation utterance: the boxed future of
/// `run_dictation` (capture + inference), forwarding events on the channel the
/// factory was handed.
pub type SessionRun =
    futures_util::future::BoxFuture<'static, Result<SessionOutcome, BackendError>>;

/// One started utterance: the running future, the [`StopHandle`] that ends
/// capture early (Release / focus-out → graceful finalize), and the capture
/// stats tap when there is capture to observe (a session that failed before
/// opening the microphone has none).
pub struct Session {
    pub run: SessionRun,
    pub stop: StopHandle,
    pub stats: Option<watch::Receiver<AudioStats>>,
}

/// A session with nothing to observe, so every existing factory - and every
/// test that never looks at audio - keeps returning the bare pair.
impl From<(SessionRun, StopHandle)> for Session {
    fn from((run, stop): (SessionRun, StopHandle)) -> Self {
        Self {
            run,
            stop,
            stats: None,
        }
    }
}

/// Builds one dictation utterance per Press (fresh backend + capture source).
/// The controller never starts a session - hence never captures audio -
/// outside a Press→Release window (FR-004).
pub trait SessionFactory: Send {
    fn start(&mut self, events: mpsc::Sender<OrchestratorEvent>) -> Session;
}

impl<F, S> SessionFactory for F
where
    F: FnMut(mpsc::Sender<OrchestratorEvent>) -> S + Send,
    S: Into<Session>,
{
    fn start(&mut self, events: mpsc::Sender<OrchestratorEvent>) -> Session {
        (self)(events).into()
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

// ── Ending ────────────────────────────────────────────────────────────────────

/// Why one utterance stopped writing into its target, if anything has. The
/// [`Target`] itself is the authority on whether a write lands; this is the
/// controller's record of why it stopped asking, and it decides three things:
/// whether output is still attempted, what the completion says about the text
/// (see [`Ending::delivery`]), and whether the utterance ends cancelled.
///
/// The gravest reason wins (see [`Ending::or`]): once the target is gone it
/// stays gone, however the loss was first noticed. A normal Release ends
/// nothing - the commit-drain tail is still ours to insert.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
enum Ending {
    /// Still writing: the target owns the field.
    #[default]
    None,
    /// Focus left the target, so further text would land in the wrong surface
    /// (FR-014/FR-022, SC-007). Text dropped that way, and an empty transcript
    /// after it, read "Focus lost" rather than misreporting a cut-short
    /// session as silence (manual test report, 2026-07-31).
    FocusLost,
    /// The target's window is gone: cancel and say so. Deliberately not
    /// `FocusLost` - it has its own "dictation target closed" message.
    TargetGone,
}

impl Ending {
    /// Whether the target may still be written to.
    fn writes_allowed(self) -> bool {
        self == Ending::None
    }

    /// What the completion should say about this utterance's text, given
    /// whether any of it was dropped (see [`CommitBuffer::dropped`]). Dropped
    /// text outranks the reason the utterance ended: it reached no field
    /// however the target was lost.
    fn delivery(self, dropped: bool) -> Delivery {
        match (self, dropped) {
            (_, true) => Delivery::Dropped,
            (Ending::FocusLost, false) => Delivery::FocusLost,
            _ => Delivery::Landed,
        }
    }

    /// Record a reason for ending, keeping the gravest of the two.
    fn or(self, reason: Ending) -> Ending {
        self.max(reason)
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
    /// region. Default false — commit-only (FR-012). Live, because the user
    /// can change the streaming mode it follows from without restarting the
    /// daemon; read per event, so a change lands mid-utterance.
    preedit: Live<bool>,
    /// Policy-driven session end (see [`AutoStop`]). Live for the same
    /// reason: the silence timeout is a user setting.
    auto_stop: Live<AutoStop>,
    /// Optional sound cues (US3, T058, FR-010/011). Defaults to
    /// [`NullSoundCuePlayer`] (silent) for every builder call site that
    /// predates this feature or never opts in — `myna_desktop::sound`'s own
    /// gating (`GatedSoundCuePlayer` + `Preferences::sound_cues_enabled`)
    /// decides whether the *configured* player actually makes sound; this
    /// field only decides whether a player is wired in at all.
    sound: Box<dyn SoundCuePlayer>,
    /// The most recent `FailurePresentation`-backed notice/failure (US4,
    /// T065/T069, FR-026): a single in-memory slot so a user can still
    /// determine what happened even after a `Recoverable` notice
    /// auto-dismisses from the indicator. Set by [`Self::report_failure`];
    /// read by [`Self::last_notice`]. `None` until the first registry-backed
    /// failure of the process.
    last_notice: Option<&'static myna_core::failure::FailurePresentation>,
}

/// This session's accept-gate drop counts, published as they happen.
///
/// Cumulative per session, so a reader that samples late still sees the whole
/// utterance's total rather than whatever happened since it last looked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AudioDrops {
    pub not_resident: u64,
    pub not_active: u64,
}

impl AudioDrops {
    fn record(&mut self, reason: myna_orchestrator::DropReason) {
        match reason {
            myna_orchestrator::DropReason::NotResident => self.not_resident += 1,
            myna_orchestrator::DropReason::NotActive => self.not_active += 1,
        }
    }
}

/// Builder for [`DesktopController`] — injects the three boundaries + a session
/// factory (mocks in tests, real portal/IBus/GTK in the binary).
#[derive(Default)]
pub struct DesktopControllerBuilder {
    trigger: Option<Box<dyn Trigger>>,
    injector: Option<Box<dyn Injector>>,
    indicator: Option<Box<dyn Indicator>>,
    session: Option<Box<dyn SessionFactory>>,
    preedit: Live<bool>,
    auto_stop: Live<AutoStop>,
    sound: Option<Box<dyn SoundCuePlayer>>,
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
    ///
    /// Takes a plain `bool` where the answer is fixed (every test), or a
    /// [`Live<bool>`] where it can change under a running daemon.
    pub fn preedit(mut self, on: impl Into<Live<bool>>) -> Self {
        self.preedit = on.into();
        self
    }

    /// End sessions by policy (the silence timeout). Off by default,
    /// which is hold-to-talk's contract and what every existing test expects.
    pub fn auto_stop(mut self, policy: impl Into<Live<AutoStop>>) -> Self {
        self.auto_stop = policy.into();
        self
    }

    /// Wire in a sound-cue player (US3, T058). Optional — omitting this call
    /// leaves cues silent ([`NullSoundCuePlayer`]), which is exactly the
    /// pre-feature behavior every existing call site keeps unless it opts in.
    pub fn sound(mut self, sound: impl SoundCuePlayer + 'static) -> Self {
        self.sound = Some(Box::new(sound));
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
            auto_stop: self.auto_stop,
            sound: self.sound.unwrap_or_else(|| Box::new(NullSoundCuePlayer)),
            last_notice: None,
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

    /// The most recent `FailurePresentation`-backed notice/failure (US4,
    /// T065, FR-026): remains `Some` after a `Recoverable` notice has
    /// auto-dismissed from the indicator (or a `Critical` one has been
    /// acknowledged), so a caller can still determine what happened.
    pub fn last_notice(&self) -> Option<&'static myna_core::failure::FailurePresentation> {
        self.last_notice
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
        myna_core::info_log!("ctrl", "press: starting utterance");

        // Acquire the target focused *now*. Secure/no-target/unavailable →
        // surface an error and abort without ever capturing audio (FR-021/023).
        let mut target = match self.injector.acquire().await {
            Ok(target) => target,
            Err(err) => {
                self.abort_before_capture(err).await;
                return;
            }
        };

        // Own the focus stream so we can select on it while still driving the
        // target (`commit`/`release`) - the stream is `'static`.
        let mut focus: BoxStream<'static, FocusEvent> = target.focus_events();

        // Start the session (capture begins at press, inside the factory).
        let (events_tx, mut events_rx) = mpsc::channel::<OrchestratorEvent>(64);
        let Session { run, stop, stats } = self.session.start(events_tx);
        // Policed only while capture is live; a session that never opened the
        // microphone has nothing to police and must not even wake the loop.
        let (mut stats, mut stats_open) = match stats {
            Some(stats) => (stats, true),
            None => (closed_tap(), false),
        };

        advance(&mut self.state, DictationState::Recording);
        self.indicator.set_state(IndicatorState::Recording).await;
        // US3/T058 (FR-010): the session-start cue. `play()` is a plain,
        // synchronous fn (not `async fn` — see `sound::SoundCuePlayer`'s doc
        // comment), so this call is structurally incapable of delaying
        // capture start via an accidental `.await` (FR-011).
        self.sound.play(CueKind::SessionStart);

        // Reborrow disjoint fields as locals so the select loop can poll the
        // trigger/focus futures and route to the target/indicator without
        // aliasing `self`.
        let supports_preedit = self.injector.supports_preedit();
        let indicator = &mut self.indicator;
        let trigger = &mut self.trigger;
        let state = &mut self.state;
        let sound = &mut self.sound;
        // Cloned handle, not a value: read at each event below so a settings
        // change mid-utterance is honored by the next hypothesis rather than
        // at the next press.
        let preedit = self.preedit.clone();
        let auto_stop = self.auto_stop.clone();

        tokio::pin!(run);
        let mut trigger_open = true;
        let mut focus_open = true;
        let mut events_open = true;
        // Why this utterance stopped writing, once something has (see
        // [`Ending`]).
        let mut ending = Ending::None;
        // Per session: a cold-start burst of pre-ready drops is normal, and a
        // count carried over from the last utterance would read as this one's.
        let mut drops = AudioDrops::default();
        // Committed text not yet inserted. Consecutive `Final`s (a
        // commit-on-finalize adapter emits them in one burst) are coalesced
        // here and inserted as ONE `CommitText`: rapid successive IBus commits
        // race and only the last lands, so we join the burst. Spaced streaming
        // finals still flush individually (see `route_event`).
        let mut buffer = CommitBuffer::default();
        // The session's result, once it has one. Its queued events are drained
        // by this same loop afterwards, so the focus arm still guards every
        // write; the trigger and stats arms shut off instead, because an edge
        // arriving during the drain belongs to the next utterance.
        let mut done: Option<Result<SessionOutcome, BackendError>> = None;
        // FR-027/T066/T070: when the current `Loading` phase started, so the
        // watchdog branch below knows how long we've been waiting for
        // `Ready`. Cleared on `Ready`/`Done`/`Error`, and also the moment we
        // stop actively recording (`Release`/`FocusOut`/`TargetGone`/the
        // trigger ending) — once the user isn't holding the key anymore,
        // "the model is still loading" is moot; finalizing is what's
        // proceeding. `None` whenever we're not in a load window at all
        // (including the entire rest of a warm-model utterance, where
        // `Loading` never fires).
        let mut loading_since: Option<tokio::time::Instant> = None;
        // Set once the threshold fires for this utterance, so the watchdog
        // branch below is a one-shot per utterance, not a repeat every poll
        // (an `IndicatorState` transition already latches visibly/audibly;
        // re-firing it every loop iteration once the deadline has passed
        // would just resend the identical notice). Read back into
        // `self.last_notice` after the loop (see below) — the loop only
        // reborrows `self.indicator`, not `self`, so it cannot call
        // `self.report_failure` directly.
        let mut model_load_slow: Option<&'static FailurePresentation> = None;

        let outcome = loop {
            // Both sides are finished: the session has its result and its
            // event queue is closed and empty.
            if !events_open {
                if let Some(result) = done.take() {
                    break result;
                }
            }
            // A one-shot deadline for the FR-027 "model load is taking a
            // while" notice: `Some` only while a `Loading` window is open
            // and hasn't already fired. Rebuilt fresh each iteration (cheap,
            // and the standard shape for an optional timer inside
            // `loop { select! {..} }` — the *absolute* deadline is stable
            // across iterations, so this doesn't restart the wait).
            let load_deadline = loading_since
                .filter(|_| model_load_slow.is_none())
                .map(|since| since + MODEL_LOAD_THRESHOLD);

            tokio::select! {
                biased;
                // `FocusOut`/`TargetGone` must be observed before the trigger's
                // next edge: the `focus_out_protection_holds_for_every_utterance`
                // regression showed a second `Press` being consumed inside the
                // first utterance when the order was random, losing the next
                // utterance's `Press`.  Biasing `focus` → `trigger` preserves
                // that precedence without re-introducing the original portal
                // latency bug (which was `events_rx` starving `trigger` when
                // `events_rx` was first).  Now `focus`/`trigger` are checked
                // before the hot `events_rx` (Transcribing pings at 15-20 Hz),
                // so press→release latency is not starved, while the
                // `still_listening` guard in `event_to_indicator` handles stale
                // post-release pings without requiring event ordering.
                fe = focus.next(), if focus_open => match fe {
                    Some(FocusEvent::FocusOut) => {
                        myna_core::info_log!("ctrl", "FocusOut: suppressing further commits, finalizing");
                        stop.stop();
                        ending = ending.or(Ending::FocusLost);
                        loading_since = None;
                        enter_finalizing(state, indicator.as_mut()).await;
                        // US3/T058 (FR-010): the immediate "stopped
                        // listening, now processing" cue — distinct from the
                        // eventual SessionEnd/Failure outcome cue, which
                        // fires later once the result is known.
                        sound.play(CueKind::StopListening);
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
                        myna_core::info_log!("ctrl", "TargetGone: cancelling utterance");
                        stop.stop();
                        ending = ending.or(Ending::TargetGone);
                        loading_since = None;
                        // Same trigger-parity resync as FocusOut, above.
                        trigger.resync().await;
                        trigger_open = false;
                    }
                    None => focus_open = false,
                },
                // A trigger edge: `Release` finalizes (graceful stop); a `None`
                // means the trigger ended — stop capture and quit after.
                edge = trigger.next_edge(), if trigger_open && done.is_none() => match edge {
                    Some(TriggerEdge::Release) => {
                        myna_core::info_log!("ctrl", "release: graceful stop, finalizing");
                        stop.stop();
                        loading_since = None;
                        enter_finalizing(state, indicator.as_mut()).await;
                        // US3/T058 (FR-010): see the FocusOut branch above —
                        // same immediate acknowledgment cue, same rationale.
                        sound.play(CueKind::StopListening);
                        // Stop reading the trigger for this utterance: any
                        // further edges (the next push-to-talk cycle) belong to
                        // the next session, not this finalizing one.
                        trigger_open = false;
                    }
                    Some(TriggerEdge::Press) => {} // ignore an extra press while recording
                    None => {
                        trigger_open = false;
                        stop.stop();
                        loading_since = None;
                        enter_finalizing(state, indicator.as_mut()).await;
                        sound.play(CueKind::StopListening);
                    }
                },
                // A fresh stats snapshot: the policy's chance to end a toggle
                // session the user walked away from. Ends exactly like a
                // Release, plus the trigger-parity resync a FocusOut needs,
                // because no edge was read off the trigger for this end.
                changed = stats.changed(), if stats_open && done.is_none() => {
                    if changed.is_err() {
                        stats_open = false;
                    } else if auto_stop_due(&stats.borrow(), auto_stop.get()) {
                        myna_core::info_log!("ctrl", "silence timeout: graceful stop, finalizing");
                        stop.stop();
                        enter_finalizing(state, indicator.as_mut()).await;
                        trigger.resync().await;
                        trigger_open = false;
                        stats_open = false;
                    }
                }
                // Before the queue, so the arms above are already shut off by
                // the time the drain below runs: an edge that arrives once the
                // session is over belongs to the next utterance.
                result = &mut run, if done.is_none() => done = Some(result),
                // Queued events, before and after the session finishes: the
                // drain is this same arm, so a focus loss is still seen first.
                ev = events_rx.recv(), if events_open => match ev {
                    Some(ev) => {
                        // Peek before `route_event` consumes `ev` (T066/T070):
                        // track the Loading→Ready window this utterance is in.
                        match &ev {
                            OrchestratorEvent::Loading => {
                                loading_since.get_or_insert_with(tokio::time::Instant::now);
                            }
                            OrchestratorEvent::Ready
                            | OrchestratorEvent::Done(_)
                            | OrchestratorEvent::Error { .. } => {
                                loading_since = None;
                            }
                            _ => {}
                        }
                        ending = route_event(
                            ev,
                            target.as_mut(),
                            indicator.as_mut(),
                            state,
                            RouteFlags {
                                ending,
                                preedit: preedit.get() && supports_preedit,
                                quality: quality_of(&stats),
                            },
                            &mut buffer,
                            &mut drops,
                        )
                        .await;
                    }
                    // The session dropped its sender: nothing more can arrive.
                    None => events_open = false,
                },
                // FR-027/T066/T070: the model load is taking longer than
                // `MODEL_LOAD_THRESHOLD` with no `Ready` yet — surface an
                // actionable notice so a non-visual user can tell "still
                // loading" from "hung" (visually, `Recording` is shown
                // throughout both phases identically — see
                // `event_to_indicator`'s `Loading`/`Ready` arm — so this is
                // the only signal a non-visual user gets here). Lowest
                // select priority: a real event/edge always wins a tie.
                // One-shot per utterance (see `model_load_slow` above); the
                // `if` guard also disables this branch entirely once there
                // is no open `Loading` window, so `sleep_until` is never
                // polled needlessly.
                () = tokio::time::sleep_until(load_deadline.unwrap_or_else(tokio::time::Instant::now)), if load_deadline.is_some() => {
                    // Defensive consistency check between the deadline this
                    // branch just woke up for and `loading_exceeds_threshold`
                    // (T066's hermetically-tested pure predicate) — the two
                    // must agree, or the `sleep_until` deadline arithmetic
                    // above has drifted from the threshold it's meant to
                    // implement.
                    debug_assert!(loading_since
                        .map(|since| loading_exceeds_threshold(since.elapsed()))
                        .unwrap_or(false));
                    let presentation = failure::lookup(failure::MODEL_LOAD_SLOW)
                        .expect("MODEL_LOAD_SLOW must be registered by default_registry");
                    myna_core::info_log!("ctrl", "model load exceeded {MODEL_LOAD_THRESHOLD:?} with no Ready yet");
                    let state_update = IndicatorState::from_failure(presentation, None);
                    if let IndicatorState::Error { message, .. } = &state_update {
                        eprintln!("myna-desktop: {message}");
                    }
                    indicator.set_state(state_update).await;
                    model_load_slow = Some(presentation);
                }
            }
        };

        // Safety flush: normally the terminal `done` already flushed the
        // buffered burst in `route_event` (leaving the buffer empty); this
        // catches a completed run whose last event was a `Final` with nothing
        // after it. Never double-commits (the flush takes the buffer). It is
        // the one write no focus poll follows, so a target that refuses it is
        // the only signal that the text never landed. A flush the ending
        // disallows inserts nothing and records the drop.
        if matches!(outcome, Ok(SessionOutcome::Completed { .. })) {
            ending = ending.or(buffer.flush(target.as_mut(), ending.writes_allowed()).await);
        }
        // One owner, one release: every terminal path gives the target up
        // here, exactly once, before the outcome is reported.
        target.release().await;

        // T065/T069: the loop above only reborrows `self.indicator`, not
        // `self`, so the "model load slow" branch couldn't update
        // `self.last_notice` directly — do it now that the loop (and its
        // reborrows) has ended. A later failure this same utterance (below)
        // overwrites it, same as any other `report_failure` call would.
        if let Some(presentation) = model_load_slow {
            self.last_notice = Some(presentation);
        }

        // Terminal disposition.
        if ending == Ending::TargetGone {
            myna_core::info_log!("ctrl", "utterance cancelled: dictation target closed");
            self.report_failure(
                failure::lookup(failure::TARGET_CLOSED)
                    .expect("TARGET_CLOSED must be registered by default_registry"),
                None,
            )
            .await;
            finalize_state(&mut self.state, DictationState::Cancelled);
        } else {
            match outcome {
                Ok(SessionOutcome::Completed { transcript }) => {
                    myna_core::info_log!("ctrl", "utterance completed");
                    ensure_finalizing(&mut self.state);
                    // C11: agrees with event_to_indicator's Done arm — both
                    // call completion_indicator_state (with the same
                    // focus-loss verdict) so a Hidden vs. notice disagreement,
                    // or a "No speech detected" vs. "Focus lost"
                    // disagreement, can never happen; a redundant repeat here
                    // is a no-op under DbusIndicator::publish's dedup (C2).
                    self.indicator
                        .set_state(completion_indicator_state(
                            &transcript,
                            ending.delivery(buffer.dropped()),
                            quality_of(&stats),
                        ))
                        .await;
                    // US3/T058 (FR-010): the session-end cue, on every
                    // successful completion regardless of whether anything
                    // was actually captured (an empty transcript still ends
                    // the session the user started) — distinct from the
                    // `Failure` cue below, which is reserved for a genuine
                    // error.
                    self.sound.play(CueKind::SessionEnd);
                    finalize_state(&mut self.state, DictationState::Completed);
                }
                Ok(SessionOutcome::Aborted) => {
                    myna_core::info_log!("ctrl", "utterance aborted");
                    finalize_state(&mut self.state, DictationState::Cancelled);
                    // The Press that opened this utterance may never have been
                    // answered by a Release read off the trigger (the abort is
                    // not itself a release edge e.g. target-gone); resync so
                    // the next toggle is a fresh Press, not a stray Release.
                    self.trigger.resync().await;
                }
                Ok(SessionOutcome::Failed { code, message }) => {
                    myna_core::info_log!("ctrl", "utterance FAILED: {message}");
                    self.report_failure(failure::lookup_by_code(&code), Some(&message))
                        .await;
                    self.sound.play(CueKind::Failure);
                    finalize_state(&mut self.state, DictationState::Error);
                    // A hard failure is not a Release edge: the toggle's Press
                    // was consumed with no matching Release, so resync or the
                    // next toggle reads as a swallowed Release (need two
                    // toggles to restart). Same as the FocusOut/TargetGone
                    // resync below (manual test report, 2026-07-31).
                    self.trigger.resync().await;
                }
                Err(err) => {
                    myna_core::info_log!("ctrl", "utterance backend ERROR: {err}");
                    let (presentation, detail) =
                        myna_orchestrator::backend_error_presentation(&err);
                    self.report_failure(presentation, detail.as_deref()).await;
                    self.sound.play(CueKind::Failure);
                    finalize_state(&mut self.state, DictationState::Error);
                    // Same toggle-parity fix as the Failed branch above.
                    self.trigger.resync().await;
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
    }

    /// A pre-capture failure (secure field / no target / unreachable backend):
    /// show an error, never capture. `acquire` already rolled back.
    async fn abort_before_capture(&mut self, err: InjectError) {
        let (presentation, detail) = inject_error_presentation(&err);
        myna_core::info_log!(
            "ctrl",
            "acquire failed, aborting before capture: {}",
            presentation.message
        );
        self.report_failure(presentation, detail.as_deref()).await;
        self.sound.play(CueKind::Failure);
        advance(&mut self.state, DictationState::Error);
        // A pre-capture abort is not a Release edge — the toggle's Press was
        // consumed and never matched, so resync or the next toggle reads as a
        // swallowed Release and the user needs two toggles to restart.
        self.trigger.resync().await;
        advance(&mut self.state, DictationState::Idle);
    }

    /// Surface a `FailurePresentation`-backed failure (US4, T068/T069,
    /// FR-024): resolves to the exact same fixed message everywhere this
    /// presentation is looked up (F2/F3), on the indicator AND recorded as
    /// the "last notice" (FR-026) so it remains retrievable after a
    /// `Recoverable` notice auto-dismisses. The stderr copy matters because
    /// the indicator can be invisible (`--dbus` mode only updates
    /// `org.myna.Dictation` properties, which nothing renders unless the
    /// myna-shell extension is installed - the 2026-08-18 silent-death
    /// debug session). `detail` is optional dynamic context (never primary
    /// text - see `IndicatorState::from_failure`).
    async fn report_failure(
        &mut self,
        presentation: &'static FailurePresentation,
        detail: Option<&str>,
    ) {
        let state = IndicatorState::from_failure(presentation, detail);
        if let IndicatorState::Error { message, .. } = &state {
            eprintln!("myna-desktop: {message}");
        }
        self.indicator.set_state(state).await;
        self.last_notice = Some(presentation);
    }
}

/// Map an [`InjectError`] to its registered [`FailurePresentation`] plus any
/// dynamic detail the variant carries (US4, T067/T068; contract F1: "every
/// known failure source" explicitly includes these four variants).
fn inject_error_presentation(err: &InjectError) -> (&'static FailurePresentation, Option<String>) {
    let lookup = |id: &str| {
        failure::lookup(id).unwrap_or_else(|| panic!("{id} must be registered by default_registry"))
    };
    match err {
        InjectError::SecureField => (lookup(failure::SECURE_FIELD), None),
        InjectError::NoTarget => (lookup(failure::NO_TARGET), None),
        // The field we were handed stopped being ours before capture began.
        // `TARGET_CLOSED` is the registered presentation for exactly this —
        // its copy reads "closed or lost focus" and its recovery action is
        // "click back into a text field" — so a pre-capture focus loss and a
        // mid-utterance one tell the user the same thing.
        InjectError::FocusLost => (lookup(failure::TARGET_CLOSED), None),
        InjectError::Unavailable(detail) => {
            (lookup(failure::INJECTION_UNAVAILABLE), Some(detail.clone()))
        }
        InjectError::Backend(detail) => (
            lookup(failure::INJECTION_BACKEND_ERROR),
            Some(detail.clone()),
        ),
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
///
/// Returns the utterance's [`Ending`], which this event may itself have
/// discovered: a target that refuses the flush has lost its lease, and nothing
/// after that - here or later - may be written.
async fn route_event(
    event: OrchestratorEvent,
    target: &mut dyn Target,
    indicator: &mut dyn Indicator,
    state: &mut DictationState,
    flags: RouteFlags,
    buffer: &mut CommitBuffer,
    drops: &mut AudioDrops,
) -> Ending {
    let RouteFlags {
        mut ending,
        preedit,
        quality,
    } = flags;
    // A non-Final event is a boundary: flush the buffered final burst as one
    // commit before handling it (so ordering with `done`/indicator holds).
    if !matches!(event, OrchestratorEvent::Final(_)) {
        ending = ending.or(buffer.flush(target, ending.writes_allowed()).await);
    }

    if let OrchestratorEvent::Transcribing = event {
        if *state == DictationState::Recording {
            advance(state, DictationState::Transcribing);
        }
    }
    if let Some(indicator_state) =
        event_to_indicator(&event, *state, ending.delivery(buffer.dropped()), quality)
    {
        indicator.set_state(indicator_state).await;
    }
    if let OrchestratorEvent::AudioDropped(reason) = &event {
        drops.record(*reason);
        indicator
            .set_audio_drops(drops.not_resident, drops.not_active)
            .await;
    }
    if let OrchestratorEvent::Final(text) = &event {
        // Commit-only: stable committed text is buffered; unstable `Snippet`
        // never is (FR-012). The flush is where an ended utterance discards it
        // rather than landing it in the wrong surface (FR-014, SC-007), so
        // that the text dictated into a lost target is counted as dropped.
        myna_core::dbg_log!(
            "inject",
            "final(len={}) buffered; ending={ending:?}",
            text.len()
        );
        buffer.push(text);
    }
    if let OrchestratorEvent::Unstable(text) = &event {
        // Streaming preedit (R9, opt-in): show the volatile hypothesis in the
        // target's preedit region — replaced on each update, cleared by the
        // next `commit`. The flush above runs first, so any pending stable
        // burst is committed (which clears the old preedit) *before* the new
        // preedit tail is drawn after it. Never committed (FR-012); suppressed
        // with commits after focus-loss (FR-014); skipped unless enabled and
        // the backend has a real preedit region (`supports_preedit`).
        if preedit && ending.writes_allowed() {
            myna_core::dbg_log!("inject", "preedit(len={})", text.len());
            target.set_preedit(text).await;
        }
    }
    ending
}

/// The per-event routing decisions [`route_event`] needs, grouped so the
/// signature stays readable.
#[derive(Clone, Copy)]
struct RouteFlags {
    /// Why this utterance stopped writing, if it has: output is refused and
    /// an empty transcript is reported differently (see [`Ending`]).
    ending: Ending,
    /// Streaming-preedit opt-in (R9), where the injector has a preedit region.
    preedit: bool,
    /// The capture's verdict on the input so far, for the `Done` notice.
    quality: InputQuality,
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
    /// Whether buffered text was discarded or refused (see [`Self::dropped`]).
    dropped: bool,
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

    /// Whether text this utterance produced reached no field: the ending
    /// disallowed the write, or the target refused it because its lease is
    /// gone. Other backend failures stay best-effort and are not counted here
    /// - a real delivery disposition for them is its own change.
    fn dropped(&self) -> bool {
        self.dropped
    }

    /// Insert the buffered text as a single `CommitText`, then clear the
    /// buffer. A no-op when empty; when `allowed` is false it discards the
    /// text rather than inserting it, and records the drop.
    ///
    /// Returns [`Ending::FocusLost`] when the target refused the write because
    /// its lease is gone: the text never landed and the utterance must stop
    /// writing. Other backend failures stay best-effort - a real delivery
    /// disposition for them is its own change.
    ///
    /// The separator from already-inserted text is prepended here, but only
    /// when the buffered text doesn't carry its own leading whitespace
    /// (contract I2 servers) - never a double space.
    async fn flush(&mut self, target: &mut dyn Target, allowed: bool) -> Ending {
        if self.pending.is_empty() {
            return Ending::None;
        }
        if !allowed {
            self.pending.clear();
            self.dropped = true;
            return Ending::None;
        }
        let mut text = std::mem::take(&mut self.pending);
        if self.committed_any && !text.starts_with(char::is_whitespace) {
            text.insert(0, ' ');
        }
        match target.commit(&text).await {
            Ok(()) => {
                self.committed_any = true;
                myna_core::dbg_log!("inject", "committed {} chars: {:?}", text.len(), text);
                Ending::None
            }
            Err(InjectError::FocusLost) => {
                self.dropped = true;
                myna_core::info_log!("inject", "commit REFUSED: focus lost");
                Ending::FocusLost
            }
            Err(e) => {
                myna_core::info_log!("inject", "commit FAILED: {e}");
                Ending::None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── T066: the loading-threshold predicate (hermetic, plain Duration
    //    values — no real or paused clock needed for the pure logic) ───────

    #[test]
    fn loading_exceeds_threshold_is_false_before_the_threshold() {
        assert!(!loading_exceeds_threshold(Duration::from_secs(1)));
        assert!(!loading_exceeds_threshold(
            MODEL_LOAD_THRESHOLD - Duration::from_millis(1)
        ));
    }

    #[test]
    fn loading_exceeds_threshold_is_true_at_and_past_the_threshold() {
        assert!(loading_exceeds_threshold(MODEL_LOAD_THRESHOLD));
        assert!(loading_exceeds_threshold(
            MODEL_LOAD_THRESHOLD + Duration::from_secs(1)
        ));
    }

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
                Delivery::Landed,
                InputQuality::Ok
            ),
            Some(IndicatorState::Recording)
        );
        assert_eq!(
            event_to_indicator(
                &OrchestratorEvent::Ready,
                DictationState::Recording,
                Delivery::Landed,
                InputQuality::Ok
            ),
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
                Delivery::Landed,
                InputQuality::Ok
            ),
            Some(IndicatorState::Recording)
        );
        assert_eq!(
            event_to_indicator(
                &OrchestratorEvent::Transcribing,
                DictationState::Transcribing,
                Delivery::Landed,
                InputQuality::Ok
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
                event_to_indicator(
                    &event,
                    DictationState::Finalizing,
                    Delivery::Landed,
                    InputQuality::Ok
                ),
                None,
                "{event:?} arriving once Finalizing must not touch the indicator"
            );
        }
    }

    #[test]
    fn done_over_a_noisy_input_maps_to_the_noise_notice() {
        assert_eq!(
            event_to_indicator(
                &OrchestratorEvent::Done("all done".into()),
                DictationState::Finalizing,
                Delivery::Landed,
                InputQuality::Noisy
            ),
            Some(IndicatorState::recoverable("Background noise is high"))
        );
        // An empty transcript keeps its own, more actionable, message.
        assert_eq!(
            completion_indicator_state("", Delivery::Landed, InputQuality::Noisy),
            IndicatorState::recoverable("No speech detected")
        );
        assert_eq!(
            completion_indicator_state("", Delivery::FocusLost, InputQuality::Noisy),
            IndicatorState::recoverable("Focus lost")
        );
    }

    // ── Input quality ─────────────────────────────────────────────────────────

    fn stats(noise_floor: f32, speech_level: f32) -> AudioStats {
        AudioStats {
            noise_floor,
            speech_level,
            ..Default::default()
        }
    }

    #[test]
    fn a_quiet_input_with_clear_speech_is_ok() {
        // The HUD meter's headset baseline: -80 dBFS floor, -41 dBFS speech.
        assert_eq!(input_quality(&stats(1e-4, 0.009)), InputQuality::Ok);
        // A quiet room where nothing was said: nothing to judge.
        assert_eq!(input_quality(&stats(1e-4, 0.0)), InputQuality::Ok);
    }

    #[test]
    fn digital_silence_is_not_noise() {
        assert_eq!(input_quality(&stats(0.0, 0.0)), InputQuality::Ok);
        assert_eq!(input_quality(&stats(-1.0, 0.5)), InputQuality::Ok);
    }

    #[test]
    fn a_loud_floor_is_noisy_on_its_own() {
        assert_eq!(input_quality(&stats(0.004, 0.0)), InputQuality::Noisy);
        assert_eq!(input_quality(&stats(0.004, 0.5)), InputQuality::Noisy);
        assert_eq!(
            input_quality(&stats(NOISE_FLOOR_LIMIT, 0.5)),
            InputQuality::Ok,
            "the limit itself is still fine"
        );
    }

    #[test]
    fn speech_barely_above_the_floor_is_noisy() {
        assert_eq!(input_quality(&stats(0.002, 0.005)), InputQuality::Noisy);
        assert_eq!(input_quality(&stats(0.002, 0.0113)), InputQuality::Ok);
        // Exactly 15 dB is clean (powers of two keep the ratio exact).
        assert_eq!(
            input_quality(&stats(0.001_953_125, 0.010_937_5)),
            InputQuality::Ok
        );
    }

    // ── Auto-stop ─────────────────────────────────────────────────────────────

    fn progress(captured_ms: u64, last_voice_ms: Option<u64>) -> AudioStats {
        AudioStats {
            captured: Duration::from_millis(captured_ms),
            last_voice: last_voice_ms.map(Duration::from_millis),
            ..Default::default()
        }
    }

    #[test]
    fn off_never_stops() {
        assert!(!auto_stop_due(&progress(3_600_000, None), AutoStop::off()));
    }

    #[test]
    fn silence_counts_from_the_start_when_nothing_was_said() {
        let policy = AutoStop::toggle(Duration::from_secs(30));
        assert!(!auto_stop_due(&progress(29_900, None), policy));
        assert!(auto_stop_due(&progress(30_000, None), policy));
    }

    #[test]
    fn silence_counts_from_the_last_voice() {
        let policy = AutoStop::toggle(Duration::from_secs(30));
        assert!(!auto_stop_due(&progress(40_000, Some(20_000)), policy));
        assert!(auto_stop_due(&progress(50_000, Some(20_000)), policy));
    }

    #[test]
    fn a_zero_silence_setting_never_stops_however_long_the_session() {
        let policy = AutoStop::toggle(Duration::ZERO);
        assert!(!auto_stop_due(&progress(3_600_000, None), policy));
    }

    #[test]
    fn done_maps_to_hidden() {
        assert_eq!(
            event_to_indicator(
                &OrchestratorEvent::Done("all done".into()),
                DictationState::Finalizing,
                Delivery::Landed,
                InputQuality::Ok
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
                Delivery::Landed,
                InputQuality::Ok
            ),
            Some(IndicatorState::recoverable("No speech detected"))
        );
        assert_eq!(
            event_to_indicator(
                &OrchestratorEvent::Done("   ".into()),
                DictationState::Finalizing,
                Delivery::Landed,
                InputQuality::Ok
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
                Delivery::FocusLost,
                InputQuality::Ok
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
                Delivery::FocusLost,
                InputQuality::Ok
            ),
            Some(IndicatorState::Hidden)
        );
    }

    /// T013: `completion_indicator_state` in isolation — empty/blank →
    /// recoverable notice, non-empty → Hidden.
    #[test]
    fn completion_indicator_state_splits_on_empty_transcript() {
        assert_eq!(
            completion_indicator_state("", Delivery::Landed, InputQuality::Ok),
            IndicatorState::recoverable("No speech detected")
        );
        assert_eq!(
            completion_indicator_state("   ", Delivery::Landed, InputQuality::Ok),
            IndicatorState::recoverable("No speech detected")
        );
        assert_eq!(
            completion_indicator_state("hello", Delivery::Landed, InputQuality::Ok),
            IndicatorState::Hidden
        );
    }

    /// Regression (manual test report, 2026-07-31): `focus_lost` overrides
    /// the empty-transcript message.
    #[test]
    fn completion_indicator_state_focus_lost_overrides_empty_transcript_message() {
        assert_eq!(
            completion_indicator_state("", Delivery::FocusLost, InputQuality::Ok),
            IndicatorState::recoverable("Focus lost")
        );
        assert_eq!(
            completion_indicator_state("   ", Delivery::FocusLost, InputQuality::Ok),
            IndicatorState::recoverable("Focus lost"),
            "whitespace-only transcript still counts as empty"
        );
        assert_eq!(
            completion_indicator_state("hello", Delivery::FocusLost, InputQuality::Ok),
            IndicatorState::Hidden,
            "captured text hides the indicator even if focus was later lost"
        );
    }

    #[test]
    fn drops_are_counted_per_reason() {
        use myna_orchestrator::DropReason;
        let mut drops = AudioDrops::default();
        drops.record(DropReason::NotResident);
        drops.record(DropReason::NotResident);
        drops.record(DropReason::NotActive);
        assert_eq!(
            drops,
            AudioDrops {
                not_resident: 2,
                not_active: 1
            }
        );
    }

    #[test]
    fn error_maps_to_error_with_message() {
        // US4/T068: an unrecognized wire code falls back to
        // UNKNOWN_BACKEND_FAILURE (contract F1's "never returns None"), with
        // the raw wire message carried as `detail` (parenthesized) rather
        // than as the primary text (FR-023: no jargon/codes as primary text).
        let indicator_state = event_to_indicator(
            &OrchestratorEvent::Error {
                code: "x".into(),
                message: "boom".into(),
            },
            DictationState::Recording,
            Delivery::Landed,
            InputQuality::Ok,
        );
        let Some(IndicatorState::Error {
            message,
            recoverable,
            presentation,
        }) = indicator_state
        else {
            panic!("expected an Error state, got {indicator_state:?}");
        };
        assert!(!recoverable);
        assert!(message.contains("boom"));
        assert_eq!(
            presentation.map(|p| p.id),
            Some(myna_core::failure::UNKNOWN_BACKEND_FAILURE)
        );
    }

    #[test]
    fn error_with_a_known_code_maps_to_its_registered_presentation() {
        let indicator_state = event_to_indicator(
            &OrchestratorEvent::Error {
                code: myna_core::failure::CODE_INFERENCE_FAILED.into(),
                message: "detail from the backend".into(),
            },
            DictationState::Recording,
            Delivery::Landed,
            InputQuality::Ok,
        );
        let Some(IndicatorState::Error { presentation, .. }) = indicator_state else {
            panic!("expected an Error state, got {indicator_state:?}");
        };
        assert_eq!(
            presentation.map(|p| p.id),
            Some(myna_core::failure::CODE_INFERENCE_FAILED)
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
                Delivery::Landed,
                InputQuality::Ok
            ),
            None
        );
        assert_eq!(
            event_to_indicator(
                &OrchestratorEvent::Final("hello".into()),
                DictationState::Recording,
                Delivery::Landed,
                InputQuality::Ok
            ),
            None
        );
        assert_eq!(
            event_to_indicator(
                &OrchestratorEvent::AudioDropped(myna_orchestrator::DropReason::NotResident),
                DictationState::Recording,
                Delivery::Landed,
                InputQuality::Ok
            ),
            None
        );
    }

    // ── Ending: the one reason an utterance stopped writing ───────────────────

    #[test]
    fn the_gravest_ending_wins_however_the_loss_was_noticed() {
        assert_eq!(Ending::None.or(Ending::FocusLost), Ending::FocusLost);
        assert_eq!(Ending::FocusLost.or(Ending::None), Ending::FocusLost);
        assert_eq!(Ending::FocusLost.or(Ending::TargetGone), Ending::TargetGone);
        assert_eq!(Ending::TargetGone.or(Ending::FocusLost), Ending::TargetGone);
    }

    #[test]
    fn only_an_unended_utterance_may_write() {
        assert!(Ending::None.writes_allowed());
        assert!(!Ending::FocusLost.writes_allowed());
        assert!(!Ending::TargetGone.writes_allowed());
    }

    #[test]
    fn dropped_text_outranks_the_reason_the_utterance_ended() {
        assert_eq!(Ending::None.delivery(false), Delivery::Landed);
        assert_eq!(Ending::FocusLost.delivery(false), Delivery::FocusLost);
        // A closed target has its own "dictation target closed" message.
        assert_eq!(Ending::TargetGone.delivery(false), Delivery::Landed);
        for ending in [Ending::None, Ending::FocusLost, Ending::TargetGone] {
            assert_eq!(
                ending.delivery(true),
                Delivery::Dropped,
                "text that reached no field is the whole story: {ending:?}"
            );
        }
    }

    /// Text the target refused landed nowhere, so the completion must say so
    /// however much was transcribed: a hidden indicator reports an insertion
    /// that never happened.
    #[test]
    fn dropped_text_is_reported_however_much_was_transcribed() {
        assert_eq!(
            completion_indicator_state("hello", Delivery::Dropped, InputQuality::Ok),
            IndicatorState::recoverable("Focus lost")
        );
        assert_eq!(
            completion_indicator_state("hello", Delivery::Dropped, InputQuality::Noisy),
            IndicatorState::recoverable("Focus lost"),
            "what never landed outranks the noise notice"
        );
        assert_eq!(
            event_to_indicator(
                &OrchestratorEvent::Done("hello".into()),
                DictationState::Finalizing,
                Delivery::Dropped,
                InputQuality::Ok
            ),
            Some(IndicatorState::recoverable("Focus lost")),
            "the live Done arm agrees with the finalize block (C11)"
        );
    }

    // ── CommitBuffer: what a target's refusal means ───────────────────────────

    /// A [`Target`] that answers the next commit with a scripted error and
    /// records what it was asked to insert.
    #[derive(Debug, Default)]
    struct ScriptedTarget {
        answer: Option<InjectError>,
        commits: Vec<String>,
    }

    impl ScriptedTarget {
        fn refusing(err: InjectError) -> Self {
            Self {
                answer: Some(err),
                commits: Vec::new(),
            }
        }
    }

    #[async_trait]
    impl Target for ScriptedTarget {
        async fn commit(&mut self, text: &str) -> Result<(), InjectError> {
            self.commits.push(text.to_string());
            match self.answer.take() {
                Some(err) => Err(err),
                None => Ok(()),
            }
        }

        fn focus_events(&self) -> BoxStream<'static, FocusEvent> {
            futures_util::stream::empty().boxed()
        }

        async fn release(self: Box<Self>) {}
    }

    #[tokio::test]
    async fn a_commit_refused_for_focus_loss_ends_the_utterance() {
        let mut target = ScriptedTarget::refusing(InjectError::FocusLost);
        let mut buffer = CommitBuffer::default();
        buffer.push("hello");
        assert_eq!(buffer.flush(&mut target, true).await, Ending::FocusLost);
        assert_eq!(target.commits, vec!["hello"]);
        assert!(buffer.dropped(), "the refused text reached no field");
        // The refused text is gone, not queued for a second attempt.
        assert_eq!(buffer.flush(&mut target, true).await, Ending::None);
        assert_eq!(target.commits, vec!["hello"]);
    }

    #[tokio::test]
    async fn other_commit_failures_stay_best_effort() {
        // A real delivery disposition for a backend that cannot insert is its
        // own change; until then these are logged and the utterance goes on.
        for err in [
            InjectError::Backend("boom".into()),
            InjectError::Unavailable("ibus down".into()),
            InjectError::SecureField,
        ] {
            let mut target = ScriptedTarget::refusing(err);
            let mut buffer = CommitBuffer::default();
            buffer.push("hello");
            assert_eq!(buffer.flush(&mut target, true).await, Ending::None);
            assert_eq!(target.commits, vec!["hello"]);
            assert!(
                !buffer.dropped(),
                "a backend failure says nothing about the target being ours"
            );
        }
    }

    #[tokio::test]
    async fn a_disallowed_flush_discards_without_asking_the_target() {
        let mut target = ScriptedTarget::default();
        let mut buffer = CommitBuffer::default();
        buffer.push("hello");
        assert_eq!(buffer.flush(&mut target, false).await, Ending::None);
        assert!(target.commits.is_empty(), "nothing may be inserted");
        assert!(buffer.dropped(), "and the discarded text is reported lost");
        assert_eq!(
            buffer.flush(&mut target, true).await,
            Ending::None,
            "the discarded text is not insertable later"
        );
        assert!(target.commits.is_empty());
    }
}
