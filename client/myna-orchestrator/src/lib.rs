//! Orchestrator subsystem — the client-side dictation brain (plan Workstream G).
//!
//! This crate holds the two-region async FSM from
//! `docs/architecture/ie115-lifecycle.md`: the per-connection session track
//! (CREATED → ACTIVE → FINALIZING → DONE) running orthogonally to model
//! residency (UNLOADED → LOADING → RESIDENT), with the accept-gate
//! (`ACTIVE ∧ RESIDENT`), commit-drain (COMMIT ≠ done), and pre-ready-audio
//! drop. Every backend error is terminal (the wire carries no disposition; a
//! T31 recoverable axis must arrive on the wire before a retry path returns).
//!
//! Every external boundary is a trait with a mock, so the FSM is buildable and
//! testable before the real audio adapter (Matias) and IE115 inference snap
//! (Ivano) land:
//! - [`backend::BackendClient`] — the inference service (T39). First impl:
//!   [`backend::ws_unix::WsUnixBackend`], speaking the existing `myna.core`
//!   ws+unix wire against the running Python `myna-server`; the in-process
//!   [`backend::fake::FakeBackend`] (T40) is the scripted regression fixture.
//! - [`audio::AudioSource`] — capture (T41), mocked by [`audio::WavFileSource`];
//!   the real PipeWire adapter drops in per `docs/audio-adapter-api.md` §3.
//! - [`trigger::Trigger`] / [`sink::TextSink`] — hotkey and injector (T41),
//!   mocked by [`trigger::StdinTrigger`] / [`sink::StdoutSink`]; the real
//!   GlobalShortcuts hotkey (T21) and IBus injector (T22) implement the traits.
//!
//! Status: `myna-core` (wire contract, T38), the backend seam (T39), the
//! two-region FSM + async driver (T40, [`fsm`] / [`driver`]), and the boundary
//! mocks + one-utterance runner (T41, [`audio`] / [`trigger`] / [`sink`] /
//! [`runner`], wired in the `myna-dictate` binary) have all landed.

pub mod audio;
pub mod backend;
pub mod driver;
pub mod fsm;
pub mod i18n;
pub mod runner;
pub mod sink;
pub mod trigger;

pub use audio::{AudioSource, CaptureError, CaptureStream, StopHandle, WavFileSource};
pub use backend::{
    fake::FakeBackend,
    ws_unix::{query_capabilities, WsUnixBackend},
    ws_unix_ie115::WsUnixIe115Backend,
    BackendClient, BackendError, BackendEvents, BackendHandle, BackendSink, Outbound,
};
pub use driver::{run_session, OrchestratorInput};
pub use fsm::{
    Action, DropReason, Fsm, FsmState, Input, OrchestratorEvent, Residency, SessionOutcome,
    SessionState,
};
pub use myna_core;
pub use runner::run_dictation;
pub use sink::{CollectingSink, StdoutSink, TextSink};
pub use trigger::{ScriptedTrigger, StdinTrigger, Trigger, TriggerEdge};
