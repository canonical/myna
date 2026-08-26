//! simulator — the `--serve-dbus` mode's PURE mapping (lab controls →
//! `org.myna.Dictation` wire properties). Ported from
//! `dev-lab-gpu/dictation_service.py`'s mapping tables (deleted with the old
//! bundle; this is now the single source of truth — the zbus publisher that
//! consumes it is T132).
//!
//! The simulator makes the lab a stand-in for `myna-desktop --dbus`: it owns
//! the bus name the renderer watches, so a live session shows the real HUD
//! driven by the lab's controls instead of by speech — no microphone, no
//! model, no backend.
//!
//! Nothing here decides how the ribbon *looks*: this module maps the lab's
//! controls onto the four wire properties of
//! `specs/004-gnome-shell-indicator/contracts/dbus-interface.md`, and that
//! mapping is the whole content of this file.

use crate::ribbon::RibbonPhase;
use crate::states::{wire, Severity};
use crate::vumeter::{DB_CEILING, DB_FLOOR};

/// The publish cadence: ~15-20 Hz per the contract's C4, not the lab's
/// render-loop rate, so the consumer sees the update rate it was tuned
/// against.
pub const PUBLISH_HZ: f64 = 20.0;

/// Which wire `State` each ribbon phase belongs to — the inverse of
/// [`crate::hud_logic::ribbon_phase_for_state_key`]. That mapping is
/// many-to-one (loading, recording and active all request `flow`), so the
/// inverse has to pick one; `recording` is chosen because it is the state a
/// person watching the ribbon flow is actually in. `unfold` is the reveal a
/// fresh session plays, so it sits inside a recording session too.
fn phase_state(phase: &str) -> Option<&'static str> {
    match phase {
        "unfold" | "flow" => Some(wire::RECORDING),
        "morph" => Some(wire::TRANSCRIBING),
        "complete" => Some(wire::FINALIZING),
        _ => None,
    }
}

/// Content-free reasons, per constitution V and contract C3 — never anything
/// derived from a transcript. The empty one is deliberate: it exercises the
/// path where the state module supplies its own default text ("No speech
/// detected"), while the error reason exercises the "Error — %s" prefix.
/// Between them the two ErrorMessage renderings are both visible from the
/// lab.
pub const NOTICE_REASON: &str = "";
pub const ERROR_REASON: &str = "Microphone unavailable";

/// The lab's look as a `(State, ErrorMessage)` pair.
///
/// Port of `dictation_service.py`'s `wire_state`:
/// * `session_active == false` (Stop/Toggle ended the session — the daemon
///   is still running, it is simply not dictating) → `idle`, the case that
///   clears the pill entirely.
/// * Severity outranks the phase: the pill drives notice/error from the
///   state itself, so a tinted ribbon has to publish the matching state.
/// * Unknown phases degrade to `active`, the same additive tolerance the
///   contract asks of clients (C8).
pub fn wire_state(
    phase: &str,
    severity_tint: Option<Severity>,
    session_active: bool,
) -> (&'static str, &'static str) {
    if !session_active {
        return (wire::IDLE, "");
    }
    if let Some(severity) = severity_tint {
        return match severity {
            Severity::Recoverable => (wire::NOTICE, NOTICE_REASON),
            Severity::Critical => (wire::ERROR, ERROR_REASON),
        };
    }
    match phase_state(phase) {
        Some(state) => (state, ""),
        None => ("active", ""),
    }
}

/// What the consumer's own state → phase mapping
/// ([`crate::hud_logic::ribbon_phase_for_state_key`]) does with each state
/// that can be published — used to explain the round trip in the lab UI;
/// the publisher itself never consults it.
///
/// The mapping is lossy in both directions: several states collapse onto
/// `flow`, so a phase can move the lab's ribbon without moving the
/// consumer's. Every current phase round-trips or is renderer-driven
/// (`unfold`: the renderer plays the reveal itself when the pill appears,
/// on its own clock).
pub fn shell_phase(
    phase: &str,
    severity_tint: Option<Severity>,
    session_active: bool,
) -> Option<RibbonPhase> {
    let (state, _) = wire_state(phase, severity_tint, session_active);
    crate::hud_logic::ribbon_phase_for_state_key(match state {
        wire::LOADING => crate::states::DictationState::Loading,
        wire::RECORDING => crate::states::DictationState::Recording,
        wire::TRANSCRIBING => crate::states::DictationState::Transcribing,
        wire::FINALIZING => crate::states::DictationState::Finalizing,
        wire::NOTICE => crate::states::DictationState::Notice,
        wire::ERROR => crate::states::DictationState::Error,
        wire::IDLE => crate::states::DictationState::Idle,
        _ => crate::states::DictationState::Active,
    })
}

/// The vumeter takes `max(rms, peak * 0.55)`, so any peak below
/// `rms / 0.55` leaves RMS in charge. 1.8 keeps a plausible ~5 dB crest
/// above RMS while staying under that limit, so the slider still maps
/// exactly onto the HUD intensity instead of the peak term quietly taking
/// over at the top of the range.
const PEAK_OVER_RMS: f64 = 1.8;

/// Invert the vumeter's `boost_level` so the slider drives the HUD 1:1.
///
/// The lab's slider is the *smoothed envelope* — what the ribbon consumes —
/// but the wire carries raw RMS and peak, which the consumer pushes back
/// through [`crate::vumeter::levels_to_intensity`]. Publishing the slider
/// value directly would put the lab's ribbon and the hosted ribbon at
/// visibly different amplitudes for the same setting; inverting the
/// calibration here is what makes the two agree (and what catches drift if
/// the vumeter constants ever change without the simulator following).
///
/// Port of `dictation_service.py`'s `envelope_to_levels`.
pub fn envelope_to_levels(envelope: f64) -> (f64, f64) {
    let level = envelope.clamp(0.0, 1.0);
    if level <= 0.0 {
        return (0.0, 0.0);
    }
    let db = DB_FLOOR + level * (DB_CEILING - DB_FLOOR);
    let rms = 10f64.powf(db / 20.0).min(1.0);
    (rms, (rms * PEAK_OVER_RMS).min(1.0))
}
