//! Core types and the JSON wire codec for the myna dictation orchestrator.
//!
//! This crate is the Rust mirror of Python `myna.core`: the event vocabulary,
//! session config, protocol version, and audio types that make up the
//! client↔service contract over a transport. The orchestrator (plan Workstream
//! G) speaks the *existing* ws+unix wire first (against the running Python
//! `myna-server`), with IE115 event names layered on later (T43) — so the wire
//! shapes here must match the Python side exactly (verified against golden
//! frames captured from `myna.core` in the module tests).
//!
//! Wire framing (see `docs/architecture/ie115-lifecycle.md` and
//! `myna/core/transport_ws.py`):
//! - Control frames carry a `"type"` key (`session.start`, `session.finish`,
//!   `capabilities.query`, `session.created`) — see [`control`].
//! - Transcript events carry an `"event"` key (`{"event": …, "data": {…}}`) —
//!   see [`events`]. That key is how a reader tells the two apart.
//! - PCM audio travels as raw binary frames (not JSON), so [`audio::PcmChunk`]
//!   has no serde impl.

pub mod audio;
pub mod capture;
pub mod control;
pub mod debug;
pub mod events;
pub mod protocol;
pub mod session;
pub mod settings;
pub mod tier;

pub use audio::{AudioFormat, PcmChunk};
pub use capture::{AudioSource, CaptureError, CaptureStream, StopHandle};
pub use control::{ClientControl, ServerControl};
pub use events::{
    Disposition, ErrorData, Progress, Segment, StreamingMode, TranscriptionEvent,
    TranscriptionFinal, WireError, PHASE_PREPARING, PHASE_READY, PHASE_TRANSCRIBING,
};
pub use protocol::{is_supported, PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS};
pub use session::SessionConfig;
pub use settings::{effective_mode, hardware_tier, resolve_mode, tier_table, Settings};
pub use tier::{
    streaming_viable, streaming_viable_here, TierAssessment, TierTable, DEFAULT_RTF_THRESHOLD,
};
