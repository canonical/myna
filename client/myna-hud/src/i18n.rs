//! i18n — the `myna` gettext domain's constants (feature 004, T133; R25).
//!
//! The translated strings live in `client/myna-hud/po/`; this module exists
//! so both the binary's domain binding and any future extraction tooling
//! reference one spelling of the domain. There is deliberately no logic
//! here — gettext is bound in `main.rs` (or left unbound, where it is the
//! identity function).

/// The gettext domain for myna's translated strings.
pub const DOMAIN: &str = "myna";
