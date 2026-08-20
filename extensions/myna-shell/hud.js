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

import {SystemPreferences} from './accent.js';
import {
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
    UNFOLD_MS,
} from './ribbon.js';
import {paintRibbon} from './ribbon-paint.js';

// ── Tunables ────────────────────────────────────────────────────────────────
const PILL_WIDTH = 360;
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

// The frame-driver timeline's own duration is irrelevant (it loops forever
// and nothing reads its progress); it exists only so the actor's frame clock
// calls us back once per presented frame.
const FRAME_TIMELINE_MS = 1000;

// Ceiling on the per-frame delta fed to the envelope smoother. Without it,
// the first frame after the HUD has been hidden for a minute would hand
// `applyEnvelopeSmoothing` a dt of tens of seconds and snap the envelope
// straight to its target — a visible pop on the very frame the pill appears.
const MAX_FRAME_DT_MS = 100;
// The dt to assume for the very first frame after a reset, when there is no
// previous frame to measure against. A nominal 60 Hz frame; passing 0 would
// make `applyEnvelopeSmoothing` snap straight to the target instead of easing.
const FIRST_FRAME_DT_MS = 1000 / 60;

// A Cairo-drawn flowing wave ribbon (2026-07-30, R17) — replaces the prior
// segmented bar meter entirely. Envelope/strand/phase-timing math lives in
// ribbon.js, the accent-color/reduced-motion resolution in accent.js, and
// the actual Cairo drawing in ribbon-paint.js (shared verbatim with the
// standalone dev-lab tuning tool, R20); this actor only wires them together
// and owns the frame-clock timeline.
const WaveRibbonActor = GObject.registerClass(
    class WaveRibbonActor extends St.DrawingArea {
        _init() {
            super._init({
                style_class: 'myna-hud-ribbon',
                reactive: false,
                can_focus: false,
                height: RIBBON_HEIGHT,
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
            this._phase = 'unfold';
            this._startedAt = 0;
            this._phaseStartedAt = 0;

            // The accent palette and reduced-motion flag are cached by
            // SystemPreferences and refreshed only from `changed::` — reading
            // GSettings and re-deriving the palette on every repaint (what this
            // used to do) is pure waste on the compositor's main loop.
            this._prefs = new SystemPreferences({
                onAccentChanged: () => this.queue_repaint(),
                onMotionChanged: () => this.queue_repaint(),
            });
            this._prefs.enable();

            // The frame driver. Binding the timeline to this actor makes it tick
            // on the actor's own frame clock: exactly one callback per presented
            // frame, vsynced, and automatically idle while the actor is unmapped
            // — unlike the GLib timeout this replaces, which ran at a fixed
            // 24 Hz out of phase with the display and at a GLib priority that
            // preempted Clutter's own redraw.
            this._frameTimeline = new Clutter.Timeline({
                actor: this,
                duration: FRAME_TIMELINE_MS,
                repeat_count: -1,
            });
            this._frameTimeline.connect('new-frame', () => this.queue_repaint());
            // A timeline can only tick once its actor actually has a frame clock,
            // which it only has while mapped. Gate on `mapped` rather than
            // assuming show() has already taken effect, so the driver survives
            // the actor being unmapped and remapped (monitor changes, the
            // overview, a Shell restart) without anyone having to remember to
            // restart it.
            this._animating = false;
            this.connect('notify::mapped', () => this._syncFrameTimeline());

            this.reset();
            this.connect('repaint', () => this._draw());
            this.connect('destroy', () => this._onDestroy());
        }

        /**
         * Restart the ribbon for a fresh session: back to the brief `unfold`
         * phase (FR-010a), with the smoothed envelope and frame clocks cleared
         * so a new session never inherits the tail of the previous one. Called
         * by HudView when the pill goes from hidden to shown — the actor itself
         * is no longer rebuilt per session (see this file's header).
         */
        reset() {
            const now = GLib.get_monotonic_time();
            this._startedAt = now;
            this._phase = 'unfold';
            this._phaseStartedAt = now;
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

        /** Stop driving repaints (hidden HUD ⇒ zero per-frame cost). Idempotent. */
        stopAnimation() {
            this._animating = false;
            this._syncFrameTimeline();
        }

        _syncFrameTimeline() {
            if (this._frameTimeline === null)
                return;
            const shouldRun = this._animating && this.mapped;
            if (shouldRun && !this._frameTimeline.is_playing())
                this._frameTimeline.start();
            else if (!shouldRun && this._frameTimeline.is_playing())
                this._frameTimeline.stop();
        }

        setLevel(rms, peak = rms) {
            this._lastRms = rms;
            this._lastPeak = peak;
            this._lastLevelAt = GLib.get_monotonic_time();
            // No queue_repaint() here: the frame timeline already repaints once
            // per frame while visible, and levels arrive at ~20 Hz (two
            // PropertiesChanged signals per pump tick), so asking for extra
            // repaints only adds work that the frame clock would coalesce away.
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
            const [w, h] = this.get_surface_size();
            if (w <= 0 || h <= 0)
                return;

            const now = GLib.get_monotonic_time();

            // unfold → flow advances on the frame clock rather than on its own
            // GLib timer: one less source to own, cancel and leak, and the
            // hand-off lands on a real frame boundary instead of between two.
            if (this._phase === 'unfold' &&
                (now - this._phaseStartedAt) / 1000 >= UNFOLD_MS) {
                this._phase = 'flow';
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
            paintRibbon(cr, w, h, model, this._prefs.accentPalette);
            cr.$dispose();
        }

        _onDestroy() {
            if (this._frameTimeline !== null) {
                this._frameTimeline.stop();
                this._frameTimeline.set_actor(null);
                this._frameTimeline = null;
            }
            this._prefs.disable();
        }
    });

/** HudView — implements the view.js IndicatorView interface. */
export class HudView {
    constructor() {
        this._container = null;
        this._box = null;
        this._icon = null;
        this._label = null;
        this._ribbon = null;
        this._dismissButton = null;
        this._holdTimer = 0;
        this._lastRms = 0;
        this._lastPeak = 0;
        this._shown = false;
        this._unredirectDisabled = false;
        this._colorClass = null;
        // The single held-notice slot (R15): {severity, statusText} or null.
        this._held = null;
    }

    show(descriptor) {
        this._ensureActor();
        this._appear();
        this._applyDescriptor(descriptor);
    }

    setLevel(rms, peak) {
        // D-Bus levels may arrive before the State transition presents the
        // HUD. Cache them so the first rendered frame is live instead of
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
            this._cancelHoldTimer();
        }

        // Every assignment below is guarded on an actual change. Writing an
        // identical icon name, label or style class still invalidates St's
        // cached theme node and queues a relayout/repaint of the pill, and
        // these run on every state emission.
        const iconName = iconForSeverity(severity);
        if (this._icon.icon_name !== iconName)
            this._icon.icon_name = iconName;
        if (this._label.text !== statusText) {
            this._label.text = statusText;
            this._box.set_accessible_name(
                statusText ? `Dictation: ${statusText}` : 'Dictation');
        }

        // Colour-code severity (orange recoverable, red critical) and the
        // cold-load phase (warm tint) so the treatment reads at a glance, not
        // just from the label text (2026-07-30 manual-test follow-up).
        const colorClass = pillColorClass(descriptor);
        if (colorClass !== this._colorClass) {
            for (const cls of PILL_COLOR_CLASSES)
                this._box.remove_style_class_name(cls);
            if (colorClass !== null)
                this._box.add_style_class_name(colorClass);
            this._colorClass = colorClass;
        }

        // 2026-07-30, R17a: only a critical error hides the ribbon; a
        // recoverable notice keeps it visible, tinted amber and gently
        // pulsing instead (hud-logic.js's `ribbonVisibleForSeverity`).
        const ribbonVisible = ribbonVisibleForSeverity(severity);
        if (this._ribbon.visible !== ribbonVisible)
            this._ribbon.visible = ribbonVisible;
        this._ribbon.setSeverityTint(severity);

        // 2026-07-30, R17: force the ribbon into `morph`/`complete` for the
        // two transitions that must visibly change its motion; every other
        // key (recording/loading/...) leaves the ribbon's own internal
        // unfold→flow phase alone (hud-logic.js's `ribbonPhaseForStateKey`).
        const forcedPhase = ribbonPhaseForStateKey(descriptor.key);
        if (forcedPhase !== null)
            this._ribbon.setPhase(forcedPhase);

        const dismissVisible = severity === 'critical';
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
        // FR-007c/X22: pointer-reactive but never focusable — this handler
        // only ever fires from a mouse click, never a keyboard event.
        this._cancelHoldTimer();
        this._held = null;
        this._dismiss();
    }

    // Build the actor tree. Runs at most ONCE per enable(): the pill is
    // reused for every session rather than rebuilt, so presenting it costs a
    // fade and nothing else (see this file's header, rule 1).
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
            visible: false,
            // Bottom-centre placement inside the constrained container
            // below; the gap from the work area's bottom edge is
            // stylesheet.css's `margin-bottom` on `.myna-hud-pill` (St
            // implements CSS margins, and would overwrite anything we set
            // on the actor by hand the next time the style resolves).
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

        // A non-reactive container spanning the primary monitor's WORK AREA
        // (so the pill sits above a bottom dock/panel rather than under it),
        // with the pill bottom-centred inside it. This is osdWindow.js's
        // pattern, and it replaces the old hand-computed `set_position` that
        // had to guess the pill's height — and the `monitors-changed`
        // handler, which MonitorConstraint owns now.
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

    // Present the pill. A no-op when it is already on screen, so a burst of
    // state changes never restarts the entrance animation, and a show()
    // landing mid-fade-out reverses that fade on the SAME actor instead of
    // building a second pill over it.
    _appear() {
        if (this._shown || this._box === null)
            return;
        this._shown = true;

        // Over a fullscreen window mutter may scan the window out directly;
        // an overlay appearing forces it in and out of that path, which
        // flickers. osdWindow.js guards its own OSD exactly this way.
        this._setUnredirectDisabled(true);

        this._ribbon.reset();
        this._ribbon.setLevel(this._lastRms, this._lastPeak);

        this._box.remove_all_transitions();
        this._box.show();
        this._ribbon.startAnimation();
        this._box.set_scale(0.9, 0.9);
        this._box.ease({
            opacity: 255,
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
                // A show() during the fade flips _shown back to true and
                // reverses this transition; onComplete then belongs to the
                // *new* animation, so re-check before hiding for real.
                // destroy() can also have run in the meantime.
                if (this._shown || this._box === null)
                    return;
                this._box.hide();
                this._ribbon.stopAnimation();
                this._setUnredirectDisabled(false);
            },
        });
    }

    // `disable_unredirect`/`enable_unredirect` are reference-counted in
    // mutter, so they must balance exactly — including across destroy() with
    // a fade still in flight.
    _setUnredirectDisabled(disabled) {
        if (disabled === this._unredirectDisabled)
            return;
        this._unredirectDisabled = disabled;
        if (disabled)
            global.compositor.disable_unredirect();
        else
            global.compositor.enable_unredirect();
    }

    _stopTimers() {
        this._cancelHoldTimer();
    }
}
