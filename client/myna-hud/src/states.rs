//! states — PURE dictation-state → *semantic descriptor* (feature
//! 004-gnome-shell-indicator; data-model E1/E1a). This is the STABLE layer:
//! it maps the `org.myna.Dictation` wire `State` to a content-free,
//! presentation-free descriptor. It says *what* the system is doing — never
//! how to draw it. Renderers own all pixels: colour, geometry, animation,
//! icon choice. Swapping the look never touches this file.
//!
//! Ported 1:1 from `extensions/myna-shell/states.js` (deleted with the old
//! bundle; this is now the single source of truth). The user-facing
//! `status_text` strings go through gettext (domain `myna`, R25) — with no
//! bound domain gettext is the identity function, so the tests assert the
//! English source, exactly like the GJS suite did.
//!
//! Nothing here ever carries transcript text (constitution V, X6): the only
//! caller-controlled text is the content-free `reason` (E3), and it can flow
//! solely into the two problem states' status lines.

use gettextrs::gettext;

/// The E1 wire-state string constants (additive contract,
/// `contracts/dbus-interface.md`). Mirrors `myna-desktop`'s
/// `indicator::dbus::wire_state` — the consumer-side copy, the same way the
/// GJS extension carried its own.
pub mod wire {
    pub const IDLE: &str = "idle";
    pub const LOADING: &str = "loading";
    pub const RECORDING: &str = "recording";
    pub const TRANSCRIBING: &str = "transcribing";
    pub const FINALIZING: &str = "finalizing";
    pub const NOTICE: &str = "notice";
    pub const ERROR: &str = "error";
}

/// Problem-tier classification (data-model E1a): a **recoverable** notice
/// auto-dismisses and never blocks a new session; a **critical** error
/// persists until the user dismisses it. Realized on the wire as the choice
/// between `notice` and `error` (E1), not a separate property.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Recoverable,
    Critical,
}

/// The machine key of a [`Descriptor`]: the known wire states plus the
/// neutral `Active` an unknown value degrades to (FR-008/X2) and `Idle` for
/// the hidden case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DictationState {
    Idle,
    Loading,
    Recording,
    Transcribing,
    Finalizing,
    Notice,
    Error,
    Active,
}

/// The stable descriptor for a state (data-model E-mapping): a machine
/// `key` (renderers switch on this), a human, content-free `status_text`,
/// a `severity` (`Some` only for the two problem states), and `hidden`
/// (idle → nothing shown, FR-002/X3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Descriptor {
    pub key: DictationState,
    pub status_text: String,
    pub severity: Option<Severity>,
    pub hidden: bool,
}

fn hidden() -> Descriptor {
    Descriptor {
        key: DictationState::Idle,
        status_text: String::new(),
        severity: None,
        hidden: true,
    }
}

/// The stable base for each known state: msgid + severity. Additive: an
/// unknown value falls through to `Active`, never panics (X2).
fn base_for(state: &str) -> (DictationState, &'static str, Option<Severity>) {
    match state {
        wire::LOADING => (DictationState::Loading, "Loading model…", None),
        wire::RECORDING => (DictationState::Recording, "Listening", None),
        wire::TRANSCRIBING => (DictationState::Transcribing, "Transcribing", None),
        wire::FINALIZING => (DictationState::Finalizing, "Finishing", None),
        wire::NOTICE => (
            DictationState::Notice,
            "No speech detected",
            Some(Severity::Recoverable),
        ),
        wire::ERROR => (DictationState::Error, "Error", Some(Severity::Critical)),
        _ => (DictationState::Active, "Active", None),
    }
}

/// Map a wire `State` string (E1) to a semantic [`Descriptor`].
///
/// Port of `states.js`'s `stateToDescriptor`:
/// * `None` (no State property) and `idle` → the hidden descriptor
///   (push-to-talk, FR-002/X3).
/// * An **unknown** value degrades to the neutral "active" descriptor
///   (FR-008/X2).
/// * `reason` — a content-free reason for a `notice`/`error` state (E3);
///   ignored for every other state so caller text can never leak into an
///   unrelated status (X6). `notice`'s reason is shown as-is (it isn't an
///   error, so no "Error —" prefix); `error`'s reason is appended after that
///   prefix, matching the pre-2026-07-30 behavior.
pub fn state_to_descriptor(state: Option<&str>, reason: &str) -> Descriptor {
    let Some(state) = state else {
        return hidden();
    };
    if state == wire::IDLE {
        return hidden();
    }

    let (key, msgid, severity) = base_for(state);
    let mut status_text = gettext(msgid);
    if !reason.is_empty() {
        match key {
            // Translated printf-style template, %s substituted (the GJS
            // version used GLib's String.format on the translated string).
            DictationState::Error => {
                status_text = gettext("Error — %s").replace("%s", reason);
            }
            DictationState::Notice => status_text = reason.to_string(),
            _ => {}
        }
    }
    Descriptor {
        key,
        status_text,
        severity,
        hidden: false,
    }
}
