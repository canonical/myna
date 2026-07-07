//! Audio-capture adapter for the myna dictation service — the settled
//! contract in `docs/audio-adapter-api.md` (v2), plan T50–T52.
//!
//! One public type, [`CaptureSource`], implements the `myna_core`
//! [`AudioSource`] trait over a pluggable [`CaptureBackend`]:
//!
//! ```text
//! device ─▶ CaptureBackend ─▶ Producer::push(bytes)
//!             │ re-chunk to whole-frame ~100 ms chunks
//!             │ update stats tap (RMS/peak/clip, §8)
//!             ▼
//!     bounded ring (drop-oldest, §6)
//!             ▼
//!     CaptureStream  ◀── drained when the consumer chooses
//! ```
//!
//! Invariants (audio-adapter-api §1): the client owns capture and pushes PCM;
//! the source produces **exactly** its configured [`AudioFormat`] (the backend
//! owns conversion); audio never persists — a bounded in-memory ring only,
//! discarded on session end; no content logged (the stats tap carries levels
//! and counters, never samples).
//!
//! The pre-ready requirement (§6): `capture()` is the hotkey press — the ring
//! fills immediately; the consumer defers draining until the model is `ready`,
//! so nothing said during a cold load is lost, up to the ring depth. Overflow
//! is drop-oldest, surfaced as [`AudioStats::dropped`].
//!
//! Backends: [`ScriptedBackend`] (the permanent fake fixture, this crate's
//! T50 deliverable), `PwRecordBackend` (subprocess, T51), `PipeWireBackend`
//! (native `pipewire-rs`, T52). Real DSP (noise suppression etc.) is PipeWire
//! filter-chain territory upstream of the capture node — this crate observes,
//! it never transforms (§10).

mod backend;
mod fake;
mod ring;
mod source;
mod stats;

pub use backend::{CaptureBackend, CaptureSpec, Producer};
pub use fake::{ScriptedBackend, Step};
pub use source::{CaptureSource, CaptureSourceBuilder, DEFAULT_CHUNK, DEFAULT_RING_DEPTH};
pub use stats::AudioStats;

// The consumer contract this crate implements, re-exported for convenience.
pub use myna_core::{AudioFormat, AudioSource, CaptureError, CaptureStream, PcmChunk, StopHandle};
