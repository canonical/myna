//! The text-injection boundary (plan T22, UD129 Text Injection Layer).
//!
//! Backend-agnostic (FR-016): the controller drives committed transcripts at an
//! [`Injector`] without knowing whether the target is filled via IBus, a future
//! Wayland `input_method_v2`, or a uinput fallback. [`ibus::IbusInjector`] is the
//! shipped implementor (branch 003b); [`mock::MockInjector`] is the hermetic test
//! fixture. See `specs/003-desktop-injection/contracts/injector.md`.

use async_trait::async_trait;
use futures_util::stream::BoxStream;

pub mod ibus;
pub mod mock;

/// An opaque handle to the surface focused when the session started. Carries the
/// **secure flag** (from the field's content-type) and enough identity to detect
/// a focus change / disappearance. Never exposes text content (Principle V).
#[derive(Debug, Clone)]
pub struct InjectionTarget {
    secure: bool,
    id: String,
}

impl InjectionTarget {
    /// Build a target handle (backends construct this from the focused context).
    pub fn new(id: impl Into<String>, secure: bool) -> Self {
        Self { secure, id: id.into() }
    }

    /// Whether the focused field is a password/secure field (injection refused).
    pub fn is_secure(&self) -> bool {
        self.secure
    }

    /// Opaque identity of the acquired surface (never its text).
    pub fn id(&self) -> &str {
        &self.id
    }
}

/// Focus/target-loss events for the acquired target, so the controller can end
/// safely (FR-014, FR-022).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusEvent {
    /// Focus moved off the acquired target — finalize already-committed text and
    /// end the session (never retarget mid-session).
    FocusOut,
    /// The acquired target's window/context is gone — cancel safely.
    TargetGone,
}

/// Why an injection operation failed.
#[derive(Debug, thiserror::Error)]
pub enum InjectError {
    /// The focused field is a password/secure field — refuse to inject (FR-021).
    #[error("focused field is secure (password); refusing to inject")]
    SecureField,
    /// Nothing editable is focused — a clear failure, not a silent no-op (FR-023).
    #[error("no editable target is focused")]
    NoTarget,
    /// The injection backend is not reachable (e.g. IBus daemon down).
    #[error("injection backend unavailable: {0}")]
    Unavailable(String),
    /// A backend-specific failure (with context).
    #[error("injection backend error: {0}")]
    Backend(String),
}

/// The text-injection seam. All mutating operations are async; the focus stream
/// is `'static` so the controller can own it while still driving the injector.
#[async_trait]
pub trait Injector: Send {
    /// Bind the surface focused *now* as the session target. `Err(SecureField)`
    /// where a password purpose is detectable; `Err(NoTarget)` where nothing
    /// editable is focused; `Err(Unavailable)` where the backend is unreachable.
    async fn acquire(&mut self) -> Result<InjectionTarget, InjectError>;

    /// Reflect recording/transcription activity on the injection channel where
    /// the backend supports it (no-op otherwise).
    async fn set_activity(&mut self, active: bool);

    /// Insert stable committed text (never modified afterwards). Commit-only.
    async fn commit(&mut self, text: &str) -> Result<(), InjectError>;

    /// FUTURE (streaming preedit, R9): render a volatile in-flight hypothesis in
    /// the target's preedit region, replaced on the next call and cleared by
    /// `commit`. Default no-op; only backends with a real preedit region honor
    /// it. NOT called in the commit-only MVP.
    async fn set_preedit(&mut self, _text: &str) {}

    /// Whether this backend has a replacement-safe preedit region (IBus / future
    /// Wayland input-method-v2 → true; uinput/wtype fallback → false).
    fn supports_preedit(&self) -> bool {
        false
    }

    /// Abort without injecting anything further. Idempotent.
    async fn cancel(&mut self);

    /// Finalize and release the target/engine. Idempotent.
    async fn end(&mut self);

    /// Take the focus/target-loss event stream for the acquired target. Returns
    /// an owned (`'static`) stream so the controller can select on it while
    /// continuing to drive the injector (`commit`/`end`). Call once per session.
    fn focus_events(&mut self) -> BoxStream<'static, FocusEvent>;
}
