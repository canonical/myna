// tests/notice_slot.rs — hermetic test for the held-notice slot (feature
// 004, R15; FR-007a/FR-007b/FR-007d; contract extension.md X20 re-homed to
// the renderer). The slot owns *when* a notice clears; the window owns the
// pixels and the actual timer.

use myna_hud::notice_slot::{NoticeSlot, HOLD_MS};
use myna_hud::states::Severity;

fn recoverable(reason: &str) -> (Option<Severity>, String) {
    (Some(Severity::Recoverable), reason.to_string())
}

// --- FR-007a: a recoverable notice auto-dismisses after the hold window ---

#[test]
fn recoverable_notice_auto_dismisses() {
    let mut slot = NoticeSlot::default();
    let (sev, reason) = recoverable("No speech detected");
    slot.hold(sev, &reason, 0.0);
    assert!(slot.is_showing(0.0), "shown immediately");
    assert!(
        slot.is_showing(HOLD_MS - 1.0),
        "still showing just before the hold expires"
    );
    assert!(
        !slot.is_showing(HOLD_MS + 1.0),
        "cleared on its own after the hold window — no user action"
    );
    assert_eq!(slot.expires_at(), Some(HOLD_MS));
}

// --- FR-007b: critical errors now also auto-dismiss (dynamic hold) ----
// Since 2026-08-28 both severities auto-dismiss with a dynamic interval
// (hold_ms_for). Timeout is notifier-side only.

#[test]
fn critical_error_auto_dismisses_with_dynamic_hold() {
    let mut slot = NoticeSlot::default();
    let reason = "Microphone unavailable";
    slot.hold(Some(Severity::Critical), reason, 0.0);
    let expected = myna_hud::notice_slot::hold_ms_for(reason);
    assert_eq!(slot.expires_at(), Some(expected));
    assert!(
        slot.is_showing(expected - 1.0),
        "still showing just before expiry"
    );
    assert!(
        !slot.is_showing(expected + 1.0),
        "cleared on its own after the dynamic hold"
    );
    // A new state from the client also clears it early.
    slot.hold(Some(Severity::Critical), reason, 0.0);
    slot.clear();
    assert!(!slot.is_showing(0.0));
}

// --- X20/FR-007a: a second recoverable replaces in place AND restarts ----

#[test]
fn recoverable_replacement_restarts_the_hold() {
    let mut slot = NoticeSlot::default();
    slot.hold(Some(Severity::Recoverable), "first", 0.0);
    // Halfway through the original hold, a second occurrence arrives.
    slot.hold(Some(Severity::Recoverable), "second", HOLD_MS / 2.0);
    assert_eq!(slot.reason(), Some("second"), "replaced in place");
    assert_eq!(
        slot.expires_at(),
        Some(HOLD_MS / 2.0 + HOLD_MS),
        "the hold restarts in full, not on the original's stale schedule"
    );
    assert!(
        slot.is_showing(HOLD_MS + 1.0),
        "still showing past the ORIGINAL expiry"
    );
}

// --- X20/FR-007d: a second critical replaces and restarts the hold ---

#[test]
fn critical_replacement_restarts_the_hold() {
    let mut slot = NoticeSlot::default();
    slot.hold(Some(Severity::Critical), "first", 0.0);
    slot.hold(Some(Severity::Critical), "second", 500.0);
    assert_eq!(slot.reason(), Some("second"), "replaced in place");
    let expected = 500.0 + myna_hud::notice_slot::hold_ms_for("second");
    assert_eq!(slot.expires_at(), Some(expected));
    assert!(
        !slot.is_showing(expected + 1.0),
        "cleared after its own hold"
    );
}

// A problem of the *other* severity also replaces the held slot — there is
// exactly one slot, never a queue (R15). Both severities now auto-dismiss.
#[test]
fn any_problem_replaces_the_single_slot() {
    let mut slot = NoticeSlot::default();
    slot.hold(Some(Severity::Recoverable), "hiccup", 0.0);
    slot.hold(Some(Severity::Critical), "broken", 100.0);
    assert_eq!(slot.severity(), Some(Severity::Critical));
    assert_eq!(slot.reason(), Some("broken"));
    assert_eq!(
        slot.expires_at(),
        Some(100.0 + myna_hud::notice_slot::hold_ms_for("broken"))
    );

    slot.hold(Some(Severity::Recoverable), "hiccup again", 200.0);
    assert_eq!(slot.severity(), Some(Severity::Recoverable));
    assert_eq!(
        slot.expires_at(),
        Some(200.0 + myna_hud::notice_slot::hold_ms_for("hiccup again"))
    );
}

// --- A non-problem state clears the slot (a new session starts clean) ----

#[test]
fn non_problem_state_clears_the_slot() {
    let mut slot = NoticeSlot::default();
    slot.hold(Some(Severity::Critical), "broken", 0.0);
    slot.hold(None, "", 10.0);
    assert!(
        !slot.is_showing(10.0),
        "a live state clears the held notice"
    );
    assert_eq!(slot.severity(), None);
}

// --- FR-007a: the notice never blocks a new session ---------------------
// Structural: the slot exposes only presentation state, so nothing here can
// gate a session start. Pinned so a future "blocking" flag would fail here.

#[test]
fn slot_carries_no_blocking_state() {
    let mut slot = NoticeSlot::default();
    slot.hold(Some(Severity::Recoverable), "No speech detected", 0.0);
    // A fresh session's live state simply replaces it, at any time.
    slot.hold(None, "", 1.0);
    assert!(!slot.is_showing(1.0));
}
