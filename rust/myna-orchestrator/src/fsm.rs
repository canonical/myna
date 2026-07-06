//! The orchestrator FSM (plan T40) — the centerpiece of Workstream G.
//!
//! This is the **pure, synchronous** core of the two-region async state machine
//! from `docs/architecture/ie115-lifecycle.md`. It owns no sockets, channels, or
//! timers: it is a function of `(state, input) -> (state', actions)`, which is
//! exactly what makes the async edge cases (§3A/§3B/§3C) exhaustively and
//! deterministically testable. The async wiring — reading backend events,
//! pushing audio, driving a real session — lives one layer up in
//! [`crate::driver`], which does nothing but pump inputs through `on_input` and
//! execute the [`Action`]s it returns.
//!
//! ## Two orthogonal regions
//!
//! - **Session** ([`SessionState`]): `Active → Finalizing → Done` (or `Failed` /
//!   `Aborted`), per connection. `Finalizing` is the commit-drain window: the
//!   hotkey is released and no more audio is accepted, but the session is *not*
//!   over until the backend emits its terminal event (§3C).
//! - **Residency** ([`Residency`]): `Unloaded → Loading → Resident`, driven
//!   independently by the backend's liveness (`STATUS` / `transcription.progress`
//!   phase) — the model may still be loading when the session is already
//!   negotiated, and may lapse back to `Loading` if it idle-unloads mid-session.
//!
//! ## Accept-gate
//!
//! An audio chunk is forwarded to the backend **iff** `session == Active` **and**
//! `residency == Resident`. Otherwise it is dropped (§3A pre-ready drop) — the
//! client must gate on `STATUS{ready}`, not merely on a negotiated session.

use myna_core::{
    ErrorData, PcmChunk, Progress, TranscriptionEvent, PHASE_PREPARING, PHASE_READY,
    PHASE_TRANSCRIBING,
};

/// The per-connection session track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    /// Negotiated; accepting audio (subject to the accept-gate).
    Active,
    /// End-of-audio signalled (`session.finish`); draining the tail, waiting for
    /// the terminal event. **Not** terminal — see §3C commit-drain.
    Finalizing,
    /// Terminal: the backend emitted `transcription.done`.
    Done,
    /// Terminal: a terminal error, or the connection dropped mid-session.
    Failed,
    /// Terminal: the client abandoned the session (`abort`); nothing committed.
    Aborted,
}

impl SessionState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            SessionState::Done | SessionState::Failed | SessionState::Aborted
        )
    }
}

/// The orthogonal model-residency track. Only `Resident` opens the accept-gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Residency {
    /// No load triggered yet (pre-liveness).
    Unloaded,
    /// Model loading (`STATUS{loading}` / `progress.phase = preparing`).
    Loading,
    /// Model resident (`STATUS{ready}` or any transcription activity).
    Resident,
}

impl Residency {
    /// The residency half of the accept-gate.
    pub fn gate_open(self) -> bool {
        matches!(self, Residency::Resident)
    }
}

/// A snapshot of both regions, for assertions and UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FsmState {
    pub session: SessionState,
    pub residency: Residency,
}

/// Why an audio chunk was dropped by the accept-gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropReason {
    /// Model not resident yet (§3A pre-ready) or it lapsed back to loading.
    NotResident,
    /// Audio arrived after end-of-audio / on a terminal session (client bug).
    NotActive,
}

/// What the FSM surfaces *outward* (to the text injector, the UI, and tests).
/// The residency variants map onto the IE115 `STATUS` liveness `state`.
#[derive(Clone, Debug, PartialEq)]
pub enum OrchestratorEvent {
    /// Model loading — show "warming up"; do not expect transcription.
    Loading,
    /// Model resident, accept-gate open — safe to speak.
    Ready,
    /// Liveness: audio is being transcribed.
    Transcribing,
    /// Unstable UI snippet — no accuracy or retraction guarantee; never commit.
    Snippet(String),
    /// Committed segment text; never retracted.
    Final(String),
    /// Utterance complete — the full transcript. Terminal.
    Done(String),
    /// A backend error surfaced. Terminal — the session is over. (Every
    /// `transcription.error` is terminal on the wire today; if T31 defines
    /// recoverable/advisory errors, the disposition must ride the wire — e.g.
    /// an `error.recoverable` field — and the §3B retry branch returns here.
    /// Do not reintroduce it as client-side string-matching on codes.)
    Error { code: String, message: String },
    /// A chunk was dropped by the accept-gate.
    AudioDropped(DropReason),
}

/// A side effect the driver must perform after a transition. Keeping effects as
/// data (rather than calling out directly) is what keeps [`Fsm`] pure.
#[derive(Debug)]
pub enum Action {
    /// Push this chunk to the backend (gate was open).
    ForwardAudio(PcmChunk),
    /// Send `session.finish` upstream.
    SendFinish,
    /// Abort the backend session (close without finishing).
    SendAbort,
    /// Surface an event to the caller.
    Emit(OrchestratorEvent),
}

/// An input the FSM reacts to: from the client side (audio / hotkey) or from the
/// backend (transcript events / connection loss).
#[derive(Debug)]
pub enum Input {
    /// A PCM chunk from the audio adapter.
    Audio(PcmChunk),
    /// Hotkey released — end of the current utterance.
    EndOfAudio,
    /// The client abandoned the utterance.
    Abort,
    /// A transcript event arrived from the backend.
    Backend(TranscriptionEvent),
    /// The backend connection closed (optionally with a wire-error reason).
    BackendClosed { error: Option<String> },
}

/// How a completed [`Fsm`] run ended.
#[derive(Clone, Debug, PartialEq)]
pub enum SessionOutcome {
    /// `transcription.done` — the full committed transcript.
    Completed { transcript: String },
    /// Terminal error or unexpected disconnect.
    Failed { code: String, message: String },
    /// The client aborted; nothing committed.
    Aborted,
}

/// The two-region FSM. Construct with [`Fsm::new`], feed [`Input`]s to
/// [`Fsm::on_input`], and read the resulting [`Action`]s.
#[derive(Debug)]
pub struct Fsm {
    session: SessionState,
    residency: Residency,
    transcript: String,
    failure: Option<(String, String)>,
}

impl Default for Fsm {
    fn default() -> Self {
        Self::new()
    }
}

impl Fsm {
    pub fn new() -> Self {
        Self {
            // The session is negotiated by the time the FSM runs (the handshake
            // is the backend's `open_session`), so it starts Active. The model
            // may still be cold — residency starts Unloaded and is driven by
            // liveness.
            session: SessionState::Active,
            residency: Residency::Unloaded,
            transcript: String::new(),
            failure: None,
        }
    }

    pub fn state(&self) -> FsmState {
        FsmState {
            session: self.session,
            residency: self.residency,
        }
    }

    /// The outcome, once [`SessionState::is_terminal`] holds.
    pub fn outcome(&self) -> Option<SessionOutcome> {
        match self.session {
            SessionState::Done => Some(SessionOutcome::Completed {
                transcript: self.transcript.clone(),
            }),
            SessionState::Failed => {
                let (code, message) = self
                    .failure
                    .clone()
                    .unwrap_or_else(|| ("internal".into(), String::new()));
                Some(SessionOutcome::Failed { code, message })
            }
            SessionState::Aborted => Some(SessionOutcome::Aborted),
            SessionState::Active | SessionState::Finalizing => None,
        }
    }

    /// The single transition function. Returns the effects the driver applies.
    pub fn on_input(&mut self, input: Input) -> Vec<Action> {
        let mut actions = Vec::new();
        match input {
            Input::Audio(chunk) => self.on_audio(chunk, &mut actions),
            Input::EndOfAudio => self.on_end_of_audio(&mut actions),
            Input::Abort => self.on_abort(&mut actions),
            Input::Backend(event) => self.on_backend(event, &mut actions),
            Input::BackendClosed { error } => self.on_backend_closed(error, &mut actions),
        }
        actions
    }

    fn on_audio(&mut self, chunk: PcmChunk, out: &mut Vec<Action>) {
        if self.session == SessionState::Active && self.residency.gate_open() {
            out.push(Action::ForwardAudio(chunk));
        } else if self.session == SessionState::Active {
            // §3A: gate closed because the model is not resident. Drop, don't
            // buffer — the client should have waited for `Ready`.
            out.push(Action::Emit(OrchestratorEvent::AudioDropped(
                DropReason::NotResident,
            )));
        } else {
            // Finalizing or terminal: audio after end-of-audio is a client bug.
            out.push(Action::Emit(OrchestratorEvent::AudioDropped(
                DropReason::NotActive,
            )));
        }
    }

    fn on_end_of_audio(&mut self, out: &mut Vec<Action>) {
        // Only meaningful from Active; idempotent otherwise (double release,
        // release after a terminal event).
        if self.session == SessionState::Active {
            self.session = SessionState::Finalizing;
            out.push(Action::SendFinish);
        }
    }

    fn on_abort(&mut self, out: &mut Vec<Action>) {
        if !self.session.is_terminal() {
            self.session = SessionState::Aborted;
            out.push(Action::SendAbort);
        }
    }

    fn on_backend(&mut self, event: TranscriptionEvent, out: &mut Vec<Action>) {
        // Late events after a terminal state are ignored (e.g. a stray frame
        // racing a close).
        if self.session.is_terminal() {
            return;
        }
        match event {
            TranscriptionEvent::Progress(p) => self.on_progress(p, out),
            TranscriptionEvent::Final(f) => {
                self.mark_resident(out);
                out.push(Action::Emit(OrchestratorEvent::Final(f.text)));
            }
            TranscriptionEvent::Done(d) => {
                self.transcript = d.text.clone();
                self.session = SessionState::Done;
                out.push(Action::Emit(OrchestratorEvent::Done(d.text)));
            }
            TranscriptionEvent::Error(e) => self.on_error(e, out),
        }
    }

    fn on_progress(&mut self, p: Progress, out: &mut Vec<Action>) {
        match p.phase.as_str() {
            PHASE_PREPARING => {
                // Model (re-)loading — the residency region can lapse back here
                // mid-session if it idle-unloaded (lifecycle open item #4),
                // closing the gate again.
                if self.residency != Residency::Loading {
                    self.residency = Residency::Loading;
                    out.push(Action::Emit(OrchestratorEvent::Loading));
                }
            }
            PHASE_READY => self.mark_resident(out),
            PHASE_TRANSCRIBING => {
                self.mark_resident(out);
                out.push(Action::Emit(OrchestratorEvent::Transcribing));
            }
            // Unknown phase: ignore (forward-compatible — additive phases are a
            // liveness detail, not a contract break).
            _ => {}
        }
        if let Some(snippet) = p.snippet {
            out.push(Action::Emit(OrchestratorEvent::Snippet(snippet)));
        }
    }

    /// Open the accept-gate the first time we see the model is resident, emitting
    /// `Ready` exactly once per transition into residency.
    fn mark_resident(&mut self, out: &mut Vec<Action>) {
        if self.residency != Residency::Resident {
            self.residency = Residency::Resident;
            out.push(Action::Emit(OrchestratorEvent::Ready));
        }
    }

    fn on_error(&mut self, e: ErrorData, out: &mut Vec<Action>) {
        // §3B: every backend error fails the session — that is the wire's
        // actual contract (both dialects treat `transcription.error`/`error`
        // as utterance-terminal). See the `OrchestratorEvent::Error` note for
        // where a T31 recoverable disposition would slot back in.
        out.push(Action::Emit(OrchestratorEvent::Error {
            code: e.code.clone(),
            message: e.message.clone(),
        }));
        self.failure = Some((e.code, e.message));
        self.session = SessionState::Failed;
    }

    fn on_backend_closed(&mut self, error: Option<String>, out: &mut Vec<Action>) {
        if self.session.is_terminal() {
            // Expected: the backend closes right after its terminal event.
            return;
        }
        let code = "connection_closed".to_string();
        let message = error
            .unwrap_or_else(|| "backend connection closed before the session completed".into());
        out.push(Action::Emit(OrchestratorEvent::Error {
            code: code.clone(),
            message: message.clone(),
        }));
        self.failure = Some((code, message));
        self.session = SessionState::Failed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use myna_core::{AudioFormat, TranscriptionFinal};

    fn chunk() -> PcmChunk {
        let fmt = AudioFormat::default();
        PcmChunk::new(vec![0u8; 3200], fmt) // ~100 ms
    }

    fn progress(phase: &str) -> TranscriptionEvent {
        TranscriptionEvent::Progress(Progress::phase(phase))
    }

    fn snippet(text: &str) -> TranscriptionEvent {
        TranscriptionEvent::Progress(Progress {
            snippet: Some(text.into()),
            phase: PHASE_TRANSCRIBING.into(),
        })
    }

    fn final_seg(text: &str) -> TranscriptionEvent {
        TranscriptionEvent::Final(TranscriptionFinal {
            text: text.into(),
            segments: vec![],
        })
    }

    fn done(text: &str) -> TranscriptionEvent {
        TranscriptionEvent::Done(TranscriptionFinal {
            text: text.into(),
            segments: vec![],
        })
    }

    fn error(code: &str) -> TranscriptionEvent {
        TranscriptionEvent::Error(ErrorData {
            code: code.into(),
            message: "boom".into(),
        })
    }

    /// Collect the `OrchestratorEvent`s from a batch of actions, ignoring the
    /// transport actions.
    fn emitted(actions: &[Action]) -> Vec<OrchestratorEvent> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Emit(e) => Some(e.clone()),
                _ => None,
            })
            .collect()
    }

    fn is_forward(action: &Action) -> bool {
        matches!(action, Action::ForwardAudio(_))
    }

    #[test]
    fn starts_active_and_unloaded() {
        let fsm = Fsm::new();
        assert_eq!(
            fsm.state(),
            FsmState {
                session: SessionState::Active,
                residency: Residency::Unloaded
            }
        );
        assert_eq!(fsm.outcome(), None);
    }

    #[test]
    fn liveness_drives_residency_region() {
        let mut fsm = Fsm::new();
        assert_eq!(
            emitted(&fsm.on_input(Input::Backend(progress(PHASE_PREPARING)))),
            vec![OrchestratorEvent::Loading]
        );
        assert_eq!(fsm.state().residency, Residency::Loading);

        assert_eq!(
            emitted(&fsm.on_input(Input::Backend(progress(PHASE_READY)))),
            vec![OrchestratorEvent::Ready]
        );
        assert_eq!(fsm.state().residency, Residency::Resident);

        // Ready is emitted exactly once — a redundant ready is a no-op.
        assert!(emitted(&fsm.on_input(Input::Backend(progress(PHASE_READY)))).is_empty());
    }

    #[test]
    fn transcribing_phase_opens_gate_and_pings_liveness() {
        let mut fsm = Fsm::new();
        let actions = fsm.on_input(Input::Backend(progress(PHASE_TRANSCRIBING)));
        assert_eq!(
            emitted(&actions),
            vec![OrchestratorEvent::Ready, OrchestratorEvent::Transcribing]
        );
        assert_eq!(fsm.state().residency, Residency::Resident);
    }

    #[test]
    fn snippet_progress_opens_gate_and_surfaces_snippet() {
        // The Python fake adapter emits bare `progress(snippet=…)` (phase defaults
        // to transcribing) — this must both open the gate and surface UI text.
        let mut fsm = Fsm::new();
        let actions = fsm.on_input(Input::Backend(snippet("the quick")));
        assert_eq!(
            emitted(&actions),
            vec![
                OrchestratorEvent::Ready,
                OrchestratorEvent::Transcribing,
                OrchestratorEvent::Snippet("the quick".into()),
            ]
        );
    }

    // ---- §3A: audio before the model is ready is dropped ---------------------

    #[test]
    fn edge_3a_pre_ready_audio_is_dropped_then_forwarded_when_resident() {
        let mut fsm = Fsm::new();

        // Unloaded: gate closed.
        let a = fsm.on_input(Input::Audio(chunk()));
        assert!(!a.iter().any(is_forward));
        assert_eq!(
            emitted(&a),
            vec![OrchestratorEvent::AudioDropped(DropReason::NotResident)]
        );

        // Loading: still closed.
        fsm.on_input(Input::Backend(progress(PHASE_PREPARING)));
        let a = fsm.on_input(Input::Audio(chunk()));
        assert_eq!(
            emitted(&a),
            vec![OrchestratorEvent::AudioDropped(DropReason::NotResident)]
        );

        // Resident: gate opens, audio is forwarded and nothing is emitted.
        fsm.on_input(Input::Backend(progress(PHASE_READY)));
        let a = fsm.on_input(Input::Audio(chunk()));
        assert!(a.iter().any(is_forward));
        assert!(emitted(&a).is_empty());
    }

    #[test]
    fn residency_can_lapse_back_to_loading_mid_session() {
        // Lifecycle open item #4: an idle-unload between utterances re-closes
        // the gate until the model reloads.
        let mut fsm = Fsm::new();
        fsm.on_input(Input::Backend(progress(PHASE_READY)));
        assert!(fsm.on_input(Input::Audio(chunk())).iter().any(is_forward));

        // Model idle-unloads → re-loading; gate closes again.
        assert_eq!(
            emitted(&fsm.on_input(Input::Backend(progress(PHASE_PREPARING)))),
            vec![OrchestratorEvent::Loading]
        );
        let a = fsm.on_input(Input::Audio(chunk()));
        assert_eq!(
            emitted(&a),
            vec![OrchestratorEvent::AudioDropped(DropReason::NotResident)]
        );

        // Reloads → gate reopens.
        fsm.on_input(Input::Backend(progress(PHASE_READY)));
        assert!(fsm.on_input(Input::Audio(chunk())).iter().any(is_forward));
    }

    // ---- §3B: error mid-stream ----------------------------------------------

    #[test]
    fn edge_3b_any_error_fails_the_session() {
        // Every backend error is terminal — the wire carries no disposition
        // (T31 open); when it does, the recoverable branch returns with it.
        let mut fsm = Fsm::new();
        fsm.on_input(Input::Backend(progress(PHASE_READY)));
        let a = fsm.on_input(Input::Backend(error("internal")));
        assert_eq!(
            emitted(&a),
            vec![OrchestratorEvent::Error {
                code: "internal".into(),
                message: "boom".into(),
            }]
        );
        assert_eq!(fsm.state().session, SessionState::Failed);
        assert_eq!(
            fsm.outcome(),
            Some(SessionOutcome::Failed {
                code: "internal".into(),
                message: "boom".into(),
            })
        );
    }

    // ---- §3C: commit-drain — COMMIT is not "done" ---------------------------

    #[test]
    fn edge_3c_commit_drain_tail_after_finish() {
        let mut fsm = Fsm::new();
        fsm.on_input(Input::Backend(progress(PHASE_READY)));

        // Stream, then release the hotkey.
        assert!(fsm.on_input(Input::Audio(chunk())).iter().any(is_forward));
        let a = fsm.on_input(Input::EndOfAudio);
        assert!(matches!(a.as_slice(), [Action::SendFinish]));
        assert_eq!(fsm.state().session, SessionState::Finalizing);

        // Tail finals arrive *after* finish — expected, not late/ignored.
        assert_eq!(
            emitted(&fsm.on_input(Input::Backend(final_seg("brown fox")))),
            vec![OrchestratorEvent::Final("brown fox".into())]
        );
        assert_eq!(fsm.state().session, SessionState::Finalizing);

        // Only `done` completes the utterance.
        assert_eq!(
            emitted(&fsm.on_input(Input::Backend(done("the quick brown fox")))),
            vec![OrchestratorEvent::Done("the quick brown fox".into())]
        );
        assert_eq!(
            fsm.outcome(),
            Some(SessionOutcome::Completed {
                transcript: "the quick brown fox".into(),
            })
        );
    }

    #[test]
    fn edge_3c_audio_after_finish_is_dropped_not_forwarded() {
        let mut fsm = Fsm::new();
        fsm.on_input(Input::Backend(progress(PHASE_READY)));
        fsm.on_input(Input::EndOfAudio);
        let a = fsm.on_input(Input::Audio(chunk()));
        assert!(!a.iter().any(is_forward));
        assert_eq!(
            emitted(&a),
            vec![OrchestratorEvent::AudioDropped(DropReason::NotActive)]
        );
    }

    #[test]
    fn double_finish_is_idempotent() {
        let mut fsm = Fsm::new();
        fsm.on_input(Input::Backend(progress(PHASE_READY)));
        assert!(matches!(
            fsm.on_input(Input::EndOfAudio).as_slice(),
            [Action::SendFinish]
        ));
        // A second release does nothing.
        assert!(fsm.on_input(Input::EndOfAudio).is_empty());
        assert_eq!(fsm.state().session, SessionState::Finalizing);
    }

    // ---- abort + unexpected close -------------------------------------------

    #[test]
    fn abort_sends_abort_and_commits_nothing() {
        let mut fsm = Fsm::new();
        fsm.on_input(Input::Backend(progress(PHASE_READY)));
        let a = fsm.on_input(Input::Abort);
        assert!(matches!(a.as_slice(), [Action::SendAbort]));
        assert_eq!(fsm.outcome(), Some(SessionOutcome::Aborted));
        // Idempotent after terminal.
        assert!(fsm.on_input(Input::Abort).is_empty());
    }

    #[test]
    fn unexpected_close_fails_the_session() {
        let mut fsm = Fsm::new();
        fsm.on_input(Input::Backend(progress(PHASE_READY)));
        let a = fsm.on_input(Input::BackendClosed {
            error: Some("reset".into()),
        });
        assert_eq!(
            emitted(&a),
            vec![OrchestratorEvent::Error {
                code: "connection_closed".into(),
                message: "reset".into(),
            }]
        );
        assert_eq!(
            fsm.outcome(),
            Some(SessionOutcome::Failed {
                code: "connection_closed".into(),
                message: "reset".into(),
            })
        );
    }

    #[test]
    fn close_after_done_is_ignored() {
        let mut fsm = Fsm::new();
        fsm.on_input(Input::Backend(progress(PHASE_READY)));
        fsm.on_input(Input::Backend(done("hello")));
        // The backend closing right after `done` is expected, not a failure.
        assert!(fsm
            .on_input(Input::BackendClosed { error: None })
            .is_empty());
        assert_eq!(
            fsm.outcome(),
            Some(SessionOutcome::Completed {
                transcript: "hello".into()
            })
        );
    }

    #[test]
    fn done_without_explicit_finish_completes() {
        // Some backends emit `done` in Active (no separate finish); still valid.
        let mut fsm = Fsm::new();
        fsm.on_input(Input::Backend(progress(PHASE_READY)));
        fsm.on_input(Input::Backend(done("hi")));
        assert_eq!(fsm.state().session, SessionState::Done);
    }

    #[test]
    fn late_events_after_terminal_are_ignored() {
        let mut fsm = Fsm::new();
        fsm.on_input(Input::Backend(error("internal")));
        assert_eq!(fsm.state().session, SessionState::Failed);
        // A stray final racing the close changes nothing.
        assert!(fsm
            .on_input(Input::Backend(final_seg("ignored")))
            .is_empty());
        assert!(fsm.on_input(Input::Audio(chunk())).iter().any(|a| matches!(
            a,
            Action::Emit(OrchestratorEvent::AudioDropped(DropReason::NotActive))
        )));
    }
}
