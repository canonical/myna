//! Gettext domain for user-visible strings owned by this crate.
//!
//! Today that is the [`crate::failure`] registry: the plain-language failure
//! text every surface renders (feature 011-accessible-dictation-ux, US4). A
//! separate domain from `myna-desktop`'s and `myna-orchestrator`'s, so the
//! three can be translated and shipped independently, and so a consumer that
//! links only this crate still resolves its own strings.

/// The gettext domain for strings defined in this crate. The embedding
/// application initializes it through gettextrs' own `TextDomain::init()`; a
/// consumer that never does falls back to gettext's identity, which is always
/// safe.
pub const GETTEXT_DOMAIN: &str = "myna-core";

/// Translate a `'static` msgid through this crate's domain, keeping the
/// `&'static str` the caller started with.
///
/// This exists because [`crate::failure::FailurePresentation`] stores
/// `&'static str`, not `String`: the registry hands out `&'static` references
/// (so an `IndicatorState::Error` can hold one without threading a lifetime)
/// and precomputes a `&'static` `spoken` form for `AnnouncementText`, which
/// deliberately has no non-`'static` constructor. Returning `String` here
/// would push an owned allocation through all of that.
///
/// **When a translation exists it is leaked.** That is bounded and deliberate:
/// every call site is a presentation built inside
/// [`crate::failure::default_registry`], which a `OnceLock` runs exactly once
/// per process, and which already leaks each presentation for the same
/// reason. It is never called in a loop or per event.
///
/// **The identity case allocates nothing.** With no catalog installed gettext
/// returns the msgid unchanged and this hands back the original `'static`
/// pointer, so an untranslated build pays literally nothing.
///
/// Because translation happens when the registry is built rather than at each
/// lookup, the process locale must already be set by then. It is: the registry
/// is built lazily on first failure, and `myna-desktop`'s `init_i18n()` — which
/// sets the locale — is the first statement in `main()`.
pub fn tr(msgid: &'static str) -> &'static str {
    let translated = gettextrs::dgettext(GETTEXT_DOMAIN, msgid);
    if translated == msgid {
        msgid
    } else {
        Box::leak(translated.into_boxed_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With no catalog installed, `tr` is the identity *and* does not
    /// allocate: it returns the very pointer it was handed. This is the case
    /// every test and every untranslated build takes, so it is worth pinning
    /// - a `tr` that leaked here would leak once per registry build.
    #[test]
    fn untranslated_returns_the_original_pointer() {
        let msgid = "No text field is selected.";
        assert!(std::ptr::eq(tr(msgid), msgid));
    }
}
