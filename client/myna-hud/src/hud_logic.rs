//! hud_logic — PURE logic for the bottom-center HUD pill (feature
//! 004-gnome-shell-indicator, 2026-07-30 HUD redesign; research R14/R15;
//! contracts extension.md X19–X21). No GTK imports — unit-tested headless
//! by [`tests/hud_logic.rs`], the same split `states`/`vumeter` already
//! establish between the *stable* pure layer and the *experimental*
//! toolkit-dependent window ([`crate::window`], harness-tier — see plan.md
//! Constitution Check). The window owns all pixels; this module only decides
//! icon choice, colour class, ribbon phase, and auto-dismiss behavior.
//!
//! The pill's positioning is not logic anymore: on GNOME the extension host
//! positions the window (R21); elsewhere the window centers itself — no
//! hand-computed `computePosition` exists.

use crate::ribbon::RibbonPhase;
use crate::states::{DictationState, Severity};

/// Icon choice for a descriptor's severity (X19): a mic-with-slash icon only
/// for a critical error (the microphone genuinely may be at fault); every
/// other treatment — including a recoverable notice, where the microphone
/// itself isn't the problem — keeps the plain filled mic.
///
pub fn icon_for_severity(severity: Option<Severity>) -> &'static str {
    match severity {
        Some(Severity::Critical) => "microphone-disabled-symbolic",
        _ => "audio-input-microphone-symbolic",
    }
}

/// Whether a held notice of this severity auto-dismisses on its own.
/// Only `Recoverable` (`notice`) auto-dismisses locally after its dynamic
/// hold; `Critical` (`error`) stays until the server publishes a new state.
/// The server auto-dismisses `notice` after a longer hold, but the client
/// keeps showing for its own (even longer, slower reading) hold, ignoring
/// the server's `idle` until its timer completes.
///
pub fn severity_auto_dismisses(severity: Option<Severity>) -> bool {
    severity == Some(Severity::Recoverable)
}

/// Whether an incoming descriptor should replace an already-held notice in
/// place rather than being ignored or queued (R15, FR-007a/FR-007d, X20):
/// any new problem descriptor (`Some` severity) always replaces whatever is
/// currently held — there is exactly one held-notice slot, never a queue,
/// regardless of whether the severity matches the one already showing.
///
pub fn should_replace_held_notice(incoming_severity: Option<Severity>) -> bool {
    incoming_severity.is_some()
}

/// The pill's colour-class name for this state/severity (feature 004
/// follow-up, post-manual-test-review): orange for a recoverable notice, red
/// for a critical error — so severity reads at a glance, not just from text —
/// and a warm "loading" tint for the cold-model-load phase so it's legible as
/// distinct from listening (FR-006) by more than its label alone. Every other
/// state (recording/transcribing/finalizing) gets no colour override — the
/// label alone is enough to distinguish those (FR-005).
///
pub fn pill_color_class(key: DictationState, severity: Option<Severity>) -> Option<&'static str> {
    match severity {
        Some(Severity::Recoverable) => Some("myna-hud-severity-recoverable"),
        Some(Severity::Critical) => Some("myna-hud-severity-critical"),
        None if key == DictationState::Loading => Some("myna-hud-phase-loading"),
        None => None,
    }
}

/// Every colour class [`pill_color_class`] can return, for a view to reset
/// before applying the current one (avoids stale classes lingering across
/// states).
///
pub const PILL_COLOR_CLASSES: [&str; 3] = [
    "myna-hud-severity-recoverable",
    "myna-hud-severity-critical",
    "myna-hud-phase-loading",
];

/// Which wave-ribbon lifecycle phase ([`crate::ribbon`]) a state transition
/// forces, or `None` when the ribbon manages its own phase internally
/// (2026-07-30 wave-ribbon redesign, R17; 2026-08-21 fix). The live states
/// pin the ribbon to the phase their motion belongs in, so a transition *out
/// of* a terminal phase visibly recovers instead of leaving the ribbon stuck
/// in it:
///   - `transcribing` → `morph` (FR-010a: session ended, simplified
///     processing motion).
///   - `finalizing` → `complete` (FR-010d: the brief quiet-success
///     indication before the pill clears).
///   - `loading`/`recording`/`active` → `flow` (the live flowing wave — also
///     what returns the ribbon to motion after a `morph`/`complete`, which
///     was previously unreachable without an idle/new session in between).
///
/// `flow` requested during the fresh-session `unfold` reveal is a no-op in
/// the renderer, so the reveal is never cut short.
/// `idle`/`notice`/`error` return `None`: idle never shows, and notice/error
/// are carried by the severity tint/visibility, not a phase.
///
pub fn ribbon_phase_for_state_key(key: DictationState) -> Option<RibbonPhase> {
    match key {
        DictationState::Transcribing => Some(RibbonPhase::Morph),
        DictationState::Finalizing => Some(RibbonPhase::Complete),
        DictationState::Loading | DictationState::Recording | DictationState::Active => {
            Some(RibbonPhase::Flow)
        }
        DictationState::Idle | DictationState::Notice | DictationState::Error => None,
    }
}

/// Whether the wave ribbon stays visible for this severity (2026-07-30
/// design refinement — "fabric in gentle airflow" pass). Only a **critical**
/// error fully hides/collapses the ribbon (the pill's icon/border/message
/// carry that state instead); a **recoverable** notice keeps the ribbon
/// visible, tinted amber and gently pulsing rather than hidden — motion
/// "pauses" but never reads as dead. The severity is passed straight through
/// as the ribbon model's `severity_tint` input.
///
pub fn ribbon_visible_for_severity(severity: Option<Severity>) -> bool {
    severity != Some(Severity::Critical)
}
