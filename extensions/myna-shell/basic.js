import Atk from 'gi://Atk';
import Clutter from 'gi://Clutter';
import GLib from 'gi://GLib';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';

import {SystemPreferences} from './accent.js';
import {basicTargetFill, smoothBasicFill} from './basic-logic.js';
import {
    computePosition,
    iconForSeverity,
    pillColorClass,
    PILL_COLOR_CLASSES,
} from './hud-logic.js';

const PILL_WIDTH = 360;
const PILL_HEIGHT_ESTIMATE = 88;
const BAR_WIDTH = 240;
const FRAME_MS = 1000 / 30;
const APPEAR_MS = 180;
const CLEAR_MS = 200;

export class BasicHudView {
    constructor({onDismiss = null} = {}) {
        this._onDismiss = onDismiss ?? (() => {});
        this._box = null;
        this._icon = null;
        this._label = null;
        this._barFill = null;
        this._dismissButton = null;
        this._monitorSignal = 0;
        this._frameTimer = 0;
        this._stateKey = 'idle';
        this._rms = 0;
        this._peak = 0;
        this._receivedAt = 0;
        this._displayFill = 0;
        this._lastFrameAt = 0;
        this._retiringBoxes = new Set();
    }

    show(descriptor) {
        this._ensureActor();
        this._stateKey = descriptor.key;
        this._icon.icon_name = iconForSeverity(descriptor.severity);
        this._label.text = descriptor.statusText;
        this._box.set_accessible_name(
            descriptor.statusText ? `Dictation: ${descriptor.statusText}` : 'Dictation');
        for (const cls of PILL_COLOR_CLASSES)
            this._box.remove_style_class_name(cls);
        const colorClass = pillColorClass(descriptor);
        if (colorClass !== null)
            this._box.add_style_class_name(colorClass);
        this._dismissButton.visible = descriptor.severity === 'critical';
        this._startFrameTimer();
    }

    setLevel(rms, peak, receivedAt = GLib.get_monotonic_time()) {
        this._rms = rms;
        this._peak = peak;
        this._receivedAt = receivedAt;
        this._startFrameTimer();
    }

    hide() {
        this._dismiss();
    }

    destroy() {
        this._stopFrameTimer();
        this._detachMonitorSignal();
        if (this._box !== null) {
            Main.layoutManager.removeChrome(this._box);
            this._box.destroy();
        }
        for (const box of this._retiringBoxes) {
            box.remove_all_transitions();
            Main.layoutManager.removeChrome(box);
            box.destroy();
        }
        this._retiringBoxes.clear();
        this._clearActors();
        this._retiringBoxes.add(box);
    }

    _ensureActor() {
        if (this._box !== null)
            return;
        this._prefs = new SystemPreferences({
            onMotionChanged: () => this._startFrameTimer(),
        });
        this._prefs.enable();
        this._icon = new St.Icon({
            style_class: 'myna-hud-icon',
            icon_name: 'audio-input-microphone-symbolic',
            icon_size: 28,
        });
        this._label = new St.Label({
            style_class: 'myna-hud-label myna-basic-label',
            text: '',
            y_align: Clutter.ActorAlign.CENTER,
        });
        const barTrack = new St.Widget({
            style_class: 'myna-basic-track',
            reactive: false,
            can_focus: false,
            width: BAR_WIDTH,
        });
        this._barFill = new St.Widget({
            style_class: 'myna-basic-fill',
            reactive: false,
            can_focus: false,
            width: 0,
        });
        barTrack.add_child(this._barFill);
        const content = new St.BoxLayout({
            style_class: 'myna-hud-content myna-basic-content',
            orientation: Clutter.Orientation.VERTICAL,
            x_expand: true,
        });
        content.add_child(this._label);
        content.add_child(barTrack);
        this._dismissButton = new St.Icon({
            style_class: 'myna-hud-dismiss',
            icon_name: 'window-close-symbolic',
            icon_size: 16,
            reactive: true,
            can_focus: false,
            visible: false,
        });
        this._dismissButton.connect('button-press-event', () => {
            this._onDismiss();
            return Clutter.EVENT_STOP;
        });
        this._box = new St.BoxLayout({
            style_class: 'myna-hud-pill myna-basic-pill',
            orientation: Clutter.Orientation.HORIZONTAL,
            reactive: false,
            can_focus: false,
            width: PILL_WIDTH,
            opacity: 0,
        });
        this._box.set_accessible_role(Atk.Role.STATUSBAR);
        this._box.add_child(this._icon);
        this._box.add_child(content);
        this._box.add_child(this._dismissButton);
        Main.layoutManager.addChrome(this._box);
        this._monitorSignal = Main.layoutManager.connect(
            'monitors-changed', () => this._position());
        this._position();
        this._box.set_pivot_point(0.5, 0.5);
        this._box.set_scale(0.9, 0.9);
        this._box.ease({
            opacity: 255,
            scale_x: 1,
            scale_y: 1,
            duration: APPEAR_MS,
            mode: Clutter.AnimationMode.EASE_OUT_QUAD,
        });
    }

    _startFrameTimer() {
        if (this._box === null || this._frameTimer !== 0)
            return;
        this._frameTimer = GLib.timeout_add(
            GLib.PRIORITY_DEFAULT, Math.floor(FRAME_MS), () => this._frame());
    }

    _frame() {
        if (this._box === null || this._barFill === null) {
            this._frameTimer = 0;
            return GLib.SOURCE_REMOVE;
        }
        const now = GLib.get_monotonic_time();
        const ageMs = this._receivedAt ? (now - this._receivedAt) / 1000 : 9999;
        const dtMs = this._lastFrameAt ? (now - this._lastFrameAt) / 1000 : FRAME_MS;
        this._lastFrameAt = now;
        const target = basicTargetFill(this._stateKey, this._rms, this._peak, ageMs);
        this._displayFill = smoothBasicFill(
            this._displayFill, target, dtMs, this._prefs?.reducedMotion ?? false);
        this._barFill.set_width(Math.round(BAR_WIDTH * this._displayFill));
        if (target === 0 && this._displayFill < 0.002) {
            this._displayFill = 0;
            this._barFill.set_width(0);
            this._frameTimer = 0;
            return GLib.SOURCE_REMOVE;
        }
        return GLib.SOURCE_CONTINUE;
    }

    _position() {
        if (this._box === null)
            return;
        const position = computePosition(
            Main.layoutManager.primaryMonitor, PILL_WIDTH, PILL_HEIGHT_ESTIMATE);
        this._box.set_position(position.x, position.y);
    }

    _dismiss() {
        const box = this._box;
        if (box === null)
            return;
        this._stopFrameTimer();
        this._detachMonitorSignal();
        this._clearActors();
        box.remove_all_transitions();
        box.ease({
            opacity: 0,
            duration: CLEAR_MS,
            mode: Clutter.AnimationMode.EASE_IN_OUT_QUAD,
            onComplete: () => {
                this._retiringBoxes.delete(box);
                Main.layoutManager.removeChrome(box);
                box.destroy();
            },
        });
    }

    _stopFrameTimer() {
        if (this._frameTimer !== 0) {
            GLib.source_remove(this._frameTimer);
            this._frameTimer = 0;
        }
    }

    _detachMonitorSignal() {
        if (this._monitorSignal !== 0) {
            Main.layoutManager.disconnect(this._monitorSignal);
            this._monitorSignal = 0;
        }
    }

    _clearActors() {
        this._prefs?.disable();
        this._prefs = null;
        this._box = null;
        this._icon = null;
        this._label = null;
        this._barFill = null;
        this._dismissButton = null;
    }
}
