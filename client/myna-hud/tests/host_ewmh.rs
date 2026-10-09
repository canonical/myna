// tests/host_ewmh.rs - the X11 host's property arithmetic.

use myna_hud::host::ewmh::{
    centre, clear_of_bottom_docks, monitor_at, position_hints, refuse_input, strut,
    without_take_focus,
};
use myna_platform::status_surface::{Point, Rect, Size, Strut};

const SCREEN: Size = Size {
    width: 1920,
    height: 1080,
};

#[test]
fn refusing_input_sets_the_flag_and_clears_input() {
    // GDK's own: StateHint | InputHint | WindowGroupHint, input True.
    let gdk = [1 | 2 | 64, 1, 1, 0, 0, 0, 0, 0, 0x400001];
    assert_eq!(
        refuse_input(&gdk),
        vec![1 | 2 | 64, 0, 1, 0, 0, 0, 0, 0, 0x400001]
    );
}

#[test]
fn refusing_input_on_a_window_without_hints_still_says_so() {
    assert_eq!(refuse_input(&[]), vec![1, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(refuse_input(&[0, 1]), vec![1, 0, 0, 0, 0, 0, 0, 0, 0]);
}

#[test]
fn a_chosen_position_names_nothing_else() {
    let hints = position_hints(Point { x: 810, y: 990 });
    assert_eq!(hints.len(), 18);
    assert_eq!(&hints[..3], &[1 | 4, 810, 990]);
    assert!(hints[3..].iter().all(|&v| v == 0), "{hints:?}");
}

#[test]
fn a_chosen_position_may_be_negative() {
    let hints = position_hints(Point { x: -5, y: 7 });
    assert_eq!(hints[1] as i32, -5);
    assert_eq!(hints[2], 7);
}

#[test]
fn take_focus_is_dropped_and_the_rest_kept() {
    assert_eq!(without_take_focus(&[10, 11, 12, 13], 11), vec![10, 12, 13]);
    assert_eq!(without_take_focus(&[10, 12], 11), vec![10, 12]);
}

#[test]
fn a_partial_strut_wins_over_the_legacy_one() {
    let partial = [0, 0, 0, 40, 0, 0, 0, 0, 0, 0, 0, 1919];
    assert_eq!(
        strut(&partial, &[0, 0, 0, 99], SCREEN),
        Some(Strut::from_partial(partial))
    );
}

#[test]
fn a_legacy_strut_is_read_when_there_is_no_partial_one() {
    assert_eq!(
        strut(&[], &[0, 0, 28, 0], SCREEN),
        Some(Strut::from_legacy([0, 0, 28, 0], SCREEN))
    );
}

#[test]
fn malformed_struts_are_ignored() {
    assert_eq!(strut(&[0, 0, 0, 40], &[0, 0, 0], SCREEN), None);
    assert_eq!(strut(&[], &[], SCREEN), None);
}

#[test]
fn a_point_belongs_to_the_monitor_it_falls_in() {
    let monitors = [
        Rect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        },
        Rect {
            x: 1920,
            y: 0,
            width: 1280,
            height: 1024,
        },
    ];
    assert_eq!(monitor_at(Point { x: 0, y: 0 }, &monitors), Some(0));
    assert_eq!(monitor_at(Point { x: 1919, y: 1079 }, &monitors), Some(0));
    assert_eq!(monitor_at(Point { x: 1920, y: 0 }, &monitors), Some(1));
    assert_eq!(monitor_at(Point { x: 3199, y: 1023 }, &monitors), Some(1));
    assert_eq!(monitor_at(Point { x: 3200, y: 0 }, &monitors), None);
    assert_eq!(monitor_at(Point { x: 2000, y: 1024 }, &monitors), None);
    assert_eq!(monitor_at(Point { x: -1, y: 5 }, &monitors), None);
}

#[test]
fn the_centre_is_half_the_size_in() {
    assert_eq!(
        centre(
            Point { x: 100, y: 50 },
            Size {
                width: 301,
                height: 66
            }
        ),
        Point { x: 250, y: 83 }
    );
}

const FHD: Rect = Rect {
    x: 0,
    y: 0,
    width: 1920,
    height: 1080,
};

fn rect(x: i32, y: i32, width: i32, height: i32) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}

#[test]
fn a_bottom_dock_without_a_strut_lifts_the_area() {
    // Xubuntu's launcher: 306x49, bottom-centre, intelligent autohide.
    let dock = rect(807, 1031, 306, 49);
    assert_eq!(
        clear_of_bottom_docks(rect(0, 27, 1920, 1053), FHD, &[dock]),
        rect(0, 27, 1920, 1004)
    );
}

#[test]
fn a_hidden_dock_still_reserves_where_it_slides_out() {
    // Autohide moves it below the screen edge, size unchanged.
    let hidden = rect(807, 1149, 306, 49);
    assert_eq!(
        clear_of_bottom_docks(rect(0, 27, 1920, 1053), FHD, &[hidden]),
        rect(0, 27, 1920, 1004)
    );
}

#[test]
fn a_dock_below_a_strut_already_honoured_changes_nothing() {
    let dock = rect(0, 1040, 1920, 40);
    let area = rect(0, 0, 1920, 1000);
    assert_eq!(clear_of_bottom_docks(area, FHD, &[dock]), area);
}

#[test]
fn docks_elsewhere_are_ignored() {
    let area = rect(0, 27, 1920, 1053);
    let top = rect(0, 0, 1920, 27);
    let left = rect(0, 0, 48, 1080);
    let other_monitor = rect(2000, 1031, 306, 49);
    assert_eq!(
        clear_of_bottom_docks(area, FHD, &[top, left, other_monitor]),
        area
    );
}
