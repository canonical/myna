// shellCompatLogic.js — the pure half of shellCompat.js (2026-08-27 Shell 46
// backport), factored out for the same reason hudLogic.js was: its seam
// imports St and Meta, and those live in mutter's and gnome-shell's private
// typelibs. Anything importing them is unreachable from the headless suite,
// so the decisions worth locking down live here, where `gjs -m` can reach
// them with nothing installed.
//
// No `gi://` imports belong in this file.

/**
 * Which St.Settings signal carries a reduced-motion change, and how to read
 * the current value off the settings object.
 *
 * `reduced-motion` is the modern spelling; before it existed the same
 * preference was `enable-animations`, which is still present on every Shell
 * in this bundle's range — so the fallback is safe rather than merely old.
 * Note the inversion: animations *enabled* is reduced motion *off*.
 *
 * @param {{reducedMotion: boolean}} caps - from `stSettingsCapabilities()`.
 * @returns {{signal: string, read: function(object): boolean}} the signal to
 *     connect and a reader returning true when motion should be suppressed.
 */
export function motionSource(caps) {
    if (caps.reducedMotion)
        return {signal: 'notify::reduced-motion', read: s => s.reducedMotion};
    return {signal: 'notify::enable-animations', read: s => !s.enableAnimations};
}

/**
 * The construction property that gives an `St.BoxLayout` its direction.
 *
 * St.BoxLayout gained Clutter's `orientation` enum in Shell 47 and lost the
 * older `vertical` boolean in 48, so the two spellings overlap but neither
 * spans this bundle's range. Passing the wrong one is not a no-op: GJS
 * raises "No property orientation on StBoxLayout" out of the constructor,
 * which takes the whole pill down.
 *
 * The enum is passed in rather than imported so this stays free of `gi://`.
 *
 * @param {boolean} hasOrientation - whether St.BoxLayout has `orientation`.
 * @param {boolean} vertical - the direction wanted.
 * @param {object} orientationEnum - `Clutter.Orientation`.
 * @returns {object} properties to spread into the constructor.
 */
export function orientationProps(hasOrientation, vertical, orientationEnum) {
    if (hasOrientation) {
        return {
            orientation: vertical
                ? orientationEnum.VERTICAL : orientationEnum.HORIZONTAL,
        };
    }
    return {vertical};
}
