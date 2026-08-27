// hud.js — HudView: the bottom-center HUD pill (feature 004-gnome-shell-
// indicator, 2026-07-30 HUD redesign; one implementation of the view.js
// IndicatorView seam, replacing the prior RibbonView/`indicator.js`, which is
// deleted, not kept as a selectable alternate — spec Assumptions).
//
// A compact pill styled after GNOME's own volume/brightness OSD: bottom-center
// of the primary monitor's work area (R14), a mic/mic-slash icon (contextual
// on severity, X19), a content-free status label, a flowing wave-ribbon for
// the live audio level (2026-07-30 wave-ribbon redesign, R17 — replaces the
// segmented bar meter; refined 2026-07-30 per the "fabric in gentle airflow"
// design pass: a smoothed, layered ribbon rather than an oscilloscope, tinted
// amber and gently pulsing — not hidden — during a recoverable notice, R17a),
// and — for a critical error only — a dismiss (×) control that is pointer-
// reactive but never keyboard-focusable (X22, FR-007c), so a click can never
// steal keyboard focus (X11/SC-001).
//
// The "held notice" slot (recoverable vs. critical) implements the
// replace-in-place / restart-timer rules from research R15 (FR-007a/FR-007d,
// X20): any new problem descriptor (severity !== null) replaces whatever is
// currently held (never a queue); a recoverable notice's hold timer restarts
// in full on a repeat; a critical error has no timer and never auto-dismisses.
//

import Atk from 'gi://Atk';
import Clutter from 'gi://Clutter';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import St from 'gi://St';

import * as Layout from 'resource:///org/gnome/shell/ui/layout.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

import {
    iconForSeverity,
    ribbonPhaseForStateKey,
    ribbonVisibleForSeverity,
    severityAutoDismisses,
    shouldReplaceHeldNotice,
    pillColorClass,
    PILL_COLOR_CLASSES,
} from './hudLogic.js';
import {Severity} from './states.js';
import {
    applyEnvelopeSmoothing,
    computeEnvelope,
    computeRibbonModel,
    RibbonPhase,
    UNFOLD_MS,
} from './ribbon.js';
import {paintRibbon} from './ribbonPaint.js';
import {ribbonShaderSupported, ShaderRibbonActor} from './ribbonShader.js';
import {
    boxOrientation,
    DisplayPreferences,
    setUnredirectDisabled,
} from './shellCompat.js';

// ── Tunables ────────────────────────────────────────────────────────────────
const PILL_WIDTH = 360;
// No RIBBON_WIDTH: the ribbon fills whatever width its parent allocates.
// stylesheet.css's `min-width` is a floor, not a target.
const RIBBON_HEIGHT = 32;
const APPEAR_MS = 180;
const CLEAR_MS = 200;
const RECOVERABLE_HOLD_MS = 3500; // matches the prior ERROR_HOLD_MS baseline

// The frame driver loops forever and nothing reads its progress, so its
// duration is arbitrary.
const FRAME_TIMELINE_MS = 1000;

// Smoother dt bounds. The ceiling stops a long hidden stretch snapping the
// envelope to its target on the first frame back; the floor stands in for a
// missing previous frame, where a dt of 0 would snap for the same reason.
const MAX_FRAME_DT_MS = 100;
const FIRST_FRAME_DT_MS = 1000 / 60;

// The GPU rasterization path (ribbonShader.js) is the default. Opt back out
// to Cairo with `MYNA_SHELL_CAIRO_RIBBON=1` in the Shell's environment —
// Cairo remains the reference implementation, it is the one the headless
// tests exercise, and it is the one dev-lab shares (GJS cannot reach the GL
// calls GtkGLArea needs, so the tuning app has no GPU equivalent).
const USE_CAIRO_RIBBON = GLib.getenv('MYNA_SHELL_CAIRO_RIBBON') === '1';

/** Build the ribbon actor for the active rasterization path. Both expose
 * the same API, so nothing downstream needs to know which one it got.
 *
 * Cairo is also the automatic answer where the GPU path cannot run at all:
 * `Clutter.ShaderEffect`'s snippet vfunc only exists from mutter 51.alpha,
 * and metadata.json still declares Shell 50. `ribbonShaderSupported()` is
 * what decides, and it logs once when it says no. */
function createRibbonActor() {
    if (USE_CAIRO_RIBBON || !ribbonShaderSupported())
        return new WaveRibbonActor();
    return new ShaderRibbonActor();
}

// A Cairo-drawn flowing wave ribbon (R17). The math lives in ribbon.js, CSS
// resolves the Shell's native colours, and ribbonPaint.js draws them (also
// shared with dev-lab); this actor wires them together and owns the timeline.
class WaveRibbonActor extends St.DrawingArea {
    static {
        GObject.registerClass(this);
    }

    constructor() {
        super({
                styleClass: 'myna-hud-ribbon',
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
        // Caller-owned smoothing state, so ribbon.js stays pure.
        this._smoothedEnvelope = 0;
        this._lastDrawAt = 0;
        this._severityTint = null;
        this._phase = RibbonPhase.UNFOLD;
        this._startedAt = 0;
        this._phaseStartedAt = 0;

        // The desktop's accent palette and accessibility preferences.
        // Where the Shell has them natively CSS resolves the colours from
        // that shared state, including any distribution-specific accent
        // choice; where it does not (pre-47), shellCompat.js resolves them
        // and `_getThemePalette` paints its palette instead.
        this._prefs = new DisplayPreferences({
            onAccentChanged: () => this.queue_repaint(),
            onMotionChanged: () => {
                this._syncFrameTimeline();
                this.queue_repaint();
            },
        });

        // Bound to this actor, so it ticks on the actor's frame clock:
        // one callback per presented frame, vblank-aligned.
        this._frameTimeline = new Clutter.Timeline({
            actor: this,
            duration: FRAME_TIMELINE_MS,
            repeatCount: -1,
        });
        this._frameTimeline.connectObject('new-frame',
            () => this.queue_repaint(), this);
        // An actor only has a frame clock while mapped, so gate on that
        // rather than on anyone remembering to restart the driver.
        this._animating = false;
        this.connect('notify::mapped', () => this._syncFrameTimeline());

        this.reset();
        this.connect('repaint', () => this._draw());
    }

        /**
         * Restart for a fresh session: back to `unfold`, state cleared so it
         * never inherits the tail of the previous one (FR-010a).
         *
         * @param {number} [startDelayMs] - hold the unfold at zero progress
         *     for this long first. The pill's entrance is a 180 ms fade and
         *     scale, and an unfold running underneath it is both invisible
         *     (still fading up) and resampled at a changing scale every
         *     frame. Delaying it by the entrance keeps the ribbon alive on
         *     its idle line, then unfolds once the pill is settled.
         */
        reset(startDelayMs = 0) {
            const now = GLib.get_monotonic_time();
            this._startedAt = now;
            this._phase = RibbonPhase.UNFOLD;
            this._phaseStartedAt = now + startDelayMs * 1000;
            this._lastDrawAt = 0;
            this._smoothedEnvelope = 0;
            this._severityTint = null;
            this.queue_repaint();
        }

        /** Begin driving repaints from the frame clock. Idempotent. */
        startAnimation() {
            this._animating = true;
            this._syncFrameTimeline();
        }

        /** Stop driving repaints. Idempotent. */
        stopAnimation() {
            this._animating = false;
            this._syncFrameTimeline();
        }

        _syncFrameTimeline() {
            if (!this._frameTimeline)
                return;
            // Under reduced motion the model is a static flat line, so a
            // per-frame repaint would redraw an identical picture forever.
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
            // No queue_repaint(): the frame timeline already covers it.
        }

        /**
         * Force a lifecycle-phase change (R17). A no-op if already in that
         * phase, so a repeated state never restarts an in-flight animation.
         *
         * @param {string} phase - a RibbonPhase value.
         */
        setPhase(phase) {
            if (phase === this._phase)
                return;
            // The fresh-session unfold reveal hands off to flow on its own
            // (see _draw); a live descriptor arriving mid-unfold must not cut
            // it short by forcing flow early.
            if (phase === RibbonPhase.FLOW && this._phase === RibbonPhase.UNFOLD)
                return;
            this._phase = phase;
            this._phaseStartedAt = GLib.get_monotonic_time();
            this.queue_repaint();
        }

        /**
         * Set the severity tint (R17a). Severity.RECOVERABLE keeps the ribbon
         * visible, amber and gently pulsing; a critical error hides the whole
         * ribbon at the HudView level instead.
         *
         * @param {(string|null)} tint - a Severity value or null.
         */
        setSeverityTint(tint) {
            if (tint === this._severityTint)
                return;
            this._severityTint = tint;
            this.queue_repaint();
        }

        _draw() {
            const [w, h] = this.get_surface_size();
            if (w <= 0 || h <= 0)
                return;

            const now = GLib.get_monotonic_time();

            // unfold → flow on the frame clock, so the hand-off lands on a
            // real frame boundary and owns no timer.
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

            const cr = this.get_context();
            try {
                const palette = this._getThemePalette();
                if (palette !== null)
                    paintRibbon(cr, w, h, model, palette);
            } finally {
                cr.$dispose();
            }
        }

        _getThemePalette() {
            // Pre-47 the stylesheet's `-st-accent-color` cannot resolve, so
            // don't ask once per frame — and don't make St re-parse a value
            // it has no keyword for. shellCompat.js already has the palette,
            // and ribbonPaint.js's `colorToRgbFloat` takes its hex as
            // readily as a resolved colour object.
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
            this._prefs?.destroy();
            this._prefs = null;
            super.destroy();
        }
}

/** HudView — implements the view.js IndicatorView interface. */
export class HudView {
    constructor() {
        this._holdTimer = 0;
        this._lastRms = 0;
        this._lastPeak = 0;
        this._shown = false;
        this._unredirectDisabled = false;
        this._colorClass = null;
        // The single held-notice slot (R15): {severity, statusText} or null.
        this._held = null;
        // Built here rather than on the first show(): actor construction, a
        // Gio.Settings open and a full CSS resolve are not work to do in the
        // frame the pill is trying to appear in. osdWindow.js builds its OSD
        // at startup for the same reason. destroy() nulls these, and every
        // entry point tolerates that.
        this._buildActor();
    }

    show(descriptor) {
        if (this._box === null)
            return;
        this._appear();
        this._applyDescriptor(descriptor);
    }

    setLevel(rms, peak) {
        // Levels can arrive before the State transition presents the HUD, so
        // cache them and the first rendered frame is already live.
        this._lastRms = rms;
        this._lastPeak = peak;
        this._ribbon?.setLevel(rms, peak);
    }

    hide() {
        // A held notice/error outlives a wire idle, including the synthesized
        // one on daemon crash: it clears on its own timer (FR-007a) or the
        // user's dismiss (FR-007b), never here.
        if (this._held !== null)
            return;
        this._dismiss();
    }

    destroy() {
        this._stopTimers();
        this._setUnredirectDisabled(false);
        this._ribbon?.stopAnimation();
        if (this._container !== null) {
            Main.layoutManager.removeChrome(this._container);
            this._container.destroy();
        }
        this._container = null;
        this._box = null;
        this._icon = null;
        this._label = null;
        this._ribbon = null;
        this._dismissButton = null;
        this._held = null;
        this._shown = false;
        this._colorClass = null;
    }

    // ── internals ────────────────────────────────────────────────────────────

    _applyDescriptor(descriptor) {
        const {severity, statusText} = descriptor;

        if (shouldReplaceHeldNotice(severity)) {
            // R15/X20: replace in place — never stack/queue a second notice.
            this._held = {severity, statusText};
            this._cancelHoldTimer();
            if (severityAutoDismisses(severity))
                this._armHoldTimer();
        } else {
            // Leaving a held notice for ANY reason cancels its timer. An
            // orphaned one would tear the pill down mid-recording.
            this._held = null;
            this._cancelHoldTimer();
        }

        // Guarded on change: an identical write still invalidates St's theme
        // node, and this runs on every state emission.
        const iconName = iconForSeverity(severity);
        if (this._icon.icon_name !== iconName)
            this._icon.icon_name = iconName;
        if (this._label.text !== statusText) {
            this._label.text = statusText;
            this._box.set_accessible_name(
                statusText ? `Dictation: ${statusText}` : 'Dictation');
        }

        // Colour-code severity and the cold-load phase, so the treatment
        // reads at a glance and not just from the label text.
        const colorClass = pillColorClass(descriptor);
        if (colorClass !== this._colorClass) {
            for (const cls of PILL_COLOR_CLASSES)
                this._box.remove_style_class_name(cls);
            if (colorClass !== null)
                this._box.add_style_class_name(colorClass);
            this._colorClass = colorClass;
        }

        // Only a critical error hides the ribbon (R17a).
        const ribbonVisible = ribbonVisibleForSeverity(severity);
        if (this._ribbon.visible !== ribbonVisible)
            this._ribbon.visible = ribbonVisible;
        this._ribbon.setSeverityTint(severity);

        // The live states pin the ribbon to their motion's phase (so leaving
        // a terminal morph/complete recovers); notice/error/idle leave it be.
        const forcedPhase = ribbonPhaseForStateKey(descriptor.key);
        if (forcedPhase !== null)
            this._ribbon.setPhase(forcedPhase);

        const dismissVisible = severity === Severity.CRITICAL;
        if (this._dismissButton.visible !== dismissVisible)
            this._dismissButton.visible = dismissVisible;
    }

    _armHoldTimer() {
        this._holdTimer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, RECOVERABLE_HOLD_MS, () => {
            this._holdTimer = 0;
            this._held = null;
            this._dismiss();
            return GLib.SOURCE_REMOVE;
        });
        GLib.Source.set_name_by_id(this._holdTimer, '[myna-shell] notice hold');
    }

    _cancelHoldTimer() {
        if (this._holdTimer === 0)
            return;
        GLib.source_remove(this._holdTimer);
        this._holdTimer = 0;
    }

    _onDismissClicked() {
        this._cancelHoldTimer();
        this._held = null;
        this._dismiss();
    }

    // Runs once per enable(): the pill is reused for every session, so
    // presenting it costs a fade and nothing else.
    _buildActor() {
        this._icon = new St.Icon({
            style_class: 'myna-hud-icon',
            icon_name: 'audio-input-microphone-symbolic',
            icon_size: 20,
        });
        this._label = new St.Label({
            style_class: 'myna-hud-label',
            text: '',
            y_align: Clutter.ActorAlign.CENTER,
        });
        this._ribbon = createRibbonActor();
        this._ribbon.setLevel(this._lastRms, this._lastPeak);
        // The only reactive actor in this chrome. `can_focus` stays false
        // though it is clickable: that is the point (X11/FR-007c).
        this._dismissButton = new St.Icon({
            style_class: 'myna-hud-dismiss',
            icon_name: 'window-close-symbolic',
            icon_size: 16,
            reactive: true,
            can_focus: false,
            visible: false,
        });
        const contentBox = new St.BoxLayout({
            style_class: 'myna-hud-content',
            ...boxOrientation(true),
            reactive: false,
            can_focus: false,
            // Claim and fill the pill's leftover width rather than
            // collapsing to the label's; the ribbon needs this to reach the
            // right edge.
            x_expand: true,
            x_align: Clutter.ActorAlign.FILL,
        });
        contentBox.add_child(this._label);
        contentBox.add_child(this._ribbon);

        this._box = new St.BoxLayout({
            style_class: 'myna-hud-pill',
            ...boxOrientation(false),
            reactive: false,
            can_focus: false,
            width: PILL_WIDTH,
            opacity: 0,
            visible: false,
            // The bottom gap is stylesheet.css's `margin-bottom`: St
            // implements CSS margins by writing the actor's margin
            // properties, so setting them here would be overwritten. The
            // expand flags are load-bearing, not tidiness: without them
            // alignment has no slack and `y_align: END` renders as centred.
            x_align: Clutter.ActorAlign.CENTER,
            y_align: Clutter.ActorAlign.END,
            x_expand: true,
            y_expand: true,
        });
        this._box.set_pivot_point(0.5, 0.5);
        this._box.set_accessible_role(Atk.Role.STATUSBAR);
        this._box.add_child(this._icon);
        this._box.add_child(contentBox);
        this._box.add_child(this._dismissButton);
        this._dismissButton.connectObject('button-press-event', () => {
            this._onDismissClicked();
            return Clutter.EVENT_STOP;
        }, this._box);

        // Spans the primary monitor's work area, so the pill clears a bottom
        // dock. MonitorConstraint owns monitor changes.
        this._container = new Clutter.Actor({
            layout_manager: new Clutter.BinLayout(),
            reactive: false,
        });
        this._container.add_constraint(new Layout.MonitorConstraint({
            primary: true,
            workArea: true,
        }));
        this._container.add_child(this._box);

        Main.layoutManager.addChrome(this._container);
    }

    // A no-op when already on screen, so a burst of state changes never
    // restarts the entrance.
    _appear() {
        if (this._shown || this._box === null)
            return;
        this._shown = true;

        // Over a fullscreen window mutter may scan the window out directly,
        // and an overlay appearing forces it in and out of that path.
        this._setUnredirectDisabled(true);

        // Chrome siblings paint in insertion order, and the Ubuntu dock
        // re-adds itself on every re-track, so it can land above us at any
        // time and hide the pill completely. Raise on each present, which is
        // what osdWindow.js does and for the same reason.
        this._container.get_parent()?.set_child_above_sibling(this._container, null);

        // Still on screen means a dismiss fade is in flight: pick the pill up
        // from wherever that fade left it and carry it back to full, rather
        // than re-running an entrance over an actor the user can still see.
        // Re-seeding the scale would shrink it, and resetting the ribbon
        // would collapse a live wave flat and re-unfold it.
        const reversing = this._box.visible;

        this._box.remove_all_transitions();
        if (!reversing) {
            this._ribbon.reset(APPEAR_MS);
            this._ribbon.setLevel(this._lastRms, this._lastPeak);
            this._box.set_scale(0.9, 0.9);
            this._box.show();
        }
        this._ribbon.startAnimation();
        // Clutter truncates rather than clamps the animated opacity, so only
        // scale (a double) may overshoot.
        this._box.ease_property('opacity', 255, {
            duration: APPEAR_MS,
            mode: Clutter.AnimationMode.EASE_OUT_QUAD,
        });
        this._box.ease({
            scale_x: 1.0,
            scale_y: 1.0,
            duration: APPEAR_MS,
            mode: Clutter.AnimationMode.EASE_OUT_BACK,
        });
    }

    _dismiss() {
        if (!this._shown || this._box === null)
            return;
        this._shown = false;
        this._stopTimers();

        this._box.remove_all_transitions();
        this._box.ease({
            opacity: 0,
            scale_x: 0.9,
            scale_y: 0.9,
            duration: CLEAR_MS,
            mode: Clutter.AnimationMode.EASE_IN_OUT_CUBIC,
            onComplete: () => {
                // A show() during the fade, or a destroy(), can have run
                // since; re-check before hiding for real.
                if (this._shown || this._box === null)
                    return;
                this._box.hide();
                this._ribbon.stopAnimation();
                this._setUnredirectDisabled(false);
            },
        });
    }

    // Ref-counted in mutter, so these must balance exactly, including across
    // a destroy() with a fade still in flight.
    _setUnredirectDisabled(disabled) {
        if (disabled === this._unredirectDisabled)
            return;
        this._unredirectDisabled = disabled;
        setUnredirectDisabled(disabled);
    }

    _stopTimers() {
        this._cancelHoldTimer();
    }
}
