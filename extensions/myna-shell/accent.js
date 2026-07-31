// accent.js — PURE accent-color + reduced-motion resolution (feature
// 004-gnome-shell-indicator, 2026-07-30 wave-ribbon redesign; research R18/
// R19; contract extension.md X25/X26). Resolves GNOME's desktop-wide
// accent-color preference (`org.gnome.desktop.interface`'s `accent-color`,
// GNOME 47+) into the wave ribbon's colour palette, and the `enable-
// animations` preference into a reduced-motion boolean.
//
// Two layers, mirroring the vumeter.js (pure)/dbus.js (Gio-seamed) split
// already established in this bundle:
//   - Pure, directly-testable functions (`resolveAccentPalette`,
//     `resolveReducedMotion`, `derivePalette`) that take already-unwrapped
//     values (a name-or-null, a boolean-or-null) — no Gio import needed to
//     exercise them (test/accent.test.js).
//   - `SystemPreferences`, a small Gio.Settings-backed class (same
//     schema/key-existence-guard + live `changed::` pattern dbus.js uses
//     for org.myna.Dictation) that actually reads the desktop's GSettings
//     and feeds the pure resolvers, with injectable seams for testability.
//
// CRITICAL correctness rule (R18): `Gio.Settings.get_user_value()`, not
// `get_string()`/`St.Settings`, is what lets "the user genuinely chose
// blue" and "the user never touched this setting" (also `'blue'`, GNOME's
// factory default) be told apart — both read identically any other way.

import Gio from 'gi://Gio';

const SCHEMA_ID = 'org.gnome.desktop.interface';
const ACCENT_KEY = 'accent-color';
const MOTION_KEY = 'enable-animations';

// The 9-value libadwaita `Adw.AccentColor` palette (GNOME 47+), from
// libadwaita's `_colors.scss` / `adw_accent_color_to_rgba`.
export const ACCENT_HEX = {
    blue: '#3584e4',
    teal: '#2190a4',
    green: '#3a944a',
    yellow: '#c88800',
    orange: '#ed5b00',
    red: '#e62d42',
    pink: '#d56199',
    purple: '#9141ac',
    slate: '#6f8396',
};

// Fallback when the user has not actively chosen an accent colour
// (untouched default, or the schema/key is unavailable on an older shell) —
// the design decision doc's "default Ubuntu orange".
export const UBUNTU_ORANGE = '#E95420';

// The design decision doc's specific override: when the ribbon's primary
// colour is orange, the darker/complementary secondary tone is a fixed
// Ubuntu-brand aubergine, not a generically-computed colour-wheel
// complement (2026-07-30 /speckit-analyze finding U2 — this had been
// dropped to a generic rule across the derived spec artifacts; reinstated
// here to match the source design brief).
export const UBUNTU_AUBERGINE = '#77216F';

/** Clamp to [0,1]. */
function clamp01(x) {
    return Math.max(0, Math.min(1, x));
}

function hexToRgb(hex) {
    const m = /^#?([0-9a-f]{6})$/i.exec(hex.trim());
    if (!m)
        return {r: 0, g: 0, b: 0};
    const n = parseInt(m[1], 16);
    return {r: (n >> 16) & 0xff, g: (n >> 8) & 0xff, b: n & 0xff};
}

function rgbToHex({r, g, b}) {
    const c = v => Math.round(clamp01(v / 255) * 255).toString(16).padStart(2, '0');
    return `#${c(r)}${c(g)}${c(b)}`;
}

function rgbToHsl({r, g, b}) {
    const rn = r / 255, gn = g / 255, bn = b / 255;
    const max = Math.max(rn, gn, bn), min = Math.min(rn, gn, bn);
    const l = (max + min) / 2;
    if (max === min)
        return {h: 0, s: 0, l};
    const d = max - min;
    const s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
    let h;
    switch (max) {
    case rn: h = (gn - bn) / d + (gn < bn ? 6 : 0); break;
    case gn: h = (bn - rn) / d + 2; break;
    default: h = (rn - gn) / d + 4; break;
    }
    return {h: h / 6, s, l};
}

function hue2rgb(p, q, t) {
    let tt = t;
    if (tt < 0)
        tt += 1;
    if (tt > 1)
        tt -= 1;
    if (tt < 1 / 6)
        return p + (q - p) * 6 * tt;
    if (tt < 1 / 2)
        return q;
    if (tt < 2 / 3)
        return p + (q - p) * (2 / 3 - tt) * 6;
    return p;
}

function hslToRgb({h, s, l}) {
    if (s === 0) {
        const v = Math.round(l * 255);
        return {r: v, g: v, b: v};
    }
    const q = l < 0.5 ? l * (1 + s) : l + s - l * s;
    const p = 2 * l - q;
    return {
        r: Math.round(hue2rgb(p, q, h + 1 / 3) * 255),
        g: Math.round(hue2rgb(p, q, h) * 255),
        b: Math.round(hue2rgb(p, q, h - 1 / 3) * 255),
    };
}

/** Lighten a hex colour toward white by `amount` in [0,1] (the highlight tone). */
function lighten(hex, amount) {
    const {r, g, b} = hexToRgb(hex);
    return rgbToHex({
        r: r + (255 - r) * amount,
        g: g + (255 - g) * amount,
        b: b + (255 - b) * amount,
    });
}

/** True colour-wheel complement (hue rotated 180°), lightness pulled down slightly. */
function complement(hex) {
    const hsl = rgbToHsl(hexToRgb(hex));
    return rgbToHex(hslToRgb({
        h: (hsl.h + 0.5) % 1,
        s: hsl.s,
        l: clamp01(hsl.l * 0.75),
    }));
}

/**
 * Derive the ribbon's full palette from one resolved main colour.
 *
 * @param {string} mainHex - the resolved primary accent hex (`#rrggbb`).
 * @param {boolean} [isOrange] - true when the *name* behind mainHex is
 *     "orange" (either the libadwaita accent or the Ubuntu-orange
 *     fallback) — triggers the aubergine override.
 * @returns {{main: string, highlight: string, darkerComplement: string,
 *     translucentAlpha: number}}
 */
export function derivePalette(mainHex, isOrange = false) {
    return {
        main: mainHex,
        highlight: lighten(mainHex, 0.55),
        darkerComplement: isOrange ? UBUNTU_AUBERGINE : complement(mainHex),
        // The "translucent secondary strand" is the same main colour at
        // reduced alpha, applied by ribbon-paint.js — not a separate hue.
        translucentAlpha: 0.35,
    };
}

/**
 * Resolve the ribbon's palette from the *result* of
 * `Gio.Settings.get_user_value('accent-color')` — a GNOME accent name
 * string if the user has genuinely set one, or `null` if they never have
 * (including sitting on the untouched factory default, itself `'blue'` —
 * R18's core distinction) or the schema/key is unavailable.
 *
 * Never throws: an unrecognized name also falls back to Ubuntu orange.
 *
 * @param {(string|null)} userValue
 * @returns {{main: string, highlight: string, darkerComplement: string,
 *     translucentAlpha: number}}
 */
export function resolveAccentPalette(userValue) {
    if (userValue === null || userValue === undefined)
        return derivePalette(UBUNTU_ORANGE, true);
    const hex = ACCENT_HEX[userValue];
    if (hex === undefined)
        return derivePalette(UBUNTU_ORANGE, true);
    return derivePalette(hex, userValue === 'orange');
}

/**
 * Resolve the reduced-motion boolean from the raw `enable-animations`
 * value (or `null`/`undefined` when the schema/key is unavailable — an
 * older GNOME shell defaults to full motion, per contract X26).
 *
 * @param {(boolean|null)} enableAnimations
 * @returns {boolean} true when the ribbon should render its static/
 *     minimal-motion alternative.
 */
export function resolveReducedMotion(enableAnimations) {
    if (enableAnimations === null || enableAnimations === undefined)
        return false;
    return enableAnimations === false;
}

/**
 * Schema/key-existence-guarded lookup, never throwing even when the
 * schema or key is entirely absent (a pre-GNOME-47 shell) — the safe
 * pattern research R18 establishes.
 *
 * @param {string} schemaId
 * @param {string} key
 * @returns {(Gio.Settings|null)} a settings object scoped to that schema,
 *     or null if the schema/key doesn't exist.
 */
function lookupGuardedSettings(schemaId, key) {
    const source = Gio.SettingsSchemaSource.get_default();
    if (source === null)
        return null;
    const schema = source.lookup(schemaId, true);
    if (schema === null || !schema.has_key(key))
        return null;
    return new Gio.Settings({settings_schema: schema});
}

/**
 * SystemPreferences — the live, Gio-backed half. Reads the desktop's
 * accent-color/enable-animations preferences and re-reads them live via
 * `changed::`, feeding the pure resolvers above. Injectable seam
 * (`_lookupSettings`) mirrors dbus.js's `DictationService` pattern so this
 * is stub-testable the same way, even though the shipped contract tests
 * (T052) exercise the pure resolvers directly rather than this class.
 */
export class SystemPreferences {
    /**
     * @param {object} [callbacks]
     * @param {function((string|null)): void} [callbacks.onAccentChanged]
     * @param {function(boolean): void} [callbacks.onMotionChanged]
     * @param {function(string, string): (Gio.Settings|null)} [callbacks._lookupSettings]
     *     test seam (default `lookupGuardedSettings`).
     */
    constructor({
        onAccentChanged = null,
        onMotionChanged = null,
        _lookupSettings = null,
    } = {}) {
        this._onAccentChanged = onAccentChanged ?? (() => {});
        this._onMotionChanged = onMotionChanged ?? (() => {});
        this._lookupSettings = _lookupSettings ?? lookupGuardedSettings;
        this._settings = null;
        this._signalIds = [];
    }

    /** Current accent palette, safe to call before/after enable(). */
    get accentPalette() {
        return resolveAccentPalette(this._readAccentUserValue());
    }

    /** Current reduced-motion boolean, safe to call before/after enable(). */
    get reducedMotion() {
        return resolveReducedMotion(this._readMotionValue());
    }

    /** Start watching both keys. Safe to call when the schema is absent. */
    enable() {
        if (this._settings !== null)
            return;
        this._settings = this._lookupSettings(SCHEMA_ID, ACCENT_KEY);
        if (this._settings === null)
            return;
        this._signalIds.push(
            this._settings.connect(`changed::${ACCENT_KEY}`,
                () => this._onAccentChanged(this._readAccentUserValue())),
            this._settings.connect(`changed::${MOTION_KEY}`,
                () => this._onMotionChanged(resolveReducedMotion(this._readMotionValue()))));
    }

    /** Tear down subscriptions. Safe when dormant. */
    disable() {
        if (this._settings === null)
            return;
        for (const id of this._signalIds)
            this._settings.disconnect(id);
        this._signalIds = [];
        this._settings = null;
    }

    _readAccentUserValue() {
        if (this._settings === null)
            return null;
        const variant = this._settings.get_user_value(ACCENT_KEY);
        return variant === null ? null : variant.deep_unpack();
    }

    _readMotionValue() {
        if (this._settings === null)
            return null;
        return this._settings.get_boolean(MOTION_KEY);
    }
}
