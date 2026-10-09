//! The ICCCM/EWMH property arithmetic the X11 host needs, as plain data.

use myna_platform::status_surface::{Point, Rect, Size, Strut};

/// `WM_HINTS` `InputHint` flag.
const INPUT_HINT: u32 = 1;
/// `WM_SIZE_HINTS` `USPosition` and `PPosition` flags.
const US_POSITION: u32 = 1;
const P_POSITION: u32 = 4;
/// `WM_SIZE_HINTS` is eighteen cardinals.
const SIZE_HINTS_LEN: usize = 18;
/// `WM_HINTS` is nine.
const WM_HINTS_LEN: usize = 9;

/// `WM_HINTS` asking never to be given focus: `InputHint` set, `input`
/// false, the other fields kept.
pub fn refuse_input(wm_hints: &[u32]) -> Vec<u32> {
    let mut hints = padded(wm_hints, WM_HINTS_LEN);
    hints[0] |= INPUT_HINT;
    hints[1] = 0;
    hints
}

/// `WM_NORMAL_HINTS` that tell the window manager the position is chosen,
/// so it maps the window at `at` instead of placing it. Nothing else: the
/// size limits GDK last wrote are the previous content's, and would hold
/// the window to them; GDK writes the current ones after the map.
pub fn position_hints(at: Point) -> Vec<u32> {
    let mut hints = vec![0; SIZE_HINTS_LEN];
    hints[0] = US_POSITION | P_POSITION;
    hints[1] = at.x as u32;
    hints[2] = at.y as u32;
    hints
}

/// `WM_PROTOCOLS` without `take_focus`: a window offering `WM_TAKE_FOCUS`
/// is focusable whatever its input hint says.
pub fn without_take_focus(protocols: &[u32], take_focus: u32) -> Vec<u32> {
    protocols
        .iter()
        .copied()
        .filter(|&atom| atom != take_focus)
        .collect()
}

/// A window's struts: `_NET_WM_STRUT_PARTIAL` when it has twelve
/// cardinals, else the legacy `_NET_WM_STRUT` when that has four.
pub fn strut(partial: &[u32], legacy: &[u32], screen: Size) -> Option<Strut> {
    if let Ok(partial) = <[u32; 12]>::try_from(partial) {
        return Some(Strut::from_partial(partial));
    }
    <[u32; 4]>::try_from(legacy)
        .ok()
        .map(|legacy| Strut::from_legacy(legacy, screen))
}

/// The monitor containing `point`, first match wins.
pub fn monitor_at(point: Point, monitors: &[Rect]) -> Option<usize> {
    monitors.iter().position(|m| {
        point.x >= m.x && point.x < m.x + m.width && point.y >= m.y && point.y < m.y + m.height
    })
}

/// The centre of a window at `origin` of size `size`.
pub fn centre(origin: Point, size: Size) -> Point {
    Point {
        x: origin.x + size.width / 2,
        y: origin.y + size.height / 2,
    }
}

fn padded(values: &[u32], len: usize) -> Vec<u32> {
    let mut out = values.to_vec();
    out.resize(len.max(values.len()), 0);
    out
}

/// `area` lowered to clear any dock along `monitor`'s bottom that claims no
/// strut, as `myna-shell` does for dash-to-dock: Xubuntu's launcher hides
/// intelligently, and reserves where it slides out to even while hidden
/// below the edge. `docks` are the dock windows' current rectangles.
pub fn clear_of_bottom_docks(area: Rect, monitor: Rect, docks: &[Rect]) -> Rect {
    let bottom = monitor.y + monitor.height;
    let lowest = docks
        .iter()
        .filter(|d| d.x < monitor.x + monitor.width && monitor.x < d.x + d.width)
        // In the lower half, and down to the edge or past it (hidden).
        .filter(|d| d.y >= monitor.y + monitor.height / 2 && d.y + d.height >= bottom)
        .map(|d| bottom - d.height)
        .min();
    match lowest {
        Some(top) if top < area.y + area.height => Rect {
            height: (top - area.y).max(0),
            ..area
        },
        _ => area,
    }
}
