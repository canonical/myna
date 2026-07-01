//! Orchestrator subsystem — the client-side dictation brain (plan Workstream G).
//!
//! This crate holds the two-region async FSM from
//! `docs/architecture/ie115-lifecycle.md`: the per-connection session track
//! (CREATED → ACTIVE → FINALIZING → DONE) running orthogonally to model
//! residency (UNLOADED → LOADING → RESIDENT), with the accept-gate
//! (`ACTIVE ∧ RESIDENT`), commit-drain (COMMIT ≠ done), pre-ready-audio drop,
//! and terminal-vs-recoverable error mapping.
//!
//! Every external boundary is a trait with a mock, so the FSM is buildable and
//! testable before the real audio adapter (Matias) and IE115 inference snap
//! (Ivano) land:
//! - [`backend::BackendClient`] — the inference service (T39). First impl:
//!   [`backend::ws_unix::WsUnixBackend`], speaking the existing `myna.core`
//!   ws+unix wire against the running Python `myna-server`.
//! - `AudioSource` — capture (T41), mocked by a WAV source; the real adapter
//!   drops in per `docs/audio-adapter-api.md`.
//! - `Trigger` / `TextSink` — hotkey and injector (T41), mocked by stdin/stdout.
//!
//! Status: `myna-core` (wire contract, T38) and the backend seam (T39) landed;
//! the FSM itself is T40.

pub mod backend;

pub use backend::{
    ws_unix::WsUnixBackend, BackendClient, BackendError, BackendEvents, BackendHandle, BackendSink,
    Outbound,
};
pub use myna_core;
