//! Gettext domain for user-visible strings owned by this crate.

/// The gettext domain for strings defined in this crate. A separate domain
/// from the desktop's, so the two can be translated and shipped independently.
/// The embedding application initializes it through gettextrs' own
/// `TextDomain::init()` (which binds it wherever the catalog lives — the
/// default data dirs, or a pushed override); a consumer that never does falls
/// back to gettext's identity, which is always safe.
pub const GETTEXT_DOMAIN: &str = "myna-orchestrator";

/// Translate `msgid` through this crate's domain. With no .mo installed
/// gettext is the identity function, so this never fails and callers can
/// format the result as if it were the source string.
pub fn tr(msgid: &str) -> String {
    gettextrs::dgettext(GETTEXT_DOMAIN, msgid)
}
