//! Screen-reader/braille accessibility announcements (feature
//! 011-accessible-dictation-ux, `contracts/announcer.md`).
//!
//! Content-free AT-SPI "Announcement" events, reaching speech and braille
//! through the same accessibility-bus path, plus an on-demand-queryable
//! accessible name/description/state (FR-001). Parallel to the existing
//! [`crate::indicator::Indicator`] seam: `Indicator` renders visual state,
//! this seam emits non-visual events; a single controller transition drives
//! both without either seam knowing about the other.

pub mod announcing_indicator;
pub mod atspi;
pub mod fake;
pub mod format;
pub mod gate;
pub mod recover;

use async_trait::async_trait;
use std::fmt;

pub use announcing_indicator::AnnouncingIndicator;
pub use fake::FakeAnnouncer;
pub use format::{format_state_announcement, StateAnnouncement};
pub use gate::{CoalescingAnnouncer, VerbosityGatedAnnouncer};
pub use recover::RecoveringAnnouncer;

/// A failure/notice severity (data-model.md), shared by announcements
/// (this module) and `FailurePresentation` (`crate::failure`, which
/// re-exports `myna_core::failure`) so the two never disagree on the
/// recoverable/critical vocabulary. Re-exported (not redefined) from
/// `myna-core` since US4 moved `FailurePresentation` there (T067) so
/// `myna-cli` can share it too.
pub use myna_core::failure::Severity;

/// An `announce()` call failed (contract A5, FR-002a). The dictation session
/// MUST continue unaffected when this happens — see `controller.rs`'s
/// handling, which turns this into a recoverable `FailurePresentation`
/// rather than propagating it as a session error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnounceError(pub String);

impl fmt::Display for AnnounceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "accessibility announcement failed: {}", self.0)
    }
}

impl std::error::Error for AnnounceError {}

/// A content-free string safe to announce (constitution V, FR-003, contract
/// A1). Constructible **only** from a `&'static str` literal — a runtime
/// transcript or unstable hypothesis is always an owned `String`, so it
/// cannot be passed here; only strings baked into the binary (state labels,
/// authored `FailurePresentation` messages) can. This is a compile-time
/// guarantee, not a runtime check: there is deliberately no `From<String>`
/// or `From<&str>` (non-`'static`) impl.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnnouncementText(&'static str);

impl AnnouncementText {
    pub const fn new(text: &'static str) -> Self {
        AnnouncementText(text)
    }

    pub fn as_str(&self) -> &str {
        self.0
    }
}

/// The accessibility-announcement seam (contracts/announcer.md). Parallel to
/// [`crate::indicator::Indicator`]; implementations never block the caller
/// beyond a bounded internal timeout, and a failure here MUST NOT propagate
/// as a session error (FR-002a) — callers are expected to treat `Err` as
/// "log/surface as a recoverable notice", not "abort the session".
#[async_trait]
pub trait AccessibilityAnnouncer: Send {
    /// Emit a content-free announcement (constitution V, FR-003): state,
    /// severity, and recovery action only. `text` is an [`AnnouncementText`]
    /// rather than a plain string specifically so transcript content cannot
    /// reach this call (see its doc comment).
    async fn announce(
        &mut self,
        text: AnnouncementText,
        severity: Option<Severity>,
    ) -> Result<(), AnnounceError>;

    /// Update the on-demand-queryable accessible name/description (FR-001)
    /// without necessarily emitting a proactive announcement.
    async fn set_state(&mut self, name: AnnouncementText, description: AnnouncementText);
}

/// Lets a boxed trait object be used anywhere a concrete `A:
/// AccessibilityAnnouncer` type parameter is expected (e.g.
/// `gate::VerbosityGatedAnnouncer<A, P>`) — needed because the real daemon
/// (`bin/myna-desktop.rs`) picks between [`atspi::AtspiAnnouncer`] and
/// [`NullAnnouncer`] at runtime (whichever `AtspiAnnouncer::connect()`
/// resolves to), so the gating stack has to compose over a `Box<dyn
/// AccessibilityAnnouncer>` rather than a single fixed concrete type.
#[async_trait]
impl AccessibilityAnnouncer for Box<dyn AccessibilityAnnouncer> {
    async fn announce(
        &mut self,
        text: AnnouncementText,
        severity: Option<Severity>,
    ) -> Result<(), AnnounceError> {
        (**self).announce(text, severity).await
    }

    async fn set_state(&mut self, name: AnnouncementText, description: AnnouncementText) {
        (**self).set_state(name, description).await;
    }
}

/// The default announcer for a caller that never connected to a real
/// accessibility bus at all (distinct from [`atspi::AtspiAnnouncer`] having
/// connected and then failing mid-session, which [`recover::RecoveringAnnouncer`]
/// handles) — a silent no-op, so a daemon whose `AtspiAnnouncer::connect()`
/// failed at startup (no `org.a11y.Bus` reachable) degrades to "no
/// screen-reader announcements" rather than panicking or blocking. Mirrors
/// `sound::NullSoundCuePlayer`'s convention.
#[derive(Debug, Default)]
pub struct NullAnnouncer;

#[async_trait]
impl AccessibilityAnnouncer for NullAnnouncer {
    async fn announce(
        &mut self,
        _text: AnnouncementText,
        _severity: Option<Severity>,
    ) -> Result<(), AnnounceError> {
        Ok(())
    }

    async fn set_state(&mut self, _name: AnnouncementText, _description: AnnouncementText) {}
}


#[cfg(test)]
mod tests {
    use super::*;

    // ── T011: AnnouncementText only accepts 'static literals ────────────────

    #[test]
    fn announcement_text_round_trips_a_static_literal() {
        let t = AnnouncementText::new("Dictation: listening");
        assert_eq!(t.as_str(), "Dictation: listening");
    }

    // ── Production-wiring regression (found 2026-08-31: the real
    //    `myna-desktop` binary never constructed an announcer/sound player at
    //    all, so a real screen-reader user heard nothing despite every layer
    //    being individually hermetically tested) — a boxed trait object must
    //    genuinely satisfy the trait so `bin/myna-desktop.rs`'s
    //    `Box<dyn AccessibilityAnnouncer>` compiles into the gating stack. ──

    #[test]
    fn null_announcer_never_errors_and_is_a_true_no_op() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut null = NullAnnouncer;
            assert!(null
                .announce(AnnouncementText::new("x"), None)
                .await
                .is_ok());
            null.set_state(AnnouncementText::new("x"), AnnouncementText::new("y"))
                .await;
        });
    }

    #[test]
    fn a_boxed_announcer_satisfies_the_trait_and_forwards_calls() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            let fake = fake::FakeAnnouncer::new();
            let log = fake.log();
            let mut boxed: Box<dyn AccessibilityAnnouncer> = Box::new(fake);
            boxed
                .announce(AnnouncementText::new("Listening"), None)
                .await
                .unwrap();
            assert_eq!(log.lock().unwrap().len(), 1);
        });
    }
}
