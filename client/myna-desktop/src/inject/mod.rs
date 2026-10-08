//! The text-injection boundary (plan T22, UD129 Text Injection Layer).
//!
//! The contract is `myna_platform::text_input`; this module holds its backends.
//! [`ibus::IbusInjector`] is the shipped implementor; [`mock::MockInjector`] is
//! the hermetic test fixture.

use gettextrs::gettext;

pub use myna_platform::text_input::{
    FocusEvent, InjectError, Injector, Support, Target, TextInputCapabilities,
};

pub mod ibus;
pub mod lazy;
pub mod mock;

/// What the user is told about an [`InjectError`], translated through the
/// desktop gettext domain.
pub trait Headline {
    fn headline(&self) -> String;
}

impl Headline for InjectError {
    fn headline(&self) -> String {
        match self {
            InjectError::SecureField => gettext("Password field skipped"),
            InjectError::NoTarget => gettext("No text field focused"),
            InjectError::FocusLost => gettext("Focus lost"),
            InjectError::Unavailable(_) => gettext("Typing unavailable"),
            InjectError::Backend(_) => gettext("Typing failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Headline, InjectError};

    #[test]
    fn every_inject_error_has_its_headline() {
        let cases = [
            (InjectError::SecureField, "Password field skipped"),
            (InjectError::NoTarget, "No text field focused"),
            (InjectError::FocusLost, "Focus lost"),
            (InjectError::Unavailable("x".into()), "Typing unavailable"),
            (InjectError::Backend("x".into()), "Typing failed"),
        ];
        for (error, headline) in cases {
            assert_eq!(error.headline(), headline);
            assert_ne!(error.to_string(), headline, "the detail says more");
        }
        assert_eq!(
            InjectError::Unavailable("no ibus".into()).to_string(),
            "injection backend unavailable: no ibus"
        );
    }
}
