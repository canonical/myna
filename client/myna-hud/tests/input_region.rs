// tests/input_region.rs — hermetic test for the per-state input-region
// geometry (R22, FR-025/SC-015): the pill's window is fully click-through in
// every state except a critical error, where exactly the dismiss (×)
// control's rectangle is interactive.

use myna_hud::input_region::{input_region_rects, Rect};
use myna_hud::states::Severity;

const DISMISS: Rect = Rect {
    x: 300.0,
    y: 8.0,
    width: 24.0,
    height: 24.0,
};

// FR-025: click-through in every non-critical state — an empty region means
// pointer events pass through the window to whatever is underneath.
#[test]
fn empty_everywhere_except_critical() {
    assert!(
        input_region_rects(None, Some(DISMISS)).is_empty(),
        "no severity"
    );
    assert!(
        input_region_rects(Some(Severity::Recoverable), Some(DISMISS)).is_empty(),
        "recoverable notice stays fully click-through"
    );
    for key in myna_hud::states::wire::ALL {
        let desc = myna_hud::states::state_to_descriptor(Some(key), "");
        if desc.severity != Some(Severity::Critical) {
            assert!(
                input_region_rects(desc.severity, Some(DISMISS)).is_empty(),
                "{key}: non-critical state is click-through"
            );
        }
    }
}

// FR-007b/c: during a critical error the region covers exactly the dismiss
// control's rectangle — the only interactive pixels on the pill.
#[test]
fn critical_error_covers_exactly_the_dismiss_rect() {
    let rects = input_region_rects(Some(Severity::Critical), Some(DISMISS));
    assert_eq!(rects, vec![DISMISS]);
}

// Pre-layout (no dismiss allocation yet): even a critical error is not
// interactive until the control's rectangle is known.
#[test]
fn critical_error_without_layout_is_still_click_through() {
    assert!(input_region_rects(Some(Severity::Critical), None).is_empty());
}

// The region recomputes when the control moves/resizes (the window re-applies
// it after size-allocate — R22).
#[test]
fn recomputes_when_the_control_moves() {
    let moved = Rect {
        x: 310.0,
        ..DISMISS
    };
    assert_eq!(
        input_region_rects(Some(Severity::Critical), Some(moved)),
        vec![moved]
    );
}

// Regions are sane geometry: finite, non-negative.
#[test]
fn rects_are_sane_geometry() {
    let rects = input_region_rects(Some(Severity::Critical), Some(DISMISS));
    assert!(rects
        .iter()
        .all(|r| r.x.is_finite() && r.y.is_finite() && r.width >= 0.0 && r.height >= 0.0));
}
