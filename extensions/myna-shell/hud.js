// hud.js — HudView: the bottom-center HUD pill (feature 004-gnome-shell-
// indicator, 2026-07-30 HUD redesign; one implementation of the view.js
// IndicatorView seam, replacing the prior RibbonView/`indicator.js`, which is
// deleted, not kept as a selectable alternate — spec Assumptions).
//
// A compact pill styled after GNOME's own volume/brightness OSD: bottom-center
// of the primary monitor (R14), a mic/mic-slash icon (contextual on severity,
// X19), a content-free status label, a flowing wave-ribbon for the live audio
// level (2026-07-30 wave-ribbon redesign, R17 — replaces the segmented bar
// meter; refined 2026-07-30 per the "fabric in gentle airflow" design pass:
// a smoothed, layered ribbon rather than an oscilloscope, tinted amber and
// gently pulsing — not hidden — during a recoverable notice, R17a), and —
// for a critical error only — a dismiss (×) control that is pointer-reactive
// but never keyboard-focusable (X22, FR-007c), so a click can never steal
// keyboard focus (X11/SC-001).
//
// The "held notice" slot (recoverable vs. critical) implements the
// replace-in-place / restart-timer rules from research R15 (FR-007a/FR-007d,
// X20): any new problem descriptor (severity !== null) replaces whatever is
// currently held (never a queue); a recoverable notice's hold timer restarts
// in full on a repeat; a critical error has no timer and never auto-dismisses.
//
// Added as Shell chrome — non-reactive, non-focusable — except the dismiss
// control, so nothing here can ever take keyboard focus. Pixels/geometry are
// deliberately isolated behind this one file (and its pure `hud-logic.js`
// helper); states.js/view.js/dbus.js are untouched by this redesign. The
// wave ribbon's own math/paint (ribbon.js/ribbon-paint.js/accent.js) are
// likewise pure, Shell-independent modules — this file only wires them into
// an `St.DrawingArea` actor.

import Atk from 'gi://Atk';
import Clutter from 'gi://Clutter';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';

import {SystemPreferences} from './accent.js';
import {
    computePosition,
    iconForSeverity,
    ribbonPhaseForStateKey,
    ribbonVisibleForSeverity,
    severityAutoDismisses,
    shouldReplaceHeldNotice,
    pillColorClass,
    PILL_COLOR_CLASSES,
} from './hud-logic.js';
import {
    applyEnvelopeSmoothing,
    computeEnvelope,
    computeRibbonModel,
    DEFAULT_ENVELOPE_HZ,
    UNFOLD_MS,
} from './ribbon.js';
import {paintRibbon} from './ribbon-paint.js';

// ── Tunables ────────────────────────────────────────────────────────────────
const PILL_WIDTH = 360;
// An ESTIMATE of the pill's natural height, used only for bottom-margin
// positioning (computePosition) — the actor's real height is left to size
// naturally from its children (see the fix note on `_box` below); forcing an
// explicit `height` here previously squeezed the bar meter's allocation down
// to near-zero regardless of the real audio level, the flat-vumeter bug a
// manual test caught (2026-07-30 follow-up).
const PILL_HEIGHT_ESTIMATE = 88;
// 2026-07-31: no fixed RIBBON_WIDTH constant — the ribbon expands to fill
// whatever horizontal space its parent actually allocates (see
// WaveRibbonActor/contentBox below); a fixed width here was the bug (it
// visibly stopped partway across the pill on real hardware instead of
// reaching the right edge). stylesheet.css's `min-width: 160px` is the
// only remaining width-related constant, and it's a floor, not a target.
const RIBBON_HEIGHT = 32;
const APPEAR_MS = 180;
const CLEAR_MS = 200;
const RECOVERABLE_HOLD_MS = 3500; // matches the prior ERROR_HOLD_MS baseline

// A Cairo-drawn flowing wave ribbon (2026-07-30, R17) — replaces the prior
// segmented bar meter entirely. Envelope/strand/phase-timing math lives in
// ribbon.js, the accent-color/reduced-motion resolution in accent.js, and
// the actual Cairo drawing in ribbon-paint.js (shared verbatim with the
// standalone dev-lab tuning tool, R20); this actor only wires them together
// and owns the GLib timers.
const WaveRibbonActor = GObject.registerClass(
class WaveRibbonActor extends St.DrawingArea {
    _init() {
        super._init({
            style_class: 'myna-hud-ribbon',
            reactive: false,
            can_focus: false,
            height: RIBBON_HEIGHT,
            // 2026-07-31 fix: the ribbon previously had a hardcoded
            // `width: 160` and `x_expand: false`, so it
            // never grew past that regardless of how much wider the pill
            // actually was — it visibly stopped partway across instead of
            // reaching the right edge (reported from the real, installed
            // extension, not just dev-lab). `x_expand` + `x_align: FILL`
            // let it claim and fill whatever horizontal space its parent
            // (`contentBox`, itself now expanding into the pill's leftover
            // width below) actually allocates; `stylesheet.css`'s
            // `min-width: 160px` remains as a floor only.
            x_expand: true,
            x_align: Clutter.ActorAlign.FILL,
            y_expand: false,
        });
        this._lastRms = 0;
        this._lastPeak = 0;
        this._lastLevelAt = 0;
        // The SMOOTHED envelope actually driving the wave shape (~300 ms
        // one-pole low-pass, 2026-07-30 refinement) — distinct from
        // vumeter.js's arrival-time stale-decay above. Caller-maintained
        // state, updated once per repaint frame via `applyEnvelopeSmoothing`
        // so `ribbon.js` itself stays a pure function of its inputs.
        this._smoothedEnvelope = 0;
        this._lastDrawAt = 0;
        this._severityTint = null;
        this._startedAt = GLib.get_monotonic_time();
        // Every new actor (i.e. every new session, since HudView tears
        // down and recreates its actors between sessions) begins in the
        // brief `unfold` phase (FR-010a) and self-advances to `flow` once
        // — never interrupted by an external setPhase() for the ordinary
        // recording/loading states (hud-logic.js's `ribbonPhaseForStateKey`
        // deliberately returns null for those).
        this._phase = 'unfold';
        this._phaseStartedAt = this._startedAt;
        this._unfoldTimerId = GLib.timeout_add(
            GLib.PRIORITY_DEFAULT, UNFOLD_MS, () => {
                this._unfoldTimerId = 0;
                if (this._phase === 'unfold')
                    this.setPhase('flow');
                return GLib.SOURCE_REMOVE;
            });

        this._prefs = new SystemPreferences({
            onAccentChanged: () => this.queue_repaint(),
            onMotionChanged: () => this.queue_repaint(),
        });
        this._prefs.enable();

        this.connect('repaint', () => this._draw());
        this.connect('destroy', () => this._onDestroy());
    }

    setLevel(rms, peak = rms) {
        this._lastRms = rms;
        this._lastPeak = peak;
        this._lastLevelAt = GLib.get_monotonic_time();
        this.queue_repaint();
    }

    /**
     * Force a lifecycle-phase change (2026-07-30, R17). A no-op if already
     * in that phase, so redundant calls (e.g. the same state repeating)
     * never restart an in-flight phase animation.
     *
     * @param {('unfold'|'flow'|'relax'|'morph'|'complete')} phase
     */
    setPhase(phase) {
        if (phase === this._phase)
            return;
        this._phase = phase;
        this._phaseStartedAt = GLib.get_monotonic_time();
        this.queue_repaint();
    }

    /**
     * Set the severity tint (2026-07-30 design refinement, R17a):
     * `'recoverable'` keeps the ribbon visible, amber, and gently pulsing
     * instead of hidden; `'critical'`/`null` render normally (a critical
     * error hides the whole ribbon at the `HudView` level instead — see
     * `hud-logic.js`'s `ribbonVisibleForSeverity`).
     *
     * @param {(('recoverable'|'critical')|null)} tint
     */
    setSeverityTint(tint) {
        if (tint === this._severityTint)
            return;
        this._severityTint = tint;
        this.queue_repaint();
    }

    _draw() {
        const cr = this.get_context();
        const [w, h] = this.get_surface_size();
        const now = GLib.get_monotonic_time();
        const ageMs = this._lastLevelAt ? (now - this._lastLevelAt) / 1000 : 9999;
        const instantEnvelope = computeEnvelope(this._lastRms, this._lastPeak, ageMs);
        const dtMs = this._lastDrawAt ? (now - this._lastDrawAt) / 1000 : 1000 / DEFAULT_ENVELOPE_HZ;
        this._smoothedEnvelope = applyEnvelopeSmoothing(this._smoothedEnvelope, instantEnvelope, dtMs);
        this._lastDrawAt = now;

        const elapsedMs = (now - this._startedAt) / 1000;
        const phaseElapsedMs = (now - this._phaseStartedAt) / 1000;
        const model = computeRibbonModel({
            envelope: this._smoothedEnvelope,
            elapsedMs,
            phase: this._phase,
            phaseElapsedMs,
            reducedMotion: this._prefs.reducedMotion,
            severityTint: this._severityTint,
        });
        paintRibbon(cr, w, h, model, this._prefs.accentPalette);
        cr.$dispose();
    }

    _onDestroy() {
        if (this._unfoldTimerId !== 0) {
            GLib.source_remove(this._unfoldTimerId);
            this._unfoldTimerId = 0;
        }
        this._prefs.disable();
    }
});

/** HudView — implements the view.js IndicatorView interface. */
export class HudView {
    constructor() {
        this._box = null;
        this._icon = null;
        this._label = null;
        this._ribbon = null;
        this._dismissButton = null;
        this._monitorsChangedId = 0;
        this._ribbonTimer = 0;
        this._holdTimer = 0;
        this._lastRms = 0;
        this._lastPeak = 0;
        // The single held-notice slot (R15): {severity, statusText} or null.
        this._held = null;
    }

    show(descriptor) {
        this._ensureActor();
        this._applyDescriptor(descriptor);
    }

    setLevel(rms, peak) {
        // D-Bus levels may arrive before the State transition creates the HUD
        // actor. Cache them so the first rendered frame is live instead of
        // waiting for a numerically-different update.
        this._lastRms = rms;
        this._lastPeak = peak;
        this._ribbon?.setLevel(rms, peak);
    }

    hide() {
        // A held notice/error is never dismissed by a wire idle transition —
        // it clears on its own timer (recoverable, FR-007a) or the user's
        // explicit dismiss (critical, FR-007b) — never by this call. This
        // includes the daemon-crash/vanished edge case (dbus.js's
        // `_onVanished` synthesizes an idle transition): a still-functional
        // dismiss button is not "frozen" (FR-007b's persistence is a
        // deliberate, later, more specific requirement than the general
        // crash-clears-to-idle edge case, which predates severity tiers).
        if (this._held !== null)
            return;
        this._dismiss();
    }

    destroy() {
        this._stopTimers();
        this._detachSignals();
        if (this._box !== null) {
            Main.layoutManager.removeChrome(this._box);
            this._box.destroy();
        }
        this._box = null;
        this._icon = null;
        this._label = null;
        this._ribbon = null;
        this._dismissButton = null;
        this._held = null;
    }

    // ── internals ────────────────────────────────────────────────────────────

    _applyDescriptor(descriptor) {
        const {severity, statusText} = descriptor;

        if (shouldReplaceHeldNotice(severity)) {
            // R15/X20: replace in place — never stack/queue a second notice.
            this._held = {severity, statusText};
            if (this._holdTimer !== 0) {
                GLib.source_remove(this._holdTimer);
                this._holdTimer = 0;
            }
            if (severityAutoDismisses(severity))
                this._armHoldTimer();
        } else {
            // Bug (manual test report, 2026-07-31): leaving a held notice for
            // ANY reason — not just being replaced by a new one — must also
            // cancel any pending auto-dismiss timer. Without this, a stale
            // `_holdTimer` armed by an earlier recoverable notice (e.g. "No
            // speech detected") outlives that notice: if the state moves on
            // to a plain `recording`/`loading` descriptor before the timer
            // fires, the orphaned timer still calls `_dismiss()` ~3.5s later
            // and tears down the pill even though a genuine recording is now
            // in progress ("pill disappears while listening").
            this._held = null;
            if (this._holdTimer !== 0) {
                GLib.source_remove(this._holdTimer);
                this._holdTimer = 0;
            }
        }

        this._icon.icon_name = iconForSeverity(severity);
        this._label.text = statusText;
        this._box.set_accessible_name(statusText ? `Dictation: ${statusText}` : 'Dictation');

        // Colour-code severity (orange recoverable, red critical) and the
        // cold-load phase (warm tint) so the treatment reads at a glance, not
        // just from the label text (2026-07-30 manual-test follow-up).
        for (const cls of PILL_COLOR_CLASSES)
            this._box.remove_style_class_name(cls);
        const colorClass = pillColorClass(descriptor);
        if (colorClass !== null)
            this._box.add_style_class_name(colorClass);

        // 2026-07-30, R17a: only a critical error hides the ribbon; a
        // recoverable notice keeps it visible, tinted amber and gently
        // pulsing instead (hud-logic.js's `ribbonVisibleForSeverity`).
        this._ribbon.visible = ribbonVisibleForSeverity(severity);
        this._ribbon.setSeverityTint(severity);

        // 2026-07-30, R17: force the ribbon into `morph`/`complete` for the
        // two transitions that must visibly change its motion; every other
        // key (recording/loading/...) leaves the ribbon's own internal
        // unfold→flow phase alone (hud-logic.js's `ribbonPhaseForStateKey`).
        const forcedPhase = ribbonPhaseForStateKey(descriptor.key);
        if (forcedPhase !== null)
            this._ribbon.setPhase(forcedPhase);

        this._dismissButton.visible = severity === 'critical';
    }

    _armHoldTimer() {
        this._holdTimer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, RECOVERABLE_HOLD_MS, () => {
            this._holdTimer = 0;
            this._held = null;
            this._dismiss();
            return GLib.SOURCE_REMOVE;
        });
    }

    _onDismissClicked() {
        // FR-007c/X22: pointer-reactive but never focusable — this handler
        // only ever fires from a mouse click, never a keyboard event.
        if (this._holdTimer !== 0) {
            GLib.source_remove(this._holdTimer);
            this._holdTimer = 0;
        }
        this._held = null;
        this._dismiss();
    }

    _ensureActor() {
        if (this._box !== null)
            return;

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
        this._ribbon = new WaveRibbonActor();
        this._ribbon.setLevel(this._lastRms, this._lastPeak);
        // The dismiss (×) control: the ONLY reactive/focusable-capable actor
        // in this chrome. can_focus stays false even though it's clickable —
        // that is the whole point (X11/FR-007c): a click can dismiss it
        // without ever taking keyboard focus from the user's application.
        this._dismissButton = new St.Icon({
            style_class: 'myna-hud-dismiss',
            icon_name: 'window-close-symbolic',
            icon_size: 16,
            reactive: true,
            can_focus: false,
            visible: false,
        });
        this._dismissButton.connect('button-press-event', () => {
            this._onDismissClicked();
            return Clutter.EVENT_STOP;
        });

        const contentBox = new St.BoxLayout({
            style_class: 'myna-hud-content',
            orientation: Clutter.Orientation.VERTICAL,
            reactive: false,
            can_focus: false,
            // 2026-07-31 fix: claim the pill's leftover horizontal space
            // (icon and dismiss button stay their natural/fixed size) and
            // actually stretch into it, rather than collapsing to the
            // label's natural (narrower) width — this is what the ribbon
            // child needs from its parent to reach the pill's right edge.
            x_expand: true,
            x_align: Clutter.ActorAlign.FILL,
        });
        contentBox.add_child(this._label);
        contentBox.add_child(this._ribbon);

        this._box = new St.BoxLayout({
            style_class: 'myna-hud-pill',
            orientation: Clutter.Orientation.HORIZONTAL,
            reactive: false,
            can_focus: false,
            width: PILL_WIDTH,
            opacity: 0,
        });
        this._box.set_accessible_role(Atk.Role.STATUSBAR);
        this._box.add_child(this._icon);
        this._box.add_child(contentBox);
        this._box.add_child(this._dismissButton);

        Main.layoutManager.addChrome(this._box);
        this._monitorsChangedId = Main.layoutManager.connect(
            'monitors-changed', () => this._position());
        this._position();

        this._box.set_pivot_point(0.5, 0.5);
        this._box.set_scale(0.9, 0.9);
        this._box.ease({
            opacity: 255,
            scale_x: 1.0,
            scale_y: 1.0,
            duration: APPEAR_MS,
            mode: Clutter.AnimationMode.EASE_OUT_BACK,
        });

        this._startRibbonTimer();
    }

    _position() {
        if (this._box === null)
            return;
        const monitor = Main.layoutManager.primaryMonitor;
        const {x, y} = computePosition(monitor, PILL_WIDTH, PILL_HEIGHT_ESTIMATE);
        this._box.set_position(x, y);
    }

    _startRibbonTimer() {
        if (this._ribbonTimer !== 0)
            return;
        this._ribbonTimer = GLib.timeout_add(
            GLib.PRIORITY_DEFAULT, Math.floor(1000 / DEFAULT_ENVELOPE_HZ), () => {
                if (this._ribbon === null)
                    return GLib.SOURCE_REMOVE;
                this._ribbon.queue_repaint();
                return GLib.SOURCE_CONTINUE;
            });
    }

    _dismiss() {
        const box = this._box;
        if (box === null)
            return;
        this._stopTimers();
        this._detachSignals();
        this._box = null;
        this._icon = null;
        this._label = null;
        this._ribbon = null;
        this._dismissButton = null;
        box.remove_all_transitions();
        box.set_pivot_point(0.5, 0.5);
        box.ease({
            opacity: 0,
            scale_x: 0.9,
            scale_y: 0.9,
            duration: CLEAR_MS,
            mode: Clutter.AnimationMode.EASE_IN_OUT_CUBIC,
            onComplete: () => {
                Main.layoutManager.removeChrome(box);
                box.destroy();
            },
        });
    }

    _stopTimers() {
        if (this._ribbonTimer !== 0) {
            GLib.source_remove(this._ribbonTimer);
            this._ribbonTimer = 0;
        }
        if (this._holdTimer !== 0) {
            GLib.source_remove(this._holdTimer);
            this._holdTimer = 0;
        }
    }

    _detachSignals() {
        if (this._monitorsChangedId !== 0) {
            Main.layoutManager.disconnect(this._monitorsChangedId);
            this._monitorsChangedId = 0;
        }
    }
}
