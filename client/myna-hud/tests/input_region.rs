// tests/input_region.rs — the click-through contract (feature 004, R22;
// FR-025; contract extension.md XH12).
//
// **Amended 2026-08-26**: the region is empty in EVERY state. It previously
// punched a hole for a critical error's dismiss (×) control, which no longer
// exists: the HUD receives no pointer events at all, and an error is cleared
// by the client publishing a new state rather than by the user clicking the
// pill.

use myna_hud::input_region::input_region_rects;
use myna_hud::states::{state_to_descriptor, wire, Severity};

#[test]
fn the_hud_is_click_through_in_every_state() {
    for state in wire::ALL {
        let descriptor = state_to_descriptor(Some(state), "some reason");
        assert!(
            input_region_rects(descriptor.severity).is_empty(),
            "{state} takes no pointer input"
        );
    }
}

#[test]
fn even_a_critical_error_is_click_through() {
    // The one state that used to be interactive. A pill that swallowed
    // clicks over the user's own application would be a regression in the
    // overlay's core promise (FR-025), not a feature.
    assert!(input_region_rects(Some(Severity::Critical)).is_empty());
}

#[test]
fn an_unknown_state_is_click_through_too() {
    // Additive tolerance (C8): an unrecognised wire value degrades to the
    // neutral descriptor, which must not become interactive by accident.
    let descriptor = state_to_descriptor(Some("quantizing"), "");
    assert!(input_region_rects(descriptor.severity).is_empty());
}

#[test]
fn no_severity_can_make_it_interactive() {
    for severity in [None, Some(Severity::Recoverable), Some(Severity::Critical)] {
        assert!(
            input_region_rects(severity).is_empty(),
            "{severity:?} takes no pointer input"
        );
    }
}
