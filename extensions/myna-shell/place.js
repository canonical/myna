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

/**
 * Shrink a work area so a bottom overlay dock's reserved extent is left
 * clear. dash-to-dock in auto-hide mode claims no strut, so its extent is
 * NOT reflected in the work area; raising the work area's bottom to the
 * dock's top edge keeps the pill above where the dock would slide out.
 *
 * The shrink applies only to a dock whose `side` equals `bottomSide`
 * (`St.Side.BOTTOM`) — a dock on the left or right (vertical mode) does not
 * occupy the strip the pill lives in, so it is ignored. And it applies only
 * when the dock does NOT claim a real strut (`affectsStruts` false): a
 * dock-fixed dock already shrinks the work area, and adding its extent
 * again would over-shrink.
 *
 * @param {{x: number, y: number, width: number, height: number}} workArea
 * @param {{side: number, x: number, y: number, width: number, height: number, affectsStruts?: boolean}|null} dockExtent -
 *     the reserved extent on this monitor, or `null`.
 * @param {number} bottomSide - the `St.Side` value for the bottom edge
 *     (imported by the caller; this module stays gi-free and testable).
 * @returns {object} the (possibly shrunk) work area.
 */
export function shrinkWorkAreaForDock(workArea, dockExtent, bottomSide) {
    // Only a bottom dock matters, and only one that does not already claim
    // a strut (the work area excludes it in that case).
    if (!dockExtent || dockExtent.side !== bottomSide || dockExtent.affectsStruts)
        return workArea;

    const reservedTop = dockExtent.y;
    const workAreaBottom = workArea.y + workArea.height;
    if (reservedTop >= workAreaBottom)
        return workArea;

    // Read the fields explicitly rather than spreading: a Meta.Rectangle
    // (or any GObject boxed work area) has its fields as GObject properties
    // that object spread does NOT copy — spreading one would drop x/y/width
    // and leave a broken `{height}`-only object.
    return {
        x: workArea.x,
        y: workArea.y,
        width: workArea.width,
        height: Math.max(0, reservedTop - workArea.y),
    };
}
