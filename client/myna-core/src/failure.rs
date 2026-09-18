//! Plain-language failure presentations (feature 011-accessible-dictation-ux,
//! US4, FR-023-027, `specs/011-accessible-dictation-ux/contracts/failure-mapping.md`).
//!
//! One `FailurePresentation` authored once per known failure source, rendered
//! identically by the indicator, notification, terminal, and accessibility-
//! announcement surfaces (FR-024) - no surface introduces its own wording
//! (FR-024a). Lives in `myna-core` (not `myna-desktop`) because `myna-cli`
//! (T071) needs the identical registry without depending on the desktop
//! daemon's D-Bus/IBus/GTK stack - both crates already depend on
//! `myna-core`.
//!
//! Two lookup shapes, matching the two kinds of failure source in this
//! codebase today (spec Assumptions: this maps onto today's ad-hoc failure
//! strings, not a not-yet-built stable taxonomy):
//! - **Closed-set concepts** (`inject::InjectError`'s variants,
//!   `myna_orchestrator::BackendError`'s connection-failure variants, "the
//!   dictation target closed"): each has a fixed, compile-time-known `id`,
//!   looked up with [`lookup`]/[`FailureRegistry::lookup`].
//! - **Open-ended wire codes** (`OrchestratorEvent::Error`/
//!   `SessionOutcome::Failed`'s `code` field, `BackendError::Rejected`'s
//!   `code`) - the backend can send a code this registry has never seen (it
//!   is not a closed enum today), so [`lookup_by_code`]/
//!   [`FailureRegistry::lookup_by_code`] always returns *something*
//!   plain-language rather than `None`: a known code gets its own entry
//!   (registered under an `id` equal to the wire code string itself, so no
//!   separate code-to-id translation table is needed), anything else falls
//!   back to [`UNKNOWN_BACKEND_FAILURE`].

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::i18n::tr;

/// A failure/notice severity (data-model.md), distinguishable without colour
/// (FR-025): `Critical` persists until acknowledged, `Recoverable`
/// auto-dismisses but remains retrievable afterward (FR-026).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Recoverable,
    Critical,
}

/// A single failure's plain-language presentation, authored once and
/// rendered identically everywhere (FR-024). `id` is the stable key two
/// different call sites for the same underlying concept both look up, so
/// they structurally cannot diverge in wording (FR-024a, contract F3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FailurePresentation {
    pub id: &'static str,
    pub message: &'static str,
    pub recovery_action: &'static str,
    pub severity: Severity,
}

impl FailurePresentation {
    /// Render this presentation as one line of text: the fixed message and
    /// recovery action, plus optional dynamic `detail` (never as a
    /// replacement for the plain-language primary text - FR-023) in
    /// parentheses. The single formatting rule every surface that needs an
    /// owned `String` (rather than the `&'static str`-only `spoken()`) goes
    /// through, so `myna-desktop`'s `IndicatorState::from_failure` and
    /// `myna-cli`'s terminal rendering (US4 T068/T071) cannot diverge in how
    /// they combine the same three pieces (contract F2/F3).
    pub fn render(&self, detail: Option<&str>) -> String {
        match detail {
            Some(d) if !d.is_empty() => {
                format!("{} {} ({d})", self.message, self.recovery_action)
            }
            _ => format!("{} {}", self.message, self.recovery_action),
        }
    }
}

/// The keyed registry every failure source looks up into (contract F1/F3).
#[derive(Debug, Default)]
pub struct FailureRegistry {
    presentations: HashMap<&'static str, &'static FailurePresentation>,
    /// A precomputed, leaked `"{message} {recovery_action}"` per id, so a
    /// caller that can only accept a `&'static str` (e.g.
    /// `accessibility::AnnouncementText`, which by design has no
    /// `From<String>`/non-`'static` constructor - see its doc comment) can
    /// still speak the *combined* meaning-and-recovery-action in a single
    /// announcement (FR-023/024, quickstart.md Scenario 2), without
    /// `FailurePresentation` itself carrying a third, hand-duplicated
    /// literal that could drift from `message`/`recovery_action`.
    spoken: HashMap<&'static str, &'static str>,
}

impl FailureRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a presentation, leaking it to `'static` so lookups can hand
    /// back a `'static` reference without threading the registry's own
    /// lifetime everywhere a presentation is stored (e.g.
    /// `indicator::IndicatorState::Error`). Only ever called against a
    /// fixed, finite set at startup (see [`default_registry`]), never in a
    /// loop, so the leak is bounded.
    pub fn register(&mut self, presentation: FailurePresentation) {
        let spoken: &'static str = Box::leak(
            format!("{} {}", presentation.message, presentation.recovery_action).into_boxed_str(),
        );
        let leaked: &'static FailurePresentation = Box::leak(Box::new(presentation));
        self.presentations.insert(leaked.id, leaked);
        self.spoken.insert(leaked.id, spoken);
    }

    /// Looks up a failure by its stable id. Two call sites naming the same
    /// `id` always get the identical `FailurePresentation` value back - the
    /// registry, not per-call-site authoring, is what makes F3 hold.
    pub fn lookup(&self, id: &str) -> Option<&'static FailurePresentation> {
        self.presentations.get(id).copied()
    }

    /// Looks up an open-ended wire code (contract F1), falling back to
    /// [`UNKNOWN_BACKEND_FAILURE`] for a code this registry has never
    /// registered - the wire's code vocabulary is not a closed set (spec
    /// Assumptions), so this never returns `None`.
    pub fn lookup_by_code(&self, code: &str) -> &'static FailurePresentation {
        self.lookup(code).unwrap_or_else(|| {
            self.lookup(UNKNOWN_BACKEND_FAILURE)
                .expect("UNKNOWN_BACKEND_FAILURE must always be registered by default_registry")
        })
    }

    /// The combined `"{message} {recovery_action}"` text for `id`, ready to
    /// pass directly to an `AnnouncementText`-typed announcer (see the
    /// `spoken` field doc comment).
    pub fn spoken(&self, id: &str) -> Option<&'static str> {
        self.spoken.get(id).copied()
    }

    /// [`Self::spoken`], keyed by an open-ended wire code (see
    /// [`Self::lookup_by_code`]'s fallback behavior).
    pub fn spoken_by_code(&self, code: &str) -> &'static str {
        self.spoken(code).unwrap_or_else(|| {
            self.spoken(UNKNOWN_BACKEND_FAILURE)
                .expect("UNKNOWN_BACKEND_FAILURE must always be registered by default_registry")
        })
    }
}

// ── Stable ids (contract F3: call sites name these, never ad-hoc strings) ──

pub const SECURE_FIELD: &str = "secure_field";
pub const NO_TARGET: &str = "no_target";
pub const INJECTION_UNAVAILABLE: &str = "injection_unavailable";
pub const INJECTION_BACKEND_ERROR: &str = "injection_backend_error";
pub const TARGET_CLOSED: &str = "target_closed";
pub const BACKEND_CONNECT: &str = "backend_connect";
pub const BACKEND_HANDSHAKE: &str = "backend_handshake";
pub const BACKEND_WIRE: &str = "backend_wire";
pub const BACKEND_CLOSED: &str = "backend_closed";
pub const BACKEND_TRANSPORT: &str = "backend_transport";
pub const UNKNOWN_BACKEND_FAILURE: &str = "unknown_backend_failure";
// Known wire codes seen from the backend today (`driver.rs`/`fsm.rs`) - each
// registered under an id equal to the code itself, so `lookup_by_code` needs
// no separate code→id translation table.
pub const CODE_INTERNAL: &str = "internal";
pub const CODE_CONNECTION_CLOSED: &str = "connection_closed";
pub const CODE_INFERENCE_FAILED: &str = "inference_failed";
pub const CODE_CAPTURE_FAILED: &str = "capture_failed";
// A long-running operation (model loading) past its threshold (FR-027, T066).
pub const MODEL_LOAD_SLOW: &str = "model_load_slow";

/// Build the registry populated with one entry per known failure source
/// (T067). Called once (see [`registry`]) - cheap, a handful of entries.
pub fn default_registry() -> FailureRegistry {
    let mut r = FailureRegistry::new();
    r.register(FailurePresentation {
        id: SECURE_FIELD,
        message: tr("This field is a password field, so dictation can't type into it."),
        recovery_action: tr("Select a text field that isn't a password field, then try again."),
        severity: Severity::Critical,
    });
    r.register(FailurePresentation {
        id: NO_TARGET,
        message: tr("No text field is selected."),
        recovery_action: tr("Click into a text field, then try again."),
        severity: Severity::Critical,
    });
    r.register(FailurePresentation {
        id: INJECTION_UNAVAILABLE,
        message: tr("The service that types text into other apps isn't available right now."),
        recovery_action: tr(
            "Check that your desktop's input method service is running, then try again.",
        ),
        severity: Severity::Critical,
    });
    r.register(FailurePresentation {
        id: INJECTION_BACKEND_ERROR,
        message: tr("Something went wrong while typing the text into the app."),
        recovery_action: tr("Try again. If it keeps happening, restart the dictation service."),
        severity: Severity::Critical,
    });
    r.register(FailurePresentation {
        id: TARGET_CLOSED,
        message: tr("The window you were dictating into closed or lost focus."),
        recovery_action: tr("Click back into a text field, then try again."),
        severity: Severity::Critical,
    });
    r.register(FailurePresentation {
        id: BACKEND_CONNECT,
        message: tr("The speech-recognition service can't be reached."),
        recovery_action: tr("Check that the dictation backend is running, then try again."),
        severity: Severity::Critical,
    });
    r.register(FailurePresentation {
        id: BACKEND_HANDSHAKE,
        message: tr(
            "The speech-recognition service didn't respond correctly when starting a session.",
        ),
        recovery_action: tr("Try again. If it keeps happening, restart the dictation service."),
        severity: Severity::Critical,
    });
    r.register(FailurePresentation {
        id: BACKEND_WIRE,
        message: tr("The speech-recognition service sent a message dictation didn't understand."),
        recovery_action: tr("Try again. If it keeps happening, restart the dictation service."),
        severity: Severity::Critical,
    });
    r.register(FailurePresentation {
        id: BACKEND_CLOSED,
        message: tr("The connection to the speech-recognition service closed unexpectedly."),
        recovery_action: tr("Try again. If it keeps happening, restart the dictation service."),
        severity: Severity::Critical,
    });
    r.register(FailurePresentation {
        id: BACKEND_TRANSPORT,
        message: tr("There was a problem communicating with the speech-recognition service."),
        recovery_action: tr("Try again. If it keeps happening, restart the dictation service."),
        severity: Severity::Critical,
    });
    r.register(FailurePresentation {
        id: UNKNOWN_BACKEND_FAILURE,
        message: tr("Something went wrong with the speech-recognition service."),
        recovery_action: tr("Try again. If it keeps happening, restart the dictation service."),
        severity: Severity::Critical,
    });
    r.register(FailurePresentation {
        id: CODE_INTERNAL,
        message: tr("Something went wrong inside the dictation service."),
        recovery_action: tr("Try again. If it keeps happening, restart the dictation service."),
        severity: Severity::Critical,
    });
    r.register(FailurePresentation {
        id: CODE_CONNECTION_CLOSED,
        message: tr("The connection to the speech-recognition service closed unexpectedly."),
        recovery_action: tr("Try again. If it keeps happening, restart the dictation service."),
        severity: Severity::Critical,
    });
    r.register(FailurePresentation {
        id: CODE_INFERENCE_FAILED,
        message: tr("Speech recognition failed for this utterance."),
        recovery_action: tr("Try again. If it keeps happening, restart the dictation service."),
        severity: Severity::Critical,
    });
    r.register(FailurePresentation {
        id: CODE_CAPTURE_FAILED,
        message: tr("The microphone couldn't be captured."),
        recovery_action: tr(
            "Check that a microphone is connected and not in use by another app, then try again.",
        ),
        severity: Severity::Critical,
    });
    r.register(FailurePresentation {
        id: MODEL_LOAD_SLOW,
        message: tr("Loading the speech-recognition model is taking longer than usual."),
        recovery_action: tr("Keep waiting, or restart dictation if this continues for a while."),
        severity: Severity::Recoverable,
    });
    r
}

/// The process-wide default registry (built once), for call sites that don't
/// need a custom/injected registry - matching every production failure
/// source through the identical, shared presentations (F3). Tests that need
/// to assert specific wording use [`default_registry`] directly instead.
fn registry() -> &'static FailureRegistry {
    static REGISTRY: OnceLock<FailureRegistry> = OnceLock::new();
    REGISTRY.get_or_init(default_registry)
}

/// Look up a closed-set failure id against the shared default registry.
pub fn lookup(id: &str) -> Option<&'static FailurePresentation> {
    registry().lookup(id)
}

/// Look up an open-ended wire code against the shared default registry,
/// falling back to [`UNKNOWN_BACKEND_FAILURE`] for an unrecognized code.
pub fn lookup_by_code(code: &str) -> &'static FailurePresentation {
    registry().lookup_by_code(code)
}

/// [`FailureRegistry::spoken`] against the shared default registry.
pub fn spoken(id: &str) -> Option<&'static str> {
    registry().spoken(id)
}

/// [`FailureRegistry::spoken_by_code`] against the shared default registry.
pub fn spoken_by_code(code: &str) -> &'static str {
    registry().spoken_by_code(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── T028 (existing scaffold, carried over verbatim): repeated lookups
    //    of the same id never diverge (F3) ───────────────────────────────────

    #[test]
    fn repeated_lookups_of_the_same_id_return_the_identical_presentation() {
        let mut registry = FailureRegistry::new();
        registry.register(FailurePresentation {
            id: "no_microphone",
            message: "No microphone is available.",
            recovery_action: "Connect a microphone and try again.",
            severity: Severity::Critical,
        });

        let first = registry.lookup("no_microphone");
        let second = registry.lookup("no_microphone");
        assert_eq!(first, second);
        assert!(first.is_some());
    }

    #[test]
    fn unknown_id_returns_none() {
        let registry = FailureRegistry::new();
        assert_eq!(registry.lookup("nonexistent"), None);
    }

    // ── T063 groundwork: `spoken()` combines message + recovery_action into
    //    a single `'static` string an `AnnouncementText`-typed announcer can
    //    accept (contract F2, quickstart.md Scenario 2) ──────────────────────

    #[test]
    fn spoken_combines_message_and_recovery_action() {
        let mut registry = FailureRegistry::new();
        registry.register(FailurePresentation {
            id: "no_microphone",
            message: "No microphone is available.",
            recovery_action: "Connect a microphone and try again.",
            severity: Severity::Critical,
        });

        let spoken = registry.spoken("no_microphone").unwrap();
        assert!(spoken.contains("No microphone is available."));
        assert!(spoken.contains("Connect a microphone and try again."));
    }

    #[test]
    fn spoken_by_code_falls_back_like_lookup_by_code() {
        let r = default_registry();
        let spoken = r.spoken_by_code("some_future_code_this_registry_has_never_seen");
        assert_eq!(spoken, r.spoken(UNKNOWN_BACKEND_FAILURE).unwrap());
    }

    // ── T062: every known failure source has a registered, non-empty,
    //    plain-language presentation (F1) ───────────────────────────────────

    #[test]
    fn every_known_failure_source_has_a_non_empty_plain_language_presentation() {
        let r = default_registry();
        for id in [
            SECURE_FIELD,
            NO_TARGET,
            INJECTION_UNAVAILABLE,
            INJECTION_BACKEND_ERROR,
            TARGET_CLOSED,
            BACKEND_CONNECT,
            BACKEND_HANDSHAKE,
            BACKEND_WIRE,
            BACKEND_CLOSED,
            BACKEND_TRANSPORT,
            UNKNOWN_BACKEND_FAILURE,
            CODE_INTERNAL,
            CODE_CONNECTION_CLOSED,
            CODE_INFERENCE_FAILED,
            CODE_CAPTURE_FAILED,
            MODEL_LOAD_SLOW,
        ] {
            let p = r
                .lookup(id)
                .unwrap_or_else(|| panic!("{id} must be registered"));
            assert!(!p.message.is_empty(), "{id}'s message must not be empty");
            assert!(
                !p.recovery_action.is_empty(),
                "{id}'s recovery_action must not be empty"
            );
            // F1: no error codes/jargon/internal component names as primary
            // text - a cheap heuristic, not exhaustive: the message must not
            // itself just echo the id (which would be jargon).
            assert_ne!(
                p.message, id,
                "{id}'s message must be plain language, not its own id"
            );
        }
    }

    #[test]
    fn lookup_by_code_falls_back_to_unknown_backend_failure_for_an_unrecognized_code() {
        let r = default_registry();
        let fallback = r.lookup_by_code("some_future_code_this_registry_has_never_seen");
        assert_eq!(fallback.id, UNKNOWN_BACKEND_FAILURE);
    }

    #[test]
    fn lookup_by_code_finds_a_known_code_directly() {
        let r = default_registry();
        let p = r.lookup_by_code(CODE_INFERENCE_FAILED);
        assert_eq!(p.id, CODE_INFERENCE_FAILED);
    }

    // ── T064: two different call sites naming the same id never diverge ────

    #[test]
    fn two_call_sites_naming_the_same_id_never_diverge_in_wording() {
        let r = default_registry();
        let site_a = r.lookup(SECURE_FIELD).unwrap();
        let site_b = r.lookup(SECURE_FIELD).unwrap();
        assert_eq!(site_a, site_b);
    }

    #[test]
    fn the_shared_default_registry_matches_a_fresh_default_registry_for_every_known_id() {
        // The process-wide `registry()`/`lookup()`/`lookup_by_code()` free
        // functions must return the identical wording a fresh
        // `default_registry()` produces - otherwise two call sites, one
        // using the shared singleton and one building its own registry
        // (e.g. a test), could disagree (F3).
        let fresh = default_registry();
        for id in [SECURE_FIELD, NO_TARGET, CODE_INFERENCE_FAILED] {
            assert_eq!(lookup(id), fresh.lookup(id));
        }
    }
}
