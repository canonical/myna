// place.js — PURE placement math for the hosted overlay window (feature
// 004-gnome-shell-indicator, 2026-08-26 architecture revision; research R21;
// contract extension.md XH1). No Shell/gi imports — unit-tested headless by
// test/place.test.js, the same pure-layer split the bundle has always used.
//
// The renderer application's window is a real client window: on Wayland a
// client cannot position itself, so the host computes the frame position and
// applies it with `move_frame`. This module is that computation and nothing
// else — the caller supplies the monitor's work area and the window's own
// frame size, both read from mutter.

/** Gap between the pill's bottom edge and the work area's bottom edge (px).
 * Matches the OSD-like placement the in-Shell pill used (its stylesheet's
 * `margin-bottom`), so the hosted window lands where users already expect
 * the indicator to be. */
export const BOTTOM_MARGIN = 24;

/**
 * Where to put the hosted window: horizontally centred on the work area,
 * sitting `BOTTOM_MARGIN` above its bottom edge (FR-004).
 *
 * Never returns off-screen coordinates: a window wider or taller than the
 * work area is clamped to the work area's own origin rather than being
 * pushed off the left/top edge (a 4K→1024x768 monitor switch, or a
 * mis-sized window during its first frame, must not fling the pill out of
 * view).
 *
 * @param {{x: number, y: number, width: number, height: number}} workArea -
 *     the target monitor's work area, in global coordinates.
 * @param {{width: number, height: number}} windowSize - the window's current
 *     frame size.
 * @param {number} [bottomMargin]
 * @returns {{x: number, y: number}} the frame position to apply.
 */
export function computePlacement(workArea, windowSize, bottomMargin = BOTTOM_MARGIN) {
    const centredX = workArea.x + Math.round((workArea.width - windowSize.width) / 2);
    const bottomY = workArea.y + workArea.height - windowSize.height - bottomMargin;
    return {
        x: Math.max(workArea.x, centredX),
        y: Math.max(workArea.y, bottomY),
    };
}

/**
 * Whether a freshly computed placement differs from where the window
 * already is. The host disconnects its own position/size handlers around
 * every programmatic move (anti-feedback), but skipping no-op moves keeps
 * `monitors-changed`/`size-changed` storms from generating churn at all.
 *
 * @param {{x: number, y: number}} current
 * @param {{x: number, y: number}} target
 * @returns {boolean}
 */
export function placementChanged(current, target) {
    return current.x !== target.x || current.y !== target.y;
}
