//! The capture-side consumer contract (`docs/audio-adapter-api.md` §3) — what
//! the orchestrator sees of an audio source, kept here beside
//! [`AudioFormat`]/[`PcmChunk`] so capture implementations (the `myna-audio`
//! adapter crate) depend on the wire vocabulary only, never on the
//! orchestrator.
//!
//! Rules of engagement (the contract the session controller codes against):
//! - `capture()` is the hotkey press: the device opens and the adapter's ring
//!   starts filling the moment it is called.
//! - Polling may be deferred: the consumer holds off draining until the model
//!   is `ready`; nothing is lost up to the ring depth (audio-adapter-api §6).
//! - Graceful stop ([`StopHandle::stop`]) drains then ends; dropping the
//!   stream aborts and discards.
//! - A fatal fault is exactly one `Err`, then `None` — never an empty stream
//!   masquerading as a clean end.

use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures_util::Stream;
use thiserror::Error;

use crate::audio::{AudioFormat, PcmChunk};

/// A capture-side fault (audio-adapter-api §3). Surfaced as an `Err` stream
/// item so the dictation service turns it into a terminal session error rather
/// than a silent stall.
#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("audio device unavailable: {0}")]
    DeviceUnavailable(String),
    #[error("requested format {0:?} cannot be produced")]
    UnsupportedFormat(AudioFormat),
    #[error("capture backend failed: {0}")]
    Backend(String),
}

/// The stream a source yields once capturing: chunks until a clean end
/// (`None`) or a fatal fault (one `Err`, then `None`).
pub type CaptureStream = Pin<Box<dyn Stream<Item = Result<PcmChunk, CaptureError>> + Send>>;

/// A source of push-side PCM (audio-adapter-api §3). The dictation service
/// sets the exact [`AudioFormat`] from the STT service's advertised
/// capabilities; the source produces exactly that and nothing else.
pub trait AudioSource: Send {
    /// The exact format this source emits.
    fn format(&self) -> AudioFormat;

    /// Begin capture, consuming the source.
    fn capture(self: Box<Self>) -> CaptureStream;
}

/// A boxed source is a source — lets callers pick an implementation at
/// runtime (e.g. live mic vs WAV clip) and hand it to generic consumers.
impl AudioSource for Box<dyn AudioSource> {
    fn format(&self) -> AudioFormat {
        (**self).format()
    }

    fn capture(self: Box<Self>) -> CaptureStream {
        (*self).capture()
    }
}

/// A cheap, cloneable graceful-stop handle (audio-adapter-api §3/§5): setting
/// it makes an in-flight capture **drain then end** (stream yields `None`),
/// which the orchestrator reads as end-of-audio — the clean hotkey-release
/// path. Dropping the stream instead is the abort path.
///
/// Plain flag by design: backends poll it (promptness contract ~250 ms), which
/// works from a tokio task, a thread, or a realtime callback alike.
#[derive(Clone, Debug, Default)]
pub struct StopHandle(Arc<AtomicBool>);

impl StopHandle {
    pub fn stop(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_stopped(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}
