// shellCompat.js — the Shell-version-varying API surface, in one place.
//
// The bundle targets GNOME Shell 46 (Ubuntu 24.04 LTS, `noble`) through 51.
// Four things the HUD depends on moved inside that range. Each is resolved
// here by *capability* detection rather than by `Config.PACKAGE_VERSION`, so
// one bundle runs on all of them and a downstream backport is picked up for
// free:
//
//   1. `St.Settings.accent-color` (47+) and `St.Settings.reduced-motion`
//      (later still). On 46 neither property exists, and merely *connecting*
//      to `notify::accent-color` throws — which would take the whole HUD
//      down at construction, not degrade it.
//   2. `-st-accent-color` in stylesheet.css, which resolves only where (1)
//      does. Where it doesn't, `lookup_color` reports not-found and the
//      caller paints `accentPalette` instead: accent.js's GSettings-backed
//      resolution, which this bundle already carried for dev-lab and which
//      lands on the documented Ubuntu-orange default when — as on 46 —
//      there is no `accent-color` key in the schema at all.
//   3. `St.BoxLayout`'s direction: the `vertical` boolean until 47, when
//      Clutter's `orientation` enum replaced it. Passing the wrong one
//      throws out of the constructor, like (1) — the pill is never built.
//   4. Unredirect control, which moved from Meta's display-scoped functions
//      to `global.compositor` in mutter 47.
//
// Detection is deliberately *lazy*, for the reason ribbonShader.js's
// `ribbonShaderSupported()` is: at module scope a throw aborts the `import`
// and takes the extension down before anything can fall back.

import Clutter from 'gi://Clutter';
import Meta from 'gi://Meta';
import St from 'gi://St';

import {SystemPreferences} from './accent.js';
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
 * @returns {{accentColor: boolean, reducedMotion: boolean,
 *     boxOrientation: boolean}}
 */
export function stSettingsCapabilities() {
    if (capabilities === null) {
        const settings = St.Settings.get();
        capabilities = {
            accentColor: 'accentColor' in settings,
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
 * DisplayPreferences — St.Settings' accent palette and reduced-motion
 * preference, with the pre-47 halves filled in.
 *
 * Replaces the direct `St.Settings.get()` + `connectObject` both ribbon
 * actors used to do. It disconnects explicitly on `destroy()` rather than
 * relying on `connectObject`'s actor-lifetime tracking, because the object
 * that owns these signals is now this one and not the actor.
 */
export class DisplayPreferences {
    /**
     * @param {object} [callbacks]
     * @param {function(): void} [callbacks.onAccentChanged]
     * @param {function(): void} [callbacks.onMotionChanged]
     */
    constructor({onAccentChanged = null, onMotionChanged = null} = {}) {
        const caps = stSettingsCapabilities();
        this._onAccentChanged = onAccentChanged ?? (() => {});
        this._onMotionChanged = onMotionChanged ?? (() => {});
        this._settings = St.Settings.get();
        this._signalIds = [];
        this._legacyAccent = null;

        this._motion = motionSource(caps);
        this._signalIds.push(this._settings.connect(
            this._motion.signal, () => this._onMotionChanged()));

        if (caps.accentColor) {
            this._signalIds.push(this._settings.connect(
                'notify::accent-color', () => this._onAccentChanged()));
        } else {
            // Pre-47: CSS's `-st-accent-color` cannot resolve, so resolve
            // the palette ourselves and hand it to the painter directly.
            this._legacyAccent = new SystemPreferences({
                onAccentChanged: () => this._onAccentChanged(),
            });
            this._legacyAccent.enable();
        }
    }

    /** Whether animation should be suppressed. Present on every Shell. */
    get reducedMotion() {
        return this._settings !== null && this._motion.read(this._settings);
    }

    /**
     * The palette to paint with when CSS cannot supply one, or `null` where
     * CSS can — in which case the resolved theme colours win, since they
     * also carry any distribution-specific accent the shell knows about and
     * this fallback does not.
     *
     * @returns {(object|null)} an accent.js hex palette, or null.
     */
    get accentPalette() {
        return this._legacyAccent?.accentPalette ?? null;
    }

    /** Tear down every subscription. Idempotent. */
    destroy() {
        for (const id of this._signalIds)
            this._settings?.disconnect(id);
        this._signalIds = [];
        this._legacyAccent?.disable();
        this._legacyAccent = null;
        this._settings = null;
    }
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
