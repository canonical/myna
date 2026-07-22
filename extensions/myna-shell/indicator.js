// indicator.js — RibbonView: the EXPERIMENTAL presentation (feature 004; one
// implementation of the view.js IndicatorView seam). Everything here is
// deliberately behind that interface so the team can redesign it, or a future
// user theme can replace it, without touching the contract / proxy / states /
// level pump. This is @cdunn's first-pass vision, not a settled design.
//
// A wide ribbon hanging under the top bar: a row of VU bars driven by the live
// audio level, a content-free status label, a per-state colour/animation, and
// errors held visible with their reason before clearing (so an error never
// just vanishes). Added as Shell chrome — non-reactive, non-focusable — so it
// can never take keyboard focus (X11/SC-001).
//
// All the knobs are here at the top: tweak freely, reload, iterate.

import Atk from 'gi://Atk';
import Clutter from 'gi://Clutter';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';

import {levelToBars} from './vumeter.js';

// ── Tunables ────────────────────────────────────────────────────────────────
const WIDTH_FRACTION = 0.8;      // ribbon spans ~80% of the monitor width
const HEIGHT = 56;               // ribbon height (px)
const BAR_COUNT = 48;            // VU bars across the ribbon
const BAR_GAP = 3;               // px between bars
const APPEAR_MS = 180;
const CLEAR_MS = 220;
const VU_FPS = 30;               // VU repaint cadence
const ERROR_HOLD_MS = 3500;      // keep an error visible this long before clearing

// Per-state palette (RGB 0–255) + whether the state pulses. Colours chosen for
// legible, conventional activity cues; edit here to reskin.
const TREATMENTS = {
    loading:      {rgb: [229, 165, 10],  pulse: true},   // amber — warming up
    recording:    {rgb: [45, 194, 130],  pulse: false},  // green — listening (VU carries life)
    transcribing: {rgb: [53, 132, 224],  pulse: false},  // blue — thinking
    finalizing:   {rgb: [38, 162, 105],  pulse: true},   // deep green — confirming
    error:        {rgb: [224, 27, 36],   pulse: true},   // red — problem
    active:       {rgb: [143, 148, 168], pulse: true},   // neutral — unknown state
};

const RibbonActor = GObject.registerClass(
class RibbonActor extends St.DrawingArea {
    _init() {
        super._init({
            style_class: 'myna-ribbon',
            reactive: false,
            can_focus: false,
            height: HEIGHT,
            opacity: 0,
        });
        this._rgb = TREATMENTS.active.rgb;
        this._bars = new Array(BAR_COUNT).fill(0.06);
        this.set_accessible_role(Atk.Role.STATUSBAR);
        this.connect('repaint', () => this._draw());
    }

    setColor(rgb) {
        this._rgb = rgb;
        this.queue_repaint();
    }

    setBars(bars) {
        this._bars = bars;
        this.queue_repaint();
    }

    _draw() {
        const cr = this.get_context();
        const [w, h] = this.get_surface_size();
        const n = this._bars.length;
        const slot = w / n;
        const barW = Math.max(1, slot - BAR_GAP);
        const [r, g, b] = this._rgb;
        cr.setSourceRGBA(r / 255, g / 255, b / 255, 0.95);
        for (let i = 0; i < n; i++) {
            const bh = Math.max(2, this._bars[i] * (h - 6));
            const x = i * slot + (slot - barW) / 2;
            const y = (h - bh) / 2;
            // Rounded-ish bar via a filled rect (cheap; good enough for a VU).
            cr.rectangle(x, y, barW, bh);
        }
        cr.fill();
        cr.$dispose();
    }
});

/** RibbonView — implements the view.js IndicatorView interface. */
export class RibbonView {
    constructor() {
        this._actor = null;
        this._label = null;
        this._box = null;
        this._monitorsChangedId = 0;
        this._vuTimer = 0;
        this._errorHoldTimer = 0;
        this._pulseHandle = null;
        this._lastLevel = 0;
        this._holdingError = false;
    }

    show(descriptor) {
        this._ensureActor();
        // An error is held visible for a beat even if a hide() races in.
        if (this._errorHoldTimer !== 0) {
            GLib.source_remove(this._errorHoldTimer);
            this._errorHoldTimer = 0;
        }
        this._holdingError = !!descriptor.isError;

        const treatment = TREATMENTS[descriptor.key] ?? TREATMENTS.active;
        this._actor.setColor(treatment.rgb);
        this._label.text = descriptor.statusText;
        this._actor.set_accessible_name(
            descriptor.statusText ? `Dictation: ${descriptor.statusText}` : 'Dictation');

        this._setPulsing(treatment.pulse);
    }

    setLevel(rms, _peak) {
        this._lastLevel = rms;
        this._lastLevelAt = GLib.get_monotonic_time();
    }

    hide() {
        // Errors linger with their reason instead of vanishing (view policy).
        if (this._holdingError && this._actor !== null) {
            if (this._errorHoldTimer === 0) {
                this._errorHoldTimer = GLib.timeout_add(
                    GLib.PRIORITY_DEFAULT, ERROR_HOLD_MS, () => {
                        this._errorHoldTimer = 0;
                        this._holdingError = false;
                        this._dismiss();
                        return GLib.SOURCE_REMOVE;
                    });
            }
            return;
        }
        this._dismiss();
    }

    destroy() {
        this._stopTimers();
        if (this._actor !== null)
            this._actor.remove_all_transitions();
        if (this._box !== null) {
            Main.layoutManager.removeChrome(this._box);
            this._box.destroy();
        }
        this._detach();
    }

    // ── internals ────────────────────────────────────────────────────────────

    _ensureActor() {
        if (this._actor !== null)
            return;
        this._actor = new RibbonActor();
        this._label = new St.Label({
            style_class: 'myna-ribbon-label',
            text: '',
            x_align: Clutter.ActorAlign.CENTER,
        });
        this._box = new St.BoxLayout({
            style_class: 'myna-ribbon-box',
            orientation: Clutter.Orientation.VERTICAL,
            reactive: false,
            can_focus: false,
        });
        this._box.add_child(this._actor);
        this._box.add_child(this._label);

        Main.layoutManager.addTopChrome(this._box);
        this._monitorsChangedId = Main.layoutManager.connect(
            'monitors-changed', () => this._position());
        this._position();

        this._box.set_pivot_point(0.5, 0);
        this._box.set_scale(1.0, 0.6);
        this._box.ease({
            opacity: 255,
            scale_x: 1.0,
            scale_y: 1.0,
            duration: APPEAR_MS,
            mode: Clutter.AnimationMode.EASE_OUT_CUBIC,
        });

        this._startVu();
    }

    _startVu() {
        if (this._vuTimer !== 0)
            return;
        this._vuTimer = GLib.timeout_add(
            GLib.PRIORITY_DEFAULT, Math.floor(1000 / VU_FPS), () => {
                if (this._actor === null)
                    return GLib.SOURCE_REMOVE;
                const ageMs = this._lastLevelAt
                    ? (GLib.get_monotonic_time() - this._lastLevelAt) / 1000
                    : 9999;
                this._actor.setBars(levelToBars(this._lastLevel, ageMs, BAR_COUNT));
                return GLib.SOURCE_CONTINUE;
            });
    }

    _setPulsing(on) {
        if (this._actor === null)
            return;
        this._actor.remove_all_transitions();
        if (!on) {
            this._actor.opacity = 255;
            return;
        }
        // A slow breathing pulse via a looping opacity transition.
        this._actor.opacity = 255;
        this._actor.ease({
            opacity: 140,
            duration: 900,
            mode: Clutter.AnimationMode.EASE_IN_OUT_SINE,
            autoReverse: true,
            repeatCount: -1,
        });
    }

    _dismiss() {
        const box = this._box;
        if (box === null)
            return;
        this._stopTimers();
        this._detachSignals();
        const actor = this._actor;
        this._actor = null;
        this._box = null;
        this._label = null;
        if (actor !== null)
            actor.remove_all_transitions();
        box.ease({
            opacity: 0,
            scale_y: 0.6,
            duration: CLEAR_MS,
            mode: Clutter.AnimationMode.EASE_IN_CUBIC,
            onComplete: () => {
                Main.layoutManager.removeChrome(box);
                box.destroy();
            },
        });
    }

    _position() {
        if (this._box === null)
            return;
        const monitor = Main.layoutManager.primaryMonitor;
        const width = Math.round(monitor.width * WIDTH_FRACTION);
        this._actor.width = width;
        this._box.set_position(
            monitor.x + Math.round((monitor.width - width) / 2),
            monitor.y + Main.panel.height);
    }

    _stopTimers() {
        if (this._vuTimer !== 0) {
            GLib.source_remove(this._vuTimer);
            this._vuTimer = 0;
        }
        if (this._errorHoldTimer !== 0) {
            GLib.source_remove(this._errorHoldTimer);
            this._errorHoldTimer = 0;
        }
    }

    _detachSignals() {
        if (this._monitorsChangedId !== 0) {
            Main.layoutManager.disconnect(this._monitorsChangedId);
            this._monitorsChangedId = 0;
        }
    }

    _detach() {
        this._detachSignals();
        this._actor = null;
        this._box = null;
        this._label = null;
    }
}
