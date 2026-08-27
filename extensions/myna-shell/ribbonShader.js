// ribbonShader.js — the GPU rasterization path for the wave ribbon
// (feature 004-gnome-shell-indicator, 2026-08-21). A drop-in alternative to
// hud.js's Cairo `WaveRibbonActor`, exposing the identical API
// (`reset`/`startAnimation`/`stopAnimation`/`setLevel`/`setPhase`/
// `setSeverityTint`/`destroy`) so `HudView` can swap one for the other
// without knowing which is in play.
//
// # Division of labour
//
// The MODEL stays on the CPU, unchanged: `computeRibbonModel` runs exactly
// as it does for Cairo, so the phase state machine, the envelope smoothing
// and the amplitude response curve remain pure, headlessly testable JS and
// remain the single authority for *what* to draw. Only RASTERIZATION moves
// to the GPU, and the shader regenerates each strand's sine from the
// per-strand parameters the model reports rather than from constants of its
// own (see ribbonGlsl.js).
//
// # Why not ShellGLSLEffect
//
// It was removed from gnome-shell in 30f545eb00 ("Remove GLSLEffect — now
// that everything uses ClutterShaderEffect"). `Clutter.ShaderEffect` with a
// `Cogl.Snippet` is the supported path, and is what the Shell's own
// `js/ui/lightbox.js` vignette uses from JS.

import Clutter from 'gi://Clutter';
import Cogl from 'gi://Cogl';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import St from 'gi://St';

import {
    applyEnvelopeSmoothing,
    computeEnvelope,
    computeRibbonModel,
    RibbonPhase,
    UNFOLD_MS,
} from './ribbon.js';
import {
    buildRibbonShader,
    packRibbonUniforms,
    RIBBON_UNIFORMS,
} from './ribbonGlsl.js';
import {DisplayPreferences} from './shellCompat.js';

const RIBBON_HEIGHT = 32;
const FRAME_TIMELINE_MS = 1000;
const MAX_FRAME_DT_MS = 100;
const FIRST_FRAME_DT_MS = 1000 / 60;

// # Why the subclass is registered lazily
//
// `ClutterShaderEffectClass::get_static_snippet` only exists from mutter
// 51.alpha (2d5bc0fbff, "clutter/shader-effect: Port to CoglSnippet"), and
// metadata.json declares Shell 50 as well. On mutter 50 the vfunc is simply
// absent, so GJS cannot hook the override up and `GObject.registerClass`
// throws. A class declared at module scope registers as the module is
// evaluated — a `static {}` block is no help, it runs then too — so the
// throw would land during `import` and take the WHOLE EXTENSION down: not
// merely the ribbon, and not even recoverable via hud.js's
// `MYNA_SHELL_CAIRO_RIBBON` opt-out, since the import runs long before any
// flag is read. Declaring the class inside a function is what defers it.
//
// There is no viable GPU path to fall back to on 50: the pre-snippet API
// wants a whole legacy GLSL program from `get_static_shader_source`, and
// `clutter_shader_effect_set_uniform_float` — the only introspectable way to
// push a vec2/3/4 — was added by that same port commit. So 50 gets Cairo,
// which is the reference implementation anyway.
let RibbonShaderEffect = null;
let shaderEffectSupported = null;

/**
 * Whether the GPU ribbon can run on the Shell we are loaded into. Registers
 * the effect subclass on first call and memoizes the outcome, failure
 * included, so the error is reported at most once per Shell session.
 *
 * @returns {boolean} true when `ShaderRibbonActor` is safe to construct.
 */
export function ribbonShaderSupported() {
    if (shaderEffectSupported === null) {
        try {
            RibbonShaderEffect = registerRibbonShaderEffect();
            shaderEffectSupported = true;
        } catch (e) {
            logError(e, 'myna: GPU ribbon unavailable on this Shell, ' +
                'falling back to the Cairo ribbon');
            RibbonShaderEffect = null;
            shaderEffectSupported = false;
        }
    }
    return shaderEffectSupported;
}

/**
 * The fragment shader itself. `vfunc_get_static_snippet` is called once per
 * subclass no matter how many instances exist, so the source is generated
 * once, here.
 *
 * @returns {GObject.Class} the registered `Clutter.ShaderEffect` subclass.
 */
function registerRibbonShaderEffect() {
    class RibbonShaderEffect extends Clutter.ShaderEffect {
        static {
            GObject.registerClass(this);
        }

        vfunc_get_static_snippet() {
            const {declarations, code} = buildRibbonShader();
            const snippet = Cogl.Snippet.new(
                Cogl.SnippetHook.FRAGMENT, declarations, null);
            // `replace` rather than `post`: the ribbon is generated entirely
            // from uniforms, so Cogl's own fragment output (the actor's
            // offscreen texture, which is just the placeholder background) is
            // deliberately discarded.
            snippet.set_replace(code);
            return snippet;
        }

        /**
         * Push one frame's model to the GPU. Every uniform is a scalar or a
         * vec2/3/4 — never an array — because ClutterShaderFloat asserts
         * `size <= 4`; see RIBBON_UNIFORMS. The trailing `total_count` argument
         * is the array length (1 for all of ours), which GJS infers from the
         * value array, so `components` is what distinguishes a vec4 from four
         * floats.
         *
         * @param {number} width - actor width in pixels.
         * @param {number} height - actor height in pixels.
         * @param {object} model - `computeRibbonModel` output.
         * @param {object} palette - the caller-resolved theme colours.
         */
        updateFromModel(width, height, model, palette) {
            // The packing itself is pure and lives in ribbonGlsl.js beside the
            // shader it feeds, so the dev-lab and the headless render test
            // upload byte-identical uniforms rather than a second hand-copied
            // packing that could drift from this one.
            const values = packRibbonUniforms(width, height, model, palette);
            for (const {name, components} of RIBBON_UNIFORMS)
                this.set_uniform_float(name, components, values[name]);
        }
    }

    return RibbonShaderEffect;
}

/**
 * A GPU-rasterized flowing wave ribbon. Mirrors hud.js's `WaveRibbonActor`
 * API exactly. Only construct it when `ribbonShaderSupported()` is true.
 */
export class ShaderRibbonActor extends St.Widget {
    static {
        GObject.registerClass(this);
    }

    constructor() {
        if (!ribbonShaderSupported()) {
            throw new Error(
                'ShaderRibbonActor: Clutter.ShaderEffect.get_static_snippet ' +
                'is unavailable; use the Cairo ribbon on this Shell');
        }
        super({
            styleClass: 'myna-hud-ribbon myna-hud-ribbon-gpu',
            reactive: false,
            canFocus: false,
            height: RIBBON_HEIGHT,
            xExpand: true,
            xAlign: Clutter.ActorAlign.FILL,
            yExpand: false,
        });

        this._lastRms = 0;
        this._lastPeak = 0;
        this._lastLevelAt = 0;
        this._smoothedEnvelope = 0;
        this._lastDrawAt = 0;
        this._severityTint = null;
        this._phase = RibbonPhase.UNFOLD;
        this._startedAt = 0;
        this._phaseStartedAt = 0;

        this._effect = new RibbonShaderEffect();
        this.add_effect(this._effect);

        this._prefs = new DisplayPreferences({
            onAccentChanged: () => this._updateUniforms(),
            onMotionChanged: () => {
                this._syncFrameTimeline();
                this._updateUniforms();
            },
        });

        this._frameTimeline = new Clutter.Timeline({
            actor: this,
            duration: FRAME_TIMELINE_MS,
            repeatCount: -1,
        });
        // Unlike the Cairo actor there is no 'repaint' signal to hang the
        // model update off, so each frame pushes uniforms and then queues a
        // redraw of the (already GPU-resident) quad.
        this._frameTimeline.connectObject('new-frame',
            () => this._updateUniforms(), this);

        this._animating = false;
        this.connect('notify::mapped', () => {
            // The constructor's reset() ran before the actor was parented,
            // so this is the first point at which the theme node — and
            // therefore the palette — actually resolves.
            this._updateUniforms();
            this._syncFrameTimeline();
        });
        this.connect('notify::allocation', () => this._updateUniforms());

        this.reset();
    }

    /**
     * Restart for a fresh session (FR-010a).
     *
     * @param {number} [startDelayMs] - hold the unfold at zero progress for
     *     this long first, so it doesn't run underneath the pill's entrance.
     */
    reset(startDelayMs = 0) {
        const now = GLib.get_monotonic_time();
        this._startedAt = now;
        this._phase = RibbonPhase.UNFOLD;
        this._phaseStartedAt = now + startDelayMs * 1000;
        this._lastDrawAt = 0;
        this._smoothedEnvelope = 0;
        this._severityTint = null;
        this._updateUniforms();
    }

    /** Begin driving frames. Idempotent. */
    startAnimation() {
        this._animating = true;
        this._syncFrameTimeline();
    }

    /** Stop driving frames. Idempotent. */
    stopAnimation() {
        this._animating = false;
        this._syncFrameTimeline();
    }

    _syncFrameTimeline() {
        if (!this._frameTimeline)
            return;
        // Under reduced motion the model is a static flat line, so a
        // per-frame update would push identical uniforms forever.
        const shouldRun =
            this._animating && this.mapped && !this._prefs.reducedMotion;
        if (shouldRun && !this._frameTimeline.is_playing())
            this._frameTimeline.start();
        else if (!shouldRun && this._frameTimeline.is_playing())
            this._frameTimeline.stop();
    }

    setLevel(rms, peak = rms) {
        this._lastRms = rms;
        this._lastPeak = peak;
        this._lastLevelAt = GLib.get_monotonic_time();
    }

    /**
     * Force a lifecycle-phase change (R17).
     *
     * @param {string} phase - a RibbonPhase value.
     */
    setPhase(phase) {
        if (phase === this._phase)
            return;
        // The fresh-session unfold reveal hands off to flow on its own; a
        // live descriptor arriving mid-unfold must not cut it short.
        if (phase === RibbonPhase.FLOW && this._phase === RibbonPhase.UNFOLD)
            return;
        this._phase = phase;
        this._phaseStartedAt = GLib.get_monotonic_time();
        this._updateUniforms();
    }

    /**
     * Set the severity tint (R17a).
     *
     * @param {(string|null)} tint - a Severity value or null.
     */
    setSeverityTint(tint) {
        if (tint === this._severityTint)
            return;
        this._severityTint = tint;
        this._updateUniforms();
    }

    _updateUniforms() {
        if (this._effect === null)
            return;
        // st_widget_get_theme_node() is a hard error off-stage, and the
        // constructor's reset() runs before the actor is parented. Deferred
        // to the notify::mapped handler, which pushes a full update.
        if (this.get_stage() === null)
            return;
        const [width, height] = this.get_size();
        if (width <= 0 || height <= 0)
            return;

        const palette = this._getThemePalette();
        if (palette === null)
            return;

        const now = GLib.get_monotonic_time();

        // unfold → flow on the frame clock, so the hand-off lands on a real
        // frame boundary and owns no timer (same as the Cairo actor).
        if (this._phase === RibbonPhase.UNFOLD &&
            (now - this._phaseStartedAt) / 1000 >= UNFOLD_MS) {
            this._phase = RibbonPhase.FLOW;
            this._phaseStartedAt = now;
        }

        const ageMs = this._lastLevelAt ? (now - this._lastLevelAt) / 1000 : 9999;
        const instantEnvelope = computeEnvelope(this._lastRms, this._lastPeak, ageMs);
        const dtMs = this._lastDrawAt
            ? Math.min((now - this._lastDrawAt) / 1000, MAX_FRAME_DT_MS)
            : FIRST_FRAME_DT_MS;
        this._smoothedEnvelope = applyEnvelopeSmoothing(
            this._smoothedEnvelope, instantEnvelope, dtMs);
        this._lastDrawAt = now;

        const model = computeRibbonModel({
            envelope: this._smoothedEnvelope,
            elapsedMs: (now - this._startedAt) / 1000,
            phase: this._phase,
            phaseElapsedMs: (now - this._phaseStartedAt) / 1000,
            reducedMotion: this._prefs.reducedMotion,
            severityTint: this._severityTint,
        });

        this._effect.updateFromModel(width, height, model, palette);
        this.queue_redraw();
    }

    _getThemePalette() {
        // See hud.js's twin: pre-47 there is no `-st-accent-color` for St to
        // resolve, so take shellCompat.js's palette without asking.
        const resolved = this._prefs.accentPalette;
        if (resolved !== null)
            return resolved;
        const themeNode = this.get_theme_node();
        const colors = [
            '-myna-ribbon-main-color',
            '-myna-ribbon-highlight-color',
            '-myna-ribbon-shadow-color',
        ].map(name => themeNode.lookup_color(name, false));
        if (colors.some(([found]) => !found))
            return null;
        return {
            main: colors[0][1],
            highlight: colors[1][1],
            darkerComplement: colors[2][1],
            translucentAlpha: 0.35,
        };
    }

    destroy() {
        if (this._frameTimeline !== null) {
            this._frameTimeline.stop();
            this._frameTimeline.set_actor(null);
            this._frameTimeline = null;
        }
        this._effect = null;
        this._prefs?.destroy();
        this._prefs = null;
        super.destroy();
    }
}
