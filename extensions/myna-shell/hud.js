// hud.js — HudView: the bottom-center HUD pill (feature 004-gnome-shell-
// indicator, 2026-07-30 HUD redesign; one implementation of the view.js
// IndicatorView seam, replacing the prior RibbonView/`indicator.js`, which is
// deleted, not kept as a selectable alternate — spec Assumptions).
//
// A compact pill styled after GNOME's own volume/brightness OSD: bottom-center
// of the primary monitor (R14), a mic/mic-slash icon (contextual on severity,
// X19), a content-free status label, a segmented bar meter for the live audio
// level (R16 — shown only for the non-problem states; the reference design
// doesn't draw one alongside a notice/error row), and — for a critical error
// only — a dismiss (×) control that is pointer-reactive but never
// keyboard-focusable (X22, FR-007c), so a click can never steal keyboard
// focus (X11/SC-001).
//
// The "held notice" slot (recoverable vs. critical) implements the
// replace-in-place / restart-timer rules from research R15 (FR-007a/FR-007d,
// X20): any new problem descriptor replaces whatever is currently held
// (never a queue); a recoverable notice's hold timer restarts in full on a
// repeat; a critical error has no timer and never auto-dismisses.
//
// Added as Shell chrome — non-reactive, non-focusable — except the dismiss
// control, so nothing here can ever take keyboard focus. Pixels/geometry are
// deliberately isolated behind this one file (and its pure `hud-logic.js`
// helper); states.js/view.js/dbus.js/vumeter.js are untouched by this
// redesign.

import Atk from 'gi://Atk';
import Cairo from 'gi://cairo';
import Clutter from 'gi://Clutter';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';

import {
    levelsToIntensity,
    intensityToActiveSegments,
    segmentColor,
} from './vumeter.js';
import {
    computePosition,
    iconForSeverity,
    severityAutoDismisses,
    shouldReplaceHeldNotice,
    pillColorClass,
    PILL_COLOR_CLASSES,
} from './hud-logic.js';

// ── Tunables ────────────────────────────────────────────────────────────────
const PILL_WIDTH = 360;
// An ESTIMATE of the pill's natural height, used only for bottom-margin
// positioning (computePosition) — the actor's real height is left to size
// naturally from its children (see the fix note on `_box` below); forcing an
// explicit `height` here previously squeezed the bar meter's allocation down
// to near-zero regardless of the real audio level, the flat-vumeter bug a
// manual test caught (2026-07-30 follow-up).
const PILL_HEIGHT_ESTIMATE = 88;
const BAR_COUNT = 24;
const BAR_METER_WIDTH = 160;
const BAR_METER_HEIGHT = 32;
const VU_FPS = 30;
const APPEAR_MS = 180;
const CLEAR_MS = 200;
const RECOVERABLE_HOLD_MS = 3500; // matches the prior ERROR_HOLD_MS baseline

// A Cairo-drawn segmented VU meter (R16) — replaces the prior continuous
// goop/ribbon glow entirely. Power mapping, dBFS calibration, active-segment
// count, and green/yellow/red zones live in vumeter.js; this actor only draws.
const BarMeterActor = GObject.registerClass(
class BarMeterActor extends St.DrawingArea {
    _init() {
        super._init({
            style_class: 'myna-hud-bars',
            reactive: false,
            can_focus: false,
            width: BAR_METER_WIDTH,
            height: BAR_METER_HEIGHT,
            x_expand: false,
            y_expand: false,
        });
        this._lastRms = 0;
        this._lastPeak = 0;
        this._lastLevelAt = 0;
        this.connect('repaint', () => this._draw());
    }

    setLevel(rms, peak = rms) {
        this._lastRms = rms;
        this._lastPeak = peak;
        this._lastLevelAt = GLib.get_monotonic_time();
        this.queue_repaint();
    }

    _draw() {
        const cr = this.get_context();
        const [w, h] = this.get_surface_size();
        const now = GLib.get_monotonic_time();
        const ageMs = this._lastLevelAt ? (now - this._lastLevelAt) / 1000 : 9999;
        const intensity = levelsToIntensity(
            this._lastRms, this._lastPeak, ageMs);
        const active = intensityToActiveSegments(intensity, BAR_COUNT);

        const gap = w / BAR_COUNT;
        const barWidth = gap * 0.55;
        for (let i = 0; i < BAR_COUNT; i++) {
            const position = (i + 1) / BAR_COUNT;
            const lit = i < active;
            switch (segmentColor(position)) {
            case 'red':
                cr.setSourceRGBA(0.95, 0.24, 0.20, lit ? 1.0 : 0.16);
                break;
            case 'yellow':
                cr.setSourceRGBA(0.98, 0.72, 0.18, lit ? 1.0 : 0.16);
                break;
            case 'green':
            default:
                cr.setSourceRGBA(0.20, 0.82, 0.42, lit ? 1.0 : 0.16);
                break;
            }
            // Conventional VU: fixed-height segments illuminate left-to-right
            // as signal power rises. Slight taper keeps the row visually alive
            // without making amplitude ambiguous.
            const barH = h * (0.66 + 0.34 * position);
            const x = i * gap + (gap - barWidth) / 2;
            const y = (h - barH) / 2;
            cr.rectangle(x, y, barWidth, barH);
            cr.fill();
        }
        cr.$dispose();
    }
});

/** HudView — implements the view.js IndicatorView interface. */
export class HudView {
    constructor() {
        this._box = null;
        this._icon = null;
        this._label = null;
        this._bars = null;
        this._dismissButton = null;
        this._monitorsChangedId = 0;
        this._vuTimer = 0;
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
        this._bars?.setLevel(rms, peak);
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
        this._bars = null;
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
            this._held = null;
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

        // The bar meter only makes sense for the non-problem states (the
        // reference design doesn't draw one alongside a notice/error row).
        this._bars.visible = severity === null;

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
        this._bars = new BarMeterActor();
        this._bars.setLevel(this._lastRms, this._lastPeak);
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
        });
        contentBox.add_child(this._label);
        contentBox.add_child(this._bars);

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

        this._startVu();
    }

    _position() {
        if (this._box === null)
            return;
        const monitor = Main.layoutManager.primaryMonitor;
        const {x, y} = computePosition(monitor, PILL_WIDTH, PILL_HEIGHT_ESTIMATE);
        this._box.set_position(x, y);
    }

    _startVu() {
        if (this._vuTimer !== 0)
            return;
        this._vuTimer = GLib.timeout_add(
            GLib.PRIORITY_DEFAULT, Math.floor(1000 / VU_FPS), () => {
                if (this._bars === null)
                    return GLib.SOURCE_REMOVE;
                this._bars.queue_repaint();
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
        this._bars = null;
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
        if (this._vuTimer !== 0) {
            GLib.source_remove(this._vuTimer);
            this._vuTimer = 0;
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
