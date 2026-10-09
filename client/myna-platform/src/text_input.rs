//! Text input: writing dictated text into the field the user focused.
//!
//! An [`Injector`] hands out one [`Target`] per utterance, the sole owner of
//! the right to write into the field focused when it was acquired. The right
//! ends when focus leaves that field, when a newer target is acquired, or at
//! [`Target::release`]; every output operation checks it. Focus that leaves
//! around an activation and comes straight back to the same field, as an X11
//! key grab sends it while Myna's shortcut is held, is a blip, not a loss
//! ([`Target::activated`]).

use std::fmt;

use async_trait::async_trait;
use futures_util::stream::BoxStream;

/// Whether a backend can answer a question at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    Supported,
    /// The backend cannot tell; callers decide what to do without the answer.
    Unknown,
}

/// What a text input backend can do, known without connecting to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextInputCapabilities {
    /// A replacement-safe preedit region for volatile hypotheses.
    pub preedit: bool,
    /// [`Target::char_before_cursor`] can answer for fields that say.
    pub surrounding_text: bool,
    /// Whether a secure field (password, PIN) can be recognised. Where it is
    /// [`Support::Unknown`] a secure field looks like any other.
    pub secure_field_detection: Support,
}

impl TextInputCapabilities {
    /// Commit only, nothing known about the field.
    pub const COMMIT_ONLY: Self = Self {
        preedit: false,
        surrounding_text: false,
        secure_field_detection: Support::Unknown,
    };
}

/// Focus or target loss on an acquired target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusEvent {
    /// Focus moved off the target: keep what was committed, end the utterance,
    /// never retarget.
    FocusOut,
    /// The target's window or context is gone.
    TargetGone,
}

/// Why a text input operation failed. `Display` is the untranslated detail
/// for logs; what the user reads is the caller's to word.
#[derive(Debug)]
pub enum InjectError {
    /// The focused field is secure; nothing is written to it.
    SecureField,
    /// Nothing editable is focused.
    NoTarget,
    /// Focus left the acquired target; its right to write is gone.
    FocusLost,
    /// The backend cannot be reached.
    Unavailable(String),
    /// Any other backend failure, with context.
    Backend(String),
}

impl fmt::Display for InjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InjectError::SecureField => {
                write!(f, "focused field is secure (password); refusing to inject")
            }
            InjectError::NoTarget => write!(f, "no editable target is focused"),
            InjectError::FocusLost => write!(f, "focus left the dictation target"),
            InjectError::Unavailable(inner) => write!(f, "injection backend unavailable: {inner}"),
            InjectError::Backend(inner) => write!(f, "injection backend error: {inner}"),
        }
    }
}

impl std::error::Error for InjectError {}

/// Hands out one [`Target`] per utterance.
#[async_trait]
pub trait Injector: Send {
    /// Bind the field focused now as the utterance's target. All or nothing:
    /// on error, whatever acquiring changed on the desktop is already undone,
    /// as far as the backend can undo it. `Err(SecureField)` where the field
    /// is detectably secure; `Err(NoTarget)` where nothing editable is
    /// focused; `Err(FocusLost)` where focus moved while acquiring;
    /// `Err(Unavailable)` where the backend is unreachable.
    async fn acquire(&mut self) -> Result<Box<dyn Target>, InjectError>;

    /// What this backend can do. Answered without a live field.
    fn capabilities(&self) -> TextInputCapabilities {
        TextInputCapabilities::COMMIT_ONLY
    }
}

/// The sole owner of one utterance's right to write into the acquired field.
#[async_trait]
pub trait Target: Send + fmt::Debug {
    /// Insert stable text, never modified afterwards; clears any preedit.
    /// `Err(FocusLost)` once the right to write is gone; `Err(SecureField)`
    /// when the field turned out secure only after `acquire`. While focus is
    /// away and may be coming back ([`Target::activated`]) the text is held:
    /// it lands once focus is back, or is dropped with the loss.
    async fn commit(&mut self, text: &str) -> Result<(), InjectError>;

    /// Show a volatile hypothesis in the field's preedit region, replacing
    /// the previous one; an empty string clears it. Nothing is shown once the
    /// right to write is gone or the field is secure. Callers use it only
    /// where [`TextInputCapabilities::preedit`] holds; elsewhere it does
    /// nothing.
    async fn set_preedit(&mut self, _text: &str) {}

    /// The character before the cursor, when the field says. `None` at its
    /// start and wherever it does not say.
    fn char_before_cursor(&self) -> Option<char> {
        None
    }

    /// Yields a [`FocusEvent`] once the right to write is gone, including
    /// when it was lost before this call.
    fn focus_events(&self) -> BoxStream<'static, FocusEvent>;

    /// The user used Myna's activation (its shortcut or toggle) while this
    /// target is held. Activating may take focus off the field until the key
    /// is released, as an X11 key grab does. Focus that leaves within a short
    /// window of an activation and comes back to the same field soon after
    /// is not a loss: writes in between are held and land once it is back.
    /// Focus leaving at any other time is a loss, as is focus that does not
    /// come back.
    fn activated(&self) {}

    /// Give up the field: clear what it shows and hand the desktop back
    /// whatever acquiring took.
    async fn release(self: Box<Self>);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_say_what_failed() {
        let cases = [
            (InjectError::SecureField, "focused field is secure"),
            (InjectError::NoTarget, "no editable target is focused"),
            (InjectError::FocusLost, "focus left the dictation target"),
            (
                InjectError::Unavailable("no daemon".into()),
                "injection backend unavailable: no daemon",
            ),
            (
                InjectError::Backend("boom".into()),
                "injection backend error: boom",
            ),
        ];
        for (error, detail) in cases {
            assert!(error.to_string().starts_with(detail), "{error:?}");
        }
    }

    struct Bare;

    #[async_trait]
    impl Injector for Bare {
        async fn acquire(&mut self) -> Result<Box<dyn Target>, InjectError> {
            Err(InjectError::NoTarget)
        }
    }

    #[derive(Debug)]
    struct Silent;

    #[async_trait]
    impl Target for Silent {
        async fn commit(&mut self, _text: &str) -> Result<(), InjectError> {
            Ok(())
        }

        fn focus_events(&self) -> BoxStream<'static, FocusEvent> {
            Box::pin(futures_util::stream::pending())
        }

        async fn release(self: Box<Self>) {}
    }

    #[test]
    fn a_field_that_says_nothing_has_no_char_before_the_cursor() {
        assert_eq!(Silent.char_before_cursor(), None);
    }

    #[test]
    fn a_backend_that_says_nothing_is_commit_only() {
        assert_eq!(
            Bare.capabilities(),
            TextInputCapabilities {
                preedit: false,
                surrounding_text: false,
                secure_field_detection: Support::Unknown,
            }
        );
    }
}
