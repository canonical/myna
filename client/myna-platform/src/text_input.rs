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
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::stream::BoxStream;

/// Whether a backend can answer a question at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    Supported,
    /// The backend cannot tell; callers decide what to do without the answer.
    Unknown,
}

/// What the focused field says it is for, when it says.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FieldPurpose {
    /// Free-form text, or a field that says nothing more specific.
    #[default]
    Plain,
    Url,
    Email,
    Terminal,
    /// A purpose Myna does not distinguish (`ALPHA`, `DIGITS`, `NUMBER`,
    /// `PHONE`, `NAME`, date/time).
    Other,
}

/// What a text input backend can do, known without connecting to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextInputCapabilities {
    /// A replacement-safe preedit region for volatile hypotheses.
    pub preedit: bool,
    /// [`Target::char_before_cursor`] can answer for fields that say.
    pub surrounding_text: bool,
    /// [`Target::field_purpose`] can answer for fields that say.
    pub field_purpose: bool,
    /// Whether a secure field (password, PIN) can be recognised. Where it is
    /// [`Support::Unknown`] a secure field looks like any other.
    pub secure_field_detection: Support,
}

impl TextInputCapabilities {
    /// Commit only, nothing known about the field.
    pub const COMMIT_ONLY: Self = Self {
        preedit: false,
        surrounding_text: false,
        field_purpose: false,
        secure_field_detection: Support::Unknown,
    };
}

fn is_control_or_break(c: char) -> bool {
    c.is_control() || matches!(c, '\u{2028}' | '\u{2029}')
}

/// Replace control characters and Unicode line/paragraph separators with
/// single ASCII spaces, collapsing adjacent ASCII spaces so folded breaks
/// never concatenate tokens (`"rm -rf /tmp/x\nsudo reboot"` becomes
/// `"rm -rf /tmp/x sudo reboot"`). Leading and trailing spaces introduced by
/// control characters are trimmed while intentional leading or trailing `' '`
/// in clean text is kept.
pub fn fold_control_chars(text: &str) -> String {
    let trim_start = text.chars().next().is_some_and(is_control_or_break);
    let trim_end = text.chars().next_back().is_some_and(is_control_or_break);
    let mut out = String::with_capacity(text.len());
    let mut prev_space = trim_start;
    for c in text.chars() {
        if is_control_or_break(c) || c == ' ' {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    if trim_end {
        out.pop();
    }
    out
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

    /// How to tell this backend of the user's activation as it happens, a
    /// held key's repeats included, whatever the caller is busy with: also
    /// while it acquires, when there is no target to call
    /// [`Target::activated`] on, as a start key held down may keep focus off
    /// the field until it is released.
    fn activation(&self) -> Activation {
        Activation::none()
    }
}

/// A handle that tells a backend the user's activation continues, as a held
/// key's repeats do. Cheap to clone, and usable while the backend is busy
/// acquiring. Signalled with nothing being acquired or held, it does nothing.
#[derive(Clone)]
pub struct Activation(Arc<dyn Fn() + Send + Sync>);

impl Activation {
    pub fn new(signal: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(signal))
    }

    /// For a backend that needs no word of it.
    pub fn none() -> Self {
        Self::new(|| {})
    }

    pub fn signal(&self) {
        (self.0)();
    }
}

impl fmt::Debug for Activation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Activation")
    }
}

/// The sole owner of one utterance's right to write into the acquired field.
#[async_trait]
pub trait Target: Send + fmt::Debug {
    /// Insert stable text, never modified afterwards; clears any preedit.
    /// Into a [`FieldPurpose::Terminal`] target, control characters are folded
    /// through [`fold_control_chars`] before insertion so a newline or escape
    /// never executes or alters a command. `Err(FocusLost)` once the right to
    /// write is gone; `Err(SecureField)` when the field turned out secure only
    /// after `acquire`. While focus is away and may be coming back
    /// ([`Target::activated`]) the text is held: it lands once focus is back,
    /// or is dropped with the loss.
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

    /// What the focused field says it is for, when it says;
    /// [`FieldPurpose::Plain`] wherever it does not.
    fn field_purpose(&self) -> FieldPurpose {
        Default::default()
    }

    /// Yields a [`FocusEvent`] once the right to write is gone, including
    /// when it was lost before this call.
    fn focus_events(&self) -> BoxStream<'static, FocusEvent>;

    /// The user used Myna's activation (its shortcut or toggle) while this
    /// target is held. Activating may take focus off the field until the key
    /// is released, as an X11 key grab does. Focus that leaves within a short
    /// window of an activation and comes back to the same field soon after
    /// is not a loss: writes in between are held and land once it is back.
    /// A held key's repeats call this again for as long as it is held, and
    /// the window lasts as long. Focus leaving at any other time is a loss,
    /// as is focus that does not come back.
    fn activated(&self) {}

    /// Give up the field: finish any write held for an unresolved blip, clear
    /// what the field shows, and hand the desktop back whatever acquiring
    /// took. Returns `Err` when text held for a blip could not be delivered
    /// (for instance `Err(FocusLost)` when the blip turned out to be a loss);
    /// the desktop is restored either way.
    async fn release(self: Box<Self>) -> Result<(), InjectError>;
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

        async fn release(self: Box<Self>) -> Result<(), InjectError> {
            Ok(())
        }
    }

    #[test]
    fn a_field_that_says_nothing_has_no_char_before_the_cursor() {
        assert_eq!(Silent.char_before_cursor(), None);
        assert_eq!(Silent.field_purpose(), FieldPurpose::Plain);
    }

    #[test]
    fn activation_signals_its_callback_and_formats_without_capturing_state() {
        let called = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = std::sync::Arc::clone(&called);
        let activation = Activation::new(move || {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        activation.signal();
        assert_eq!(called.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(format!("{activation:?}"), "Activation");
        Activation::none().signal();
    }

    #[test]
    fn a_backend_that_says_nothing_is_commit_only() {
        assert_eq!(
            Bare.capabilities(),
            TextInputCapabilities {
                preedit: false,
                surrounding_text: false,
                field_purpose: false,
                secure_field_detection: Support::Unknown,
            }
        );
    }

    #[test]
    fn control_characters_fold_to_single_word_breaks_without_joining_tokens() {
        let cases = [
            ("rm -rf /tmp/x\nsudo reboot", "rm -rf /tmp/x sudo reboot"),
            ("rm -rf /tmp/x\r\nsudo reboot", "rm -rf /tmp/x sudo reboot"),
            (
                "rm -rf /tmp/x \t\n\t sudo reboot",
                "rm -rf /tmp/x sudo reboot",
            ),
            ("echo\x1b[31mred\x08\x7f\u{85}end", "echo [31mred end"),
            ("line\u{2028}two\u{2029}three", "line two three"),
            ("\nBonjour.\n", "Bonjour."),
            ("Bonjour. \n", "Bonjour."),
            ("\n Bonjour.", "Bonjour."),
            ("\n\r\t\x1b", ""),
            ("  \n", ""),
            ("\n  ", ""),
            ("", ""),
            (" hello", " hello"),
            ("w001 ", "w001 "),
            (" second\nthird.", " second third."),
            (
                "\u{ab}\u{00a0}Bonjour\u{202f}!\u{00a0}\u{bb}\n",
                "\u{ab}\u{00a0}Bonjour\u{202f}!\u{00a0}\u{bb}",
            ),
        ];
        for (input, expected) in cases {
            let folded = fold_control_chars(input);
            assert_eq!(folded, expected, "input: {input:?}");
            assert!(
                !folded.chars().any(char::is_control),
                "control survived in {folded:?}"
            );
            assert_eq!(
                fold_control_chars(&folded),
                folded,
                "not idempotent on {input:?}"
            );
        }
    }

    #[test]
    fn format_characters_survive_while_controls_still_fold() {
        let keep = [
            "\u{0645}\u{06cc}\u{200c}\u{062e}\u{0648}\u{0627}\u{0647}\u{0645}",
            "a\u{200d}b",
            "\u{200f}\u{0633}\u{0644}\u{0627}\u{0645}\u{200f} hello \u{200e}",
            "x\u{202a}y\u{202b}z\u{202c}\u{202d}\u{202e}",
        ];
        for input in keep {
            assert_eq!(fold_control_chars(input), input, "input: {input:?}");
        }
        assert_eq!(fold_control_chars("a\u{200c}\nb"), "a\u{200c} b");
        assert_eq!(
            fold_control_chars("\u{200f}a\r\n\u{200e}b"),
            "\u{200f}a \u{200e}b"
        );
    }
}
