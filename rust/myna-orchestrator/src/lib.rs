//! Orchestrator subsystem — the client-side dictation brain (plan Workstream G).
//!
//! This crate will hold the two-region async FSM from
//! `docs/architecture/ie115-lifecycle.md`: the per-connection session track
//! (CREATED → ACTIVE → FINALIZING → DONE) running orthogonally to model
//! residency (UNLOADED → LOADING → RESIDENT), with the accept-gate
//! (`ACTIVE ∧ RESIDENT`), commit-drain (COMMIT ≠ done), pre-ready-audio drop,
//! and terminal-vs-recoverable error mapping.
//!
//! Every external boundary is a trait with a mock, so the FSM is buildable and
//! testable before the real audio adapter (Matias) and IE115 inference snap
//! (Ivano) land:
//! - `BackendClient` — the inference service (T39), first speaking the existing
//!   `myna.core` ws+unix wire against the running Python `myna-server`.
//! - `AudioSource` — capture (T41), mocked by a WAV source; real adapter drops
//!   in per `docs/audio-adapter-api.md`.
//! - `Trigger` / `TextSink` — hotkey and injector (T41), mocked by stdin/stdout.
//!
//! T38 landed `myna-core` (the wire contract); the FSM itself is T40.

// Placeholder re-export so downstream crates have a stable path while the FSM
// is built out.
pub use myna_core;
