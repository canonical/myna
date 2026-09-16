//! The text-injection boundary (plan T22, UD129 Text Injection Layer).
//!
//! Backend-agnostic (FR-016): the controller drives committed transcripts at an
//! [`Injector`] without knowing whether the target is filled via IBus, a future
//! Wayland `input_method_v2`, or a uinput fallback. [`ibus::IbusInjector`] is the
//! shipped implementor; [`mock::MockInjector`] is the hermetic test fixture.

use std::fmt;

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use gettextrs::gettext;

pub mod ibus;
pub mod lazy;
pub mod mock;

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
///
/// `Display` renders user-facing messages through the desktop gettext domain;
/// with no .mo installed it is the identity, so the strings below double as
/// the source templates for translation.
#[derive(Debug)]
pub enum InjectError {
    /// The focused field is a password/secure field — refuse to inject (FR-021).
    SecureField,
    /// Nothing editable is focused — a clear failure, not a silent no-op (FR-023).
    NoTarget,
    /// Focus left the acquired target; its right to write is gone (FR-014).
    FocusLost,
    /// The injection backend is not reachable (e.g. IBus daemon down).
    Unavailable(String),
    /// A backend-specific failure (with context).
    Backend(String),
}

impl fmt::Display for InjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InjectError::SecureField => {
                write!(
                    f,
                    "{}",
                    gettext("focused field is secure (password); refusing to inject")
                )
            }
            InjectError::NoTarget => {
                write!(f, "{}", gettext("no editable target is focused"))
            }
            InjectError::FocusLost => write!(f, "{}", gettext("Focus lost")),
            InjectError::Unavailable(inner) => write!(
                f,
                "{}",
                gettext("injection backend unavailable: %s").replace("%s", inner)
            ),
            InjectError::Backend(inner) => write!(
                f,
                "{}",
                gettext("injection backend error: %s").replace("%s", inner)
            ),
        }
    }
}

impl std::error::Error for InjectError {}

/// The text-injection seam: hands out one [`Target`] per utterance.
#[async_trait]
pub trait Injector: Send {
    /// Bind the surface focused *now* as the utterance's target. All or
    /// nothing: on error, whatever was taken (the engine switch) is already
    /// rolled back, as far as the backend can roll it back - IBus can only
    /// restore an input method it was able to read. `Err(SecureField)` where the field's content type is
    /// detectably secure; `Err(NoTarget)` where nothing editable is focused;
    /// `Err(FocusLost)` where focus moved while acquiring; `Err(Unavailable)`
    /// where the backend is unreachable.
    async fn acquire(&mut self) -> Result<Box<dyn Target>, InjectError>;

    /// Whether this backend has a replacement-safe preedit region (IBus / future
    /// Wayland input-method-v2 → true; uinput/wtype fallback → false).
    fn supports_preedit(&self) -> bool {
        false
    }
}

/// The sole owner of one utterance's right to write into the acquired
/// surface. The right ends when focus leaves it, when a newer target is
/// acquired, or at [`Target::release`]; every output operation checks it.
#[async_trait]
pub trait Target: Send + fmt::Debug {
    /// Insert stable committed text (never modified afterwards). Commit-only.
    /// `Err(FocusLost)` once the right to write is gone; `Err(SecureField)`
    /// when the target's secure state became known only after `acquire`
    /// (late content-type delivery, I5, FR-021).
    async fn commit(&mut self, text: &str) -> Result<(), InjectError>;

    /// Streaming preedit (R9): render a volatile in-flight hypothesis in the
    /// target's preedit region, replaced on the next call and cleared by
    /// `commit` (empty string clears explicitly). Skipped once the right to
    /// write is gone or the field is secure. The controller calls this only
    /// when its opt-in preedit mode is on AND the injector
    /// `supports_preedit()` - the commit-only default (FR-012) never routes
    /// unstable text here.
    async fn set_preedit(&mut self, _text: &str) {}

    /// Yields `FocusOut` once the right to write is gone, including when it
    /// was lost before this call.
    fn focus_events(&self) -> BoxStream<'static, FocusEvent>;

    /// Give up the target: clear what it shows and restore the input method
    /// it displaced.
    async fn release(self: Box<Self>);
}
