// shellCompat.js — the Shell-version-varying API surface, in one place.
//
// The bundle targets GNOME Shell 46 (Ubuntu 24.04 LTS, `noble`) through 51.
// Four things the HUD depends on moved inside that range. Each is resolved
// here by *capability* detection rather than by `Config.PACKAGE_VERSION`, so
// one bundle runs on all of them and a downstream backport is picked up for
// free:
//
//   1. `St.Settings.reduced-motion` (later still). On 46 it does not exist;
//      the same preference is read as `enable-animations` instead.
//   2. `St.BoxLayout`'s direction: the `vertical` boolean until 47, when
//      Clutter's `orientation` enum replaced it. Passing the wrong one
//      throws out of the constructor — the pill is never built.
//   3. Unredirect control, which moved from Meta's display-scoped functions
//      to `global.compositor` in mutter 47.
//
// Detection is deliberately *lazy*, for the reason ribbonShader.js's
// `ribbonShaderSupported()` is: at module scope a throw aborts the `import`
// and takes the extension down before anything can fall back.
//
// (The pre-47 `St.Settings.accent-color` shim that used to live here moved
// to the now-deleted `accent.js`. The renderer (myna-hud) reads its accent
// from libadwaita's AdwStyleManager directly, so the host no longer needs
// to expose it.)

import Clutter from 'gi://Clutter';
import Meta from 'gi://Meta';
import St from 'gi://St';

import {motionSource, orientationProps} from './shellCompatLogic.js';

// Re-exported so callers (and test/compat-probe.js) have one import site for
// the compat surface, pure half included.
export {motionSource, orientationProps};

let capabilities = null;

/**
 * What this Shell's St.Settings actually offers. GJS defines an accessor on
 * the instance prototype for every introspected property, so `in` is an
 * exact test for "this Shell has it" — no version table to keep current.
 *
 * @returns {{reducedMotion: boolean, boxOrientation: boolean}}
 */
export function stSettingsCapabilities() {
    if (capabilities === null) {
        const settings = St.Settings.get();
        capabilities = {
            reducedMotion: 'reducedMotion' in settings,
            boxOrientation: 'orientation' in St.BoxLayout.prototype,
        };
    }
    return capabilities;
}

/**
 * Direction properties for an `St.BoxLayout`, in the spelling this Shell
 * has. Spread into the constructor:
 *
 *     new St.BoxLayout({style_class: 'x', ...boxOrientation(true)})
 *
 * @param {boolean} vertical
 * @returns {object}
 */
export function boxOrientation(vertical) {
    return orientationProps(
        stSettingsCapabilities().boxOrientation, vertical, Clutter.Orientation);
}

/**
 * Disable or re-enable mutter's unredirect optimization. Ref-counted in
 * mutter, so calls must balance exactly — the caller owns that; this only
 * picks the spelling.
 *
 * `global.compositor.disable_unredirect()` arrived in mutter 47; before it
 * the same control was display-scoped on Meta itself.
 *
 * @param {boolean} disabled
 */
export function setUnredirectDisabled(disabled) {
    const compositor = global.compositor;
    if (typeof compositor?.disable_unredirect === 'function') {
        if (disabled)
            compositor.disable_unredirect();
        else
            compositor.enable_unredirect();
        return;
    }
    if (disabled)
        Meta.disable_unredirect_for_display(global.display);
    else
        Meta.enable_unredirect_for_display(global.display);
}
