//! The capture-side consumer contract — what the orchestrator sees of an audio
//! source, kept here beside
//! [`AudioFormat`]/[`PcmChunk`] so capture implementations (the `myna-audio`
//! adapter crate) depend on the wire vocabulary only, never on the
//! orchestrator.
//!
//! Rules of engagement (the contract the session controller codes against):
//! - `capture()` is the hotkey press: the device opens and the adapter's ring
//!   starts filling the moment it is called.
//! - Polling may be deferred: the consumer holds off draining until the model
//!   is `ready`; nothing is lost up to the ring depth.
//! - Graceful stop ([`StopHandle::stop`]) drains then ends; dropping the
//!   stream aborts and discards.
//! - A fatal fault is exactly one `Err`, then `None` — never an empty stream
//!   masquerading as a clean end.
//! - [`AudioSource::health`] reports opening, capturing, a fault or the end
//!   as they happen, independent of whether the stream is being drained.

use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures_util::Stream;
use thiserror::Error;

use crate::audio::{AudioFormat, PcmChunk};

/// A capture-side fault. Surfaced as an `Err` stream
/// item so the dictation service turns it into a terminal session error rather
/// than a silent stall.
#[derive(Clone, Debug, Error, PartialEq)]
pub enum CaptureError {
    #[error("audio device unavailable: {0}")]
    DeviceUnavailable(String),
    #[error("requested format {0:?} cannot be produced")]
    UnsupportedFormat(AudioFormat),
    #[error("capture backend failed: {0}")]
    Backend(String),
    /// The capture buffer hit its bound because the consumer (the STT service)
    /// could not keep up — an overload/lag condition. Surfaced (not silently
    /// dropped) so the client can tell the user their hardware tier can't keep
    /// up rather than lose speech. Carries the buffered duration in seconds.
    #[error("audio buffer overflow after {0:.1}s — the transcription service cannot keep up with capture")]
    Overloaded(f64),
}

/// The stream a source yields once capturing: chunks until a clean end
/// (`None`) or a fatal fault (one `Err`, then `None`).
pub type CaptureStream = Pin<Box<dyn Stream<Item = Result<PcmChunk, CaptureError>> + Send>>;

/// Capture lifecycle as seen without draining PCM. `Faulted` and `Ended` are
/// terminal; a source never leaves them.
#[derive(Clone, Debug, PartialEq)]
pub enum CaptureHealth {
    /// Not yet capturing: the device is being opened, or `capture()` has not
    /// been called.
    Opening,
    /// Audio has arrived from the device.
    Capturing,
    /// Capture failed: open error, device fault, stall or overload. The same
    /// error is the stream's `Err` item once the audio queued before it drains.
    Faulted(CaptureError),
    /// Capture ended cleanly (graceful stop, end of input or abort). Queued
    /// audio may still be draining.
    Ended,
}

/// Health transitions of one source: the current state first, then each
/// change, conflated (only the latest state is kept while nobody polls).
/// The stream ends once the source has released its device.
pub type CaptureHealthStream = Pin<Box<dyn Stream<Item = CaptureHealth> + Send>>;

/// A source of push-side PCM. The dictation service
/// sets the exact [`AudioFormat`] from the STT service's advertised
/// capabilities; the source produces exactly that and nothing else.
pub trait AudioSource: Send {
    /// The exact format this source emits.
    fn format(&self) -> AudioFormat;

    /// Observe capture health. Call before [`AudioSource::capture`]; each call
    /// is an independent subscriber and never consumes audio.
    fn health(&self) -> CaptureHealthStream;

    /// Begin capture, consuming the source. Returns promptly: opening the
    /// device happens behind the stream, and its outcome shows on `health`.
    fn capture(self: Box<Self>) -> CaptureStream;
}

/// A boxed source is a source — lets callers pick an implementation at
/// runtime (e.g. live mic vs WAV clip) and hand it to generic consumers.
impl AudioSource for Box<dyn AudioSource> {
    fn format(&self) -> AudioFormat {
        (**self).format()
    }

    fn health(&self) -> CaptureHealthStream {
        (**self).health()
    }

    fn capture(self: Box<Self>) -> CaptureStream {
        (*self).capture()
    }
}

/// A cheap, cloneable graceful-stop handle: setting
/// it makes an in-flight capture **drain then end** (stream yields `None`),
/// which the orchestrator reads as end-of-audio — the clean hotkey-release
/// path. Dropping the stream instead is the abort path.
///
/// Plain flag by design: backends poll it (promptness contract ~250 ms), which
/// works from a tokio task, a thread, or a loop timer alike.
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
