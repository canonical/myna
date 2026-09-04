//! states — PURE dictation-state → *semantic descriptor* (feature
//! 004-gnome-shell-indicator; data-model E1/E1a; contracts RC1, RC2, RC6).
//! This is the STABLE layer: it maps the `com.canonical.Myna.Dictation`
//! wire `State` to a content-free, presentation-free descriptor. It says
//! *what* the system is doing — never how to draw it. Renderers own all
//! pixels: colour, geometry, animation, icon choice. Swapping the look
//! never touches this file.
//!
//! Nothing here ever carries transcript text (constitution V, RC6). The
//! publisher owns the content-free `StatusMessage` label for every visible
//! state, so this module preserves it without translating or reformatting it.

/// The E1 wire-state string constants (additive contract,
/// `contracts/dbus-interface.md`). Mirrors `myna-desktop`'s
/// `indicator::dbus::wire_state` — the consumer-side copy.
pub mod wire {
    pub const IDLE: &str = "idle";
    pub const LOADING: &str = "loading";
    pub const RECORDING: &str = "recording";
    pub const TRANSCRIBING: &str = "transcribing";
    pub const FINALIZING: &str = "finalizing";
    pub const NOTICE: &str = "notice";
    pub const ERROR: &str = "error";

    /// Every defined wire value, for exhaustive iteration in tests.
    pub const ALL: [&str; 7] = [
        IDLE,
        LOADING,
        RECORDING,
        TRANSCRIBING,
        FINALIZING,
        NOTICE,
        ERROR,
    ];
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
/// neutral `Active` an unknown value degrades to (FR-008/RC2) and `Idle` for
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
/// (idle → nothing shown, FR-002/RC3).
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

/// The stable base for each known state: key + severity. Additive: an
/// unknown value falls through to `Active`, never panics (RC2).
fn base_for(state: &str) -> (DictationState, Option<Severity>) {
    match state {
        wire::LOADING => (DictationState::Loading, None),
        wire::RECORDING => (DictationState::Recording, None),
        wire::TRANSCRIBING => (DictationState::Transcribing, None),
        wire::FINALIZING => (DictationState::Finalizing, None),
        wire::NOTICE => (DictationState::Notice, Some(Severity::Recoverable)),
        wire::ERROR => (DictationState::Error, Some(Severity::Critical)),
        _ => (DictationState::Active, None),
    }
}

/// Map a wire `State` string (E1) to a semantic [`Descriptor`].
///
/// Port of `states.js`'s `stateToDescriptor`:
/// * `None` (no State property) and `idle` → the hidden descriptor
///   (push-to-talk, FR-002/RC3).
/// * An **unknown** value degrades to the neutral "active" descriptor
///   (FR-008/RC2).
/// * `status_message` is the publisher-owned, content-free label for every
///   visible state (C3/RC6). It is displayed verbatim; this client does not
///   own a second translation or formatting table.
pub fn state_to_descriptor(state: Option<&str>, status_message: &str) -> Descriptor {
    let Some(state) = state else {
        return hidden();
    };
    if state == wire::IDLE {
        return hidden();
    }

    let (key, severity) = base_for(state);
    Descriptor {
        key,
        status_text: status_message.to_string(),
        severity,
        hidden: false,
    }
}
