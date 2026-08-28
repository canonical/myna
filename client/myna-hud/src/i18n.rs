//! i18n — the `myna` gettext domain's constants (feature 004, T133; R25).
//!
//! The translated strings live in `client/myna-hud/po/`; this module exists
//! so both the binary's domain binding and any future extraction tooling
//! reference one spelling of the domain. There is deliberately no logic
//! here — gettext is bound in `main.rs` (or left unbound, where it is the
//! identity function).

/// The gettext domain for myna's translated strings.
pub const DOMAIN: &str = "myna";

/// Marker for a translatable string that is looked up by VARIABLE later
/// (the port of gettext's `N_` / GJS's `N_()`).
///
/// [`gettext`](gettextrs::gettext) is often called with a msgid that flows
/// through a variable (e.g. `states.rs` passes the msgid from a match arm),
/// which xgettext cannot see. Wrapping the literal in [`N_`] marks it for
/// extraction while being a no-op at runtime — the string is then translated
/// when `gettext()` finally runs on it. Extraction:
///
/// ```sh
/// xgettext --keyword=gettext --keyword=N_ --files-from=po/POTFILES.in ...
/// ```
pub fn n_(msgid: &str) -> &str {
    msgid
}
