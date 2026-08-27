// tests/session_control.rs — the `--serve-dbus` simulator's session-control
// state machine (feature 004, T132; contract dbus-interface.md C6), ported
// from `dictation_service.py`'s `_on_method_call` + `dbus_headless.py`'s
// method checks. The bus wiring is out of scope here; this pins the rules
// the served `Start`/`Stop`/`Toggle` methods drive.

use myna_hud::session_control::{Session, StartOutcome};

#[test]
fn start_begins_a_session_and_reports_success() {
    let mut session = Session::default();
    assert!(!session.is_active(), "a fresh session is idle");

    let outcome = session.start();
    assert_eq!(
        outcome,
        StartOutcome::Started,
        "Start reports success (C6/C7)"
    );
    assert!(session.is_active());
}

#[test]
fn stop_ends_the_session() {
    let mut session = Session::default();
    session.start();
    session.stop();
    assert!(!session.is_active(), "Stop clears to idle");
}

#[test]
fn stop_when_idle_is_a_no_op() {
    // Contract: "no-op if idle" — Stop must not error or toggle anything on.
    let mut session = Session::default();
    session.stop();
    assert!(!session.is_active());
    session.stop();
    assert!(!session.is_active());
}

#[test]
fn toggle_starts_when_idle_and_stops_when_active() {
    let mut session = Session::default();
    assert_eq!(session.toggle(), Some(StartOutcome::Started));
    assert!(session.is_active(), "toggle from idle starts");
    assert_eq!(
        session.toggle(),
        None,
        "toggle while active stops (no start outcome)"
    );
    assert!(!session.is_active());
}

// --- C6: duplicate Start does not begin two sessions --------------------

#[test]
fn duplicate_start_does_not_begin_a_second_session() {
    let mut session = Session::default();
    assert_eq!(session.start(), StartOutcome::Started);
    assert_eq!(
        session.start(),
        StartOutcome::AlreadyActive,
        "a repeated Start is a dedup no-op, not a second session"
    );
    assert!(session.is_active());

    // ...and stopping once ends it — there is only ever one session.
    session.stop();
    assert!(!session.is_active());
}

#[test]
fn repeated_toggles_never_stack_sessions() {
    let mut session = Session::default();
    // idle -> active -> idle -> active, always exactly one session.
    session.toggle();
    assert!(session.is_active());
    session.toggle();
    assert!(!session.is_active());
    session.toggle();
    assert!(session.is_active());
}

// --- The served snapshot the wire methods report -----------------------
// The lab drives the visual state from the same session flag the methods
// set, so a `Stop` clears the pill to idle (dbus_headless.py) via wire_state.

#[test]
fn an_inactive_session_publishes_idle() {
    use myna_hud::simulator::wire_state;
    use myna_hud::states::wire;

    let mut session = Session::default();
    session.start();
    let (state, _) = wire_state("flow", None, session.is_active());
    assert_eq!(
        state,
        wire::RECORDING,
        "an active session shows the live state"
    );

    session.stop();
    let (state, _) = wire_state("flow", None, session.is_active());
    assert_eq!(state, wire::IDLE, "Stop clears the pill to idle");
}

// --- The lab's chosen state implies the session (set_active) ------------
// The lab has no separate "start a session" control — its state dropdown is
// its whole intent. So a non-idle selection must mean a live session, or
// `--serve-dbus` would publish nothing until the operator separately called
// `Toggle` on the bus (which is exactly the surprise this pins against).

#[test]
fn a_non_idle_selection_implies_an_active_session() {
    let mut session = Session::default();
    assert!(!session.is_active(), "starts idle");
    session.set_active(true);
    assert!(session.is_active());
    session.set_active(false);
    assert!(!session.is_active(), "an idle selection stops the session");
}
