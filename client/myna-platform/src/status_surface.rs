//! Status surface: where the dictation pill goes.
//!
//! Pure placement only, the same rules the GNOME extension's `place.js`
//! applies, plus the work area an X11 host derives from panel struts. Hosting
//! the window is the backend's.

/// A rectangle in global screen coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    fn right(&self) -> i32 {
        self.x + self.width
    }

    fn bottom(&self) -> i32 {
        self.y + self.height
    }

    fn intersects(&self, other: &Rect) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub width: i32,
    pub height: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

/// Gap between the pill and the work area's bottom edge, as on GNOME.
pub const BOTTOM_MARGIN: i32 = 24;

/// Centred on the work area, `bottom_margin` above its bottom edge. A pill
/// larger than the work area is pinned to its origin rather than pushed off
/// the left or top edge.
pub fn placement(work_area: Rect, pill: Size, bottom_margin: i32) -> Point {
    // Half rounded up, as JavaScript's Math.round in place.js.
    let centred = work_area.x + (work_area.width - pill.width + 1).div_euclid(2);
    let bottom = work_area.bottom() - pill.height - bottom_margin;
    Point {
        x: centred.max(work_area.x),
        y: bottom.max(work_area.y),
    }
}

/// The monitor the pill belongs on: the focused window's, else the
/// pointer's, else the primary; only an index below `count` counts.
pub fn choose_monitor(
    focus: Option<usize>,
    pointer: Option<usize>,
    primary: Option<usize>,
    count: usize,
) -> Option<usize> {
    [focus, pointer, primary]
        .into_iter()
        .flatten()
        .find(|&index| index < count)
}

/// One window's `_NET_WM_STRUT_PARTIAL`: space reserved at each screen
/// edge, with the span along that edge it covers (ends inclusive).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Strut {
    pub left: u32,
    pub right: u32,
    pub top: u32,
    pub bottom: u32,
    pub left_start_y: u32,
    pub left_end_y: u32,
    pub right_start_y: u32,
    pub right_end_y: u32,
    pub top_start_x: u32,
    pub top_end_x: u32,
    pub bottom_start_x: u32,
    pub bottom_end_x: u32,
}

impl Strut {
    /// The property's twelve cardinals, in EWMH order.
    pub fn from_partial(c: [u32; 12]) -> Self {
        Self {
            left: c[0],
            right: c[1],
            top: c[2],
            bottom: c[3],
            left_start_y: c[4],
            left_end_y: c[5],
            right_start_y: c[6],
            right_end_y: c[7],
            top_start_x: c[8],
            top_end_x: c[9],
            bottom_start_x: c[10],
            bottom_end_x: c[11],
        }
    }

    /// The legacy four-cardinal `_NET_WM_STRUT`, which spans whole edges.
    pub fn from_legacy(c: [u32; 4], screen: Size) -> Self {
        let last_x = screen.width.max(1) as u32 - 1;
        let last_y = screen.height.max(1) as u32 - 1;
        Self::from_partial([
            c[0], c[1], c[2], c[3], 0, last_y, 0, last_y, 0, last_x, 0, last_x,
        ])
    }
}

/// `monitor` less what any strut reserves on it. A strut is measured from
/// the screen's edge, so one on another monitor reaches this one only where
/// their rectangles meet; a zero-thickness strut meets nothing.
pub fn work_area(monitor: Rect, screen: Size, struts: &[Strut]) -> Rect {
    let (mut left, mut top) = (monitor.x, monitor.y);
    let (mut right, mut bottom) = (monitor.right(), monitor.bottom());
    let span = |start: u32, end: u32| (start as i32, end as i32 - start as i32 + 1);
    for strut in struts {
        let (y, h) = span(strut.left_start_y, strut.left_end_y);
        if reaches(monitor, 0, y, strut.left as i32, h) {
            left = left.max(strut.left as i32);
        }
        let (y, h) = span(strut.right_start_y, strut.right_end_y);
        let edge = screen.width - strut.right as i32;
        if reaches(monitor, edge, y, strut.right as i32, h) {
            right = right.min(edge);
        }
        let (x, w) = span(strut.top_start_x, strut.top_end_x);
        if reaches(monitor, x, 0, w, strut.top as i32) {
            top = top.max(strut.top as i32);
        }
        let (x, w) = span(strut.bottom_start_x, strut.bottom_end_x);
        let edge = screen.height - strut.bottom as i32;
        if reaches(monitor, x, edge, w, strut.bottom as i32) {
            bottom = bottom.min(edge);
        }
    }
    Rect {
        x: left,
        y: top,
        width: (right - left).max(0),
        height: (bottom - top).max(0),
    }
}

fn reaches(monitor: Rect, x: i32, y: i32, width: i32, height: i32) -> bool {
    monitor.intersects(&Rect {
        x,
        y,
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FHD: Rect = Rect {
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
    };
    const PILL: Size = Size {
        width: 300,
        height: 48,
    };

    fn rect(x: i32, y: i32, width: i32, height: i32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    fn size(width: i32, height: i32) -> Size {
        Size { width, height }
    }

    #[test]
    fn the_pill_sits_bottom_centre_of_the_work_area() {
        assert_eq!(
            placement(FHD, PILL, BOTTOM_MARGIN),
            Point { x: 810, y: 1008 }
        );
        assert_eq!(
            placement(rect(1920, 30, 1280, 994), PILL, BOTTOM_MARGIN),
            Point { x: 2410, y: 952 }
        );
    }

    #[test]
    fn odd_halves_round_up_as_place_js_does() {
        let area = rect(0, 0, 101, 100);
        assert_eq!(placement(area, size(50, 10), 0).x, 26);
        assert_eq!(placement(rect(10, 0, 100, 100), size(51, 10), 0).x, 35);
    }

    #[test]
    fn a_pill_larger_than_the_work_area_stays_on_it() {
        let area = rect(100, 50, 200, 40);
        assert_eq!(
            placement(area, size(400, 80), BOTTOM_MARGIN),
            Point { x: 100, y: 50 }
        );
    }

    #[test]
    fn the_focused_monitor_wins_then_pointer_then_primary() {
        assert_eq!(choose_monitor(Some(1), Some(0), Some(0), 2), Some(1));
        assert_eq!(choose_monitor(None, Some(1), Some(0), 2), Some(1));
        assert_eq!(choose_monitor(Some(5), None, Some(0), 2), Some(0));
        assert_eq!(choose_monitor(None, Some(2), Some(1), 2), Some(1));
        assert_eq!(choose_monitor(None, None, None, 2), None);
        assert_eq!(choose_monitor(Some(0), Some(0), Some(0), 0), None);
    }

    #[test]
    fn a_bottom_panel_lifts_the_work_area() {
        let panel = Strut {
            bottom: 40,
            bottom_start_x: 0,
            bottom_end_x: 1919,
            ..Strut::default()
        };
        assert_eq!(
            work_area(FHD, size(1920, 1080), &[panel]),
            rect(0, 0, 1920, 1040)
        );
    }

    #[test]
    fn every_edge_is_honoured() {
        let screen = size(1920, 1080);
        let struts = [
            Strut::from_partial([30, 0, 0, 0, 0, 1079, 0, 0, 0, 0, 0, 0]),
            Strut::from_partial([0, 20, 0, 0, 0, 0, 0, 1079, 0, 0, 0, 0]),
            Strut::from_partial([0, 0, 28, 0, 0, 0, 0, 0, 0, 1919, 0, 0]),
            Strut::from_partial([0, 0, 0, 44, 0, 0, 0, 0, 0, 0, 0, 1919]),
        ];
        assert_eq!(work_area(FHD, screen, &struts), rect(30, 28, 1870, 1008));
    }

    #[test]
    fn a_panel_on_one_monitor_leaves_the_other_alone() {
        // Two monitors side by side, the panel along the right one's bottom.
        let screen = size(3840, 1080);
        let left = rect(0, 0, 1920, 1080);
        let right = rect(1920, 0, 1920, 1080);
        let panel = Strut {
            bottom: 40,
            bottom_start_x: 1920,
            bottom_end_x: 3839,
            ..Strut::default()
        };
        assert_eq!(work_area(left, screen, &[panel]), left);
        assert_eq!(
            work_area(right, screen, &[panel]),
            rect(1920, 0, 1920, 1040)
        );
    }

    #[test]
    fn side_and_top_panels_stay_on_their_monitor() {
        let screen = size(3840, 1080);
        let left = rect(0, 0, 1920, 1080);
        let right = rect(1920, 0, 1920, 1080);
        let struts = [
            Strut::from_partial([30, 0, 0, 0, 0, 1079, 0, 0, 0, 0, 0, 0]),
            Strut::from_partial([0, 20, 0, 0, 0, 0, 0, 1079, 0, 0, 0, 0]),
            Strut::from_partial([0, 0, 28, 0, 0, 0, 0, 0, 1920, 3839, 0, 0]),
        ];
        assert_eq!(work_area(left, screen, &struts), rect(30, 0, 1890, 1080));
        assert_eq!(
            work_area(right, screen, &struts),
            rect(1920, 28, 1900, 1052)
        );
    }

    #[test]
    fn a_panel_on_the_middle_monitor_leaves_both_neighbours_alone() {
        let screen = size(5760, 1080);
        let monitors = [
            rect(0, 0, 1920, 1080),
            rect(1920, 0, 1920, 1080),
            rect(3840, 0, 1920, 1080),
        ];
        let panel = Strut {
            bottom: 40,
            bottom_start_x: 1920,
            bottom_end_x: 3839,
            ..Strut::default()
        };
        let areas = monitors.map(|m| work_area(m, screen, &[panel]));
        assert_eq!(areas, [monitors[0], rect(1920, 0, 1920, 1040), monitors[2]]);
    }

    #[test]
    fn a_span_includes_its_end() {
        let screen = size(3840, 1080);
        let left = rect(0, 0, 1920, 1080);
        let right = rect(1920, 0, 1920, 1080);
        let just_left = Strut {
            bottom: 40,
            bottom_start_x: 0,
            bottom_end_x: 1919,
            ..Strut::default()
        };
        assert_eq!(work_area(right, screen, &[just_left]), right);
        assert_eq!(
            work_area(left, screen, &[just_left]),
            rect(0, 0, 1920, 1040)
        );
        let one_column_over = Strut {
            bottom_end_x: 1920,
            ..just_left
        };
        assert_eq!(
            work_area(right, screen, &[one_column_over]),
            rect(1920, 0, 1920, 1040)
        );
    }

    #[test]
    fn a_side_panel_on_one_stacked_monitor_misses_the_other() {
        let screen = size(1920, 2160);
        let upper = rect(0, 0, 1920, 1080);
        let lower = rect(0, 1080, 1920, 1080);
        let on_upper = Strut::from_partial([50, 0, 0, 0, 0, 1079, 0, 0, 0, 0, 0, 0]);
        let on_lower = Strut::from_partial([30, 0, 0, 0, 1080, 2159, 0, 0, 0, 0, 0, 0]);
        assert_eq!(work_area(lower, screen, &[on_upper]), lower);
        assert_eq!(work_area(upper, screen, &[on_lower]), upper);
        assert_eq!(
            work_area(upper, screen, &[on_upper, on_lower]),
            rect(50, 0, 1870, 1080)
        );
        assert_eq!(
            work_area(lower, screen, &[on_upper, on_lower]),
            rect(30, 1080, 1890, 1080)
        );
    }

    #[test]
    fn a_bottom_strut_on_the_lower_monitor_misses_the_upper_one() {
        let screen = size(1920, 2160);
        let upper = rect(0, 0, 1920, 1080);
        let lower = rect(0, 1080, 1920, 1080);
        let panel = Strut {
            bottom: 40,
            bottom_start_x: 0,
            bottom_end_x: 1919,
            ..Strut::default()
        };
        assert_eq!(work_area(upper, screen, &[panel]), upper);
        assert_eq!(
            work_area(lower, screen, &[panel]),
            rect(0, 1080, 1920, 1040)
        );
    }

    #[test]
    fn a_strut_below_a_shorter_monitor_reaches_it_where_it_overlaps() {
        // A 1080-high monitor beside a 1440-high one: a panel at the short
        // monitor's bottom reserves from the screen's bottom, 400 px.
        let screen = size(4480, 1440);
        let short = rect(0, 0, 1920, 1080);
        let panel = Strut {
            bottom: 400,
            bottom_start_x: 0,
            bottom_end_x: 1919,
            ..Strut::default()
        };
        assert_eq!(work_area(short, screen, &[panel]), rect(0, 0, 1920, 1040));
    }

    #[test]
    fn legacy_struts_span_whole_edges() {
        let screen = size(1920, 1080);
        let strut = Strut::from_legacy([0, 0, 0, 32], screen);
        assert_eq!(strut.bottom_end_x, 1919);
        assert_eq!(strut.left_end_y, 1079);
        assert_eq!(work_area(FHD, screen, &[strut]), rect(0, 0, 1920, 1048));
    }

    #[test]
    fn struts_that_eat_the_monitor_leave_an_empty_area() {
        let screen = size(100, 100);
        let monitor = rect(0, 0, 100, 100);
        let struts = [Strut::from_legacy([60, 60, 0, 0], screen)];
        assert_eq!(work_area(monitor, screen, &struts).width, 0);
    }
}
