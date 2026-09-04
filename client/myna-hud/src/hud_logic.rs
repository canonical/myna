//! hud_logic — PURE logic for the bottom-center HUD pill (feature
//! 004-gnome-shell-indicator, 2026-07-30 HUD redesign; research R14/R15;
//! contracts RC19–RC21). No GTK imports — unit-tested headless
//! by [`tests/hud_logic.rs`], the same split `states`/`vumeter` already
//! establish between the *stable* pure layer and the *experimental*
//! toolkit-dependent window ([`crate::window"], harness-tier — see plan.md
//! Constitution Check). The window owns all pixels; this module only decides
//! icon choice, colour class, ribbon phase, and auto-dismiss behavior.
//!
//! The pill's positioning is not logic anymore: on GNOME the extension host
//! positions the window (R21); elsewhere the window centers itself — no
//! hand-computed `computePosition` exists.

use crate::ribbon::RibbonPhase;
use crate::states::{DictationState, Severity};

/// The HUD's audio-level presentation, selected by the `hud-style` GSettings
/// key (`com.canonical.Myna.Dictation`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum HudStyle {
    /// A simple level bar in the accent colour (the default; `vumeter.png`).
    #[default]
    Bar,
    /// The flowing GPU wave ribbon (the 2026-07-30 redesign).
    Ribbon,
    /// The classic segmented bar meter (the pre-ribbon `BarMeterActor`).
    Vumeter,
    /// A plain `GtkProgressBar`.
    Progress,
}

impl HudStyle {
    /// Parse a `hud-style` nick; anything unrecognised is the default (Bar) —
    /// a foreign/newer value must never break the HUD.
    pub fn from_nick(nick: &str) -> Self {
        match nick {
            "ribbon" => HudStyle::Ribbon,
            "vumeter" => HudStyle::Vumeter,
            "progress" => HudStyle::Progress,
            _ => HudStyle::Bar,
        }
    }
}

/// Icon choice for a descriptor's severity (RC19): a mic-with-slash icon only
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
/// place rather than being ignored or queued (R15, FR-007a/FR-007d, RC20):
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

// ── Non-ribbon indicator animation (bar / vumeter / progress) ───────────────

/// The indeterminate "activity" pulse shown by the simple indicators while the
/// session is working: a little block that travels back and forth (à la pong).
/// The bar and the segmented meter draw it themselves from the shared
/// [`pulse_position`]; the progress view maps it onto the stock
/// `GtkProgressBar::pulse()`.
#[derive(Clone, Copy, Debug)]
pub struct Pulse {
    /// The block's width as a fraction of the indicator (`[0,1]`).
    pub width: f64,
    /// Milliseconds for one full back-and-forth cycle.
    pub period_ms: f64,
    /// The block's alpha — `<1` for a semi-transparent accent (loading).
    pub alpha: f64,
}

impl Default for Pulse {
    fn default() -> Self {
        Self {
            width: 0.2,
            period_ms: 1000.0,
            alpha: 1.0,
        }
    }
}

/// How the simple indicators (bar / vumeter / progress) render the current
/// state, mirroring the ribbon's phase-driven motion with their own
/// primitives:
///
/// - `loading` → an activity [`Pulse`] at a slow pace, semi-transparent
///   accent (the "warming up" look).
/// - `transcribing` → a faster, fuller [`Pulse`] — the "working on it" feel
///   while partial results are being committed.
/// - `finalizing` → a quick [`Pulse`] — the "done" tail before the pill
///   clears.
/// - `notice` (recoverable) → the bar reads **full and warning-coloured**,
///   gently breathing so it is alive but never looks like a level.
/// - anything else live (`recording`/`active`/`idle`) → the plain level.
///
/// A `critical` error hides the indicator, so it reports a closed (0) fill.
#[derive(Clone, Copy, Debug)]
pub struct IndicatorState {
    /// Filled fraction for plain-level states (`[0,1]`).
    pub fraction: f64,
    /// `Some` when the view should show an indeterminate activity pulse
    /// instead of a level (loading / transcribing / finalizing).
    pub pulse: Option<Pulse>,
    /// Warning (recoverable notice): use the warning colour and read full.
    pub warning: bool,
}

impl Default for IndicatorState {
    fn default() -> Self {
        Self {
            fraction: 0.0,
            pulse: None,
            warning: false,
        }
    }
}

/// The animation state for the simple indicators.
///
/// `intensity` is the calibrated `[0,1]` level; `state_ms` is how long the
/// indicator has been in its current state (0 = just entered); `reduced_motion`
/// follows the desktop's reduce-animation preference — under it the activity
/// pulse travels much more slowly than usual (reduced, not removed motion).
pub fn indicator_state(
    key: DictationState,
    severity: Option<Severity>,
    intensity: f64,
    _state_ms: f64,
    reduced_motion: bool,
) -> IndicatorState {
    let level = intensity.clamp(0.0, 1.0);
    // Reduced motion slows the travel: callers multiply the "normal" period
    // by this. The Ribbon's reduce-animation keeps a gentle, slow wave too.
    let speed = if reduced_motion { 3.5 } else { 1.0 };
    match (key, severity) {
        (_, Some(Severity::Critical)) => IndicatorState::default(),
        // Notice: warning colour, and an **empty** bar — the recoverable
        // treatment reads as "attention, warning tint, nothing recorded".
        (_, Some(Severity::Recoverable)) => IndicatorState {
            fraction: 0.0,
            warning: true,
            ..Default::default()
        },
        (DictationState::Loading, _) => IndicatorState {
            pulse: Some(Pulse {
                width: 0.20,
                period_ms: 1600.0 * speed,
                alpha: 0.45, // semi-transparent accent
            }),
            ..Default::default()
        },
        (DictationState::Transcribing, _) => IndicatorState {
            pulse: Some(Pulse {
                width: 0.26,
                period_ms: 1100.0 * speed,
                alpha: 1.0,
            }),
            ..Default::default()
        },
        (DictationState::Finalizing, _) => IndicatorState {
            pulse: Some(Pulse {
                width: 0.32,
                period_ms: 800.0 * speed,
                alpha: 0.9,
            }),
            ..Default::default()
        },
        // Recording / Active / Idle: the plain level.
        _ => IndicatorState {
            fraction: level,
            ..Default::default()
        },
    }
}

/// The centre of the activity block at `state_ms`: a `0..1..0` triangle wave
/// across `period_ms` — 0 (left) at t=0, 1 (right) at t=period/2, 0 again at
/// t=period — the "pong" back-and-forth used by the pulsing states.
pub fn pulse_position(state_ms: f64, period_ms: f64) -> f64 {
    if period_ms <= 0.0 {
        return 0.0;
    }
    (0.5 - 0.5 * (state_ms / period_ms * std::f64::consts::TAU).cos()).clamp(0.0, 1.0)
}
