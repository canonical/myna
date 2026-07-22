// indicator.js — RibbonView: the EXPERIMENTAL presentation (feature 004; one
// implementation of the view.js IndicatorView seam). Everything here is
// deliberately behind that interface so the team can redesign it, or a future
// user theme can replace it, without touching the contract / proxy / states /
// level pump. This is @cdunn's first-pass vision, not a settled design.
//
// A wide ribbon hanging under the top bar rendering a flowing "goop" blob: a
// mirrored, gradient-filled organic band that swells and jiggles with the live
// audio level (voice activity), plus a content-free status label, a per-state
// colour, and errors held visible with their reason before clearing (so an
// error never just vanishes). Added as Shell chrome — non-reactive,
// non-focusable — so it can never take keyboard focus (X11/SC-001).
//
// This is deliberately NOT an accurate VU meter: it's an expressive activity
// cue (Gemini-style liquid UI). The energy still comes from the real level via
// vumeter.js (levelToIntensity); the goop layers travelling waves + smoothing
// on top for life. All the knobs are here at the top: tweak, reload, iterate.

import Atk from 'gi://Atk';
import Cairo from 'gi://cairo';
import Clutter from 'gi://Clutter';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';

import {levelToIntensity, FLOOR} from './vumeter.js';

// ── Tunables ────────────────────────────────────────────────────────────────
const WIDTH_FRACTION = 0.8;      // ribbon spans ~80% of the monitor width
const HEIGHT = 84;               // ribbon height (px) — room for the goop to swell
const SAMPLES = 120;             // x-resolution of the goop envelope
const APPEAR_MS = 200;
const CLEAR_MS = 240;
const VU_FPS = 60;               // repaint cadence — smooth flowing goop
const ERROR_HOLD_MS = 3500;      // keep an error visible this long before clearing

// Envelope shaping. The goop is a mirrored blob spanning the ribbon: a base
// thickness (always alive) that swells with voice, its edge riding a sum of
// drifting sine waves whose amplitude tracks the level — so loud speech reads
// as a big, organic jiggle and silence as a calm, gently breathing band.
const BASE_THICKNESS = 0.16;     // fraction of half-height when quiet
const SWELL = 0.78;              // extra fraction at full intensity
const WOBBLE = 0.60;             // how much the travelling waves distort the edge
// Intensity smoothing: attack fast (feel responsive), release slower (goop
// settles rather than snapping) — exponential per-frame blend factors.
const ATTACK = 0.55;
const RELEASE = 0.10;

// "Working" motion (loading/transcribing): there is no mic audio in these
// phases, so the goop drives itself — a steady breathing swell that never dies
// (so it reads as "busy, please wait", not "off") plus a highlight that sweeps
// across the ribbon like an indeterminate progress scan.
const SWEEP_PERIOD = 1.5;        // seconds for the scan to cross the ribbon
const SWEEP_SIGMA = 0.11;        // width of the scanning bulge (fraction of width)
const SWEEP_GAIN = 0.5;          // how much the scan swells the local edge
const WORK_BASE = 0.52;          // baseline intensity while working
const WORK_BREATH = 0.20;        // breathing depth of that baseline
const WORK_BREATH_HZ = 0.55;     // breaths per second

// Per-state palette + motion. `rgb` is the core colour, `accent` a second hue
// the gradient flows toward (creative colour, Gemini-ish). `mode` picks the
// animation: 'audio' (mic-driven VU), 'working' (self-driven "please wait"
// scan), 'confirm' (one-shot "done" flourish), 'still' (calm hold). `pulse` is
// a slow opacity breath layered on top. Edit here to reskin — pixels live
// behind view.js.
const TREATMENTS = {
    loading:      {rgb: [255, 184, 46],  accent: [255, 120, 30],  mode: 'working', pulse: false}, // amber→orange — warming up
    recording:    {rgb: [60, 220, 150],  accent: [40, 170, 235],  mode: 'audio',   pulse: false}, // green→cyan — listening
    transcribing: {rgb: [90, 150, 245],  accent: [170, 110, 240], mode: 'working', pulse: false}, // blue→violet — thinking, wait
    finalizing:   {rgb: [80, 230, 150],  accent: [140, 245, 200], mode: 'confirm', pulse: false}, // bright green — done!
    error:        {rgb: [240, 60, 70],   accent: [255, 130, 90],  mode: 'still',   pulse: true},  // red→coral — problem
    active:       {rgb: [160, 168, 190], accent: [120, 130, 170], mode: 'working', pulse: false}, // neutral — unknown
};

// Travelling-wave components (freq in Hz over the ribbon width, temporal drift
// in rad/s, relative weight). A small incommensurate set gives a non-repeating,
// liquid edge without any randomness (deterministic, cheap).
const WAVES = [
    {k: 1.5, w: 1.1, amp: 0.55, phase: 0.0},
    {k: 2.7, w: -1.7, amp: 0.28, phase: 1.3},
    {k: 4.3, w: 2.3, amp: 0.17, phase: 2.9},
];

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
        this._accent = TREATMENTS.active.accent;
        this._mode = 'audio';      // motion mode (see TREATMENTS)
        this._intensity = FLOOR;   // smoothed drive level
        this._phase = 0;           // animation clock (seconds)
        this._modeClock = 0;       // seconds since the current mode began
        this.set_accessible_role(Atk.Role.STATUSBAR);
        this.connect('repaint', () => this._draw());
    }

    setColor(rgb, accent) {
        this._rgb = rgb;
        this._accent = accent;
        this.queue_repaint();
    }

    // Switch motion mode, resetting the per-mode clock so 'confirm' replays its
    // one-shot flourish each time we (re)enter finalizing.
    setMode(mode) {
        this._mode = mode;
        this._modeClock = 0;
    }

    // Advance the goop one frame. `audioTarget` is the fresh mic-driven
    // intensity in [FLOOR,1] (used only in 'audio' mode); `dt` is elapsed
    // seconds. Non-audio modes synthesise their own drive so the goop stays
    // alive with no microphone (the whole point of the "please wait" cue).
    tick(audioTarget, dt) {
        this._phase += dt;
        this._modeClock += dt;
        const target = this._driveFor(audioTarget);
        const blend = target > this._intensity ? ATTACK : RELEASE;
        this._intensity += (target - this._intensity) * blend;
        this.queue_repaint();
    }

    // The target intensity this frame, by mode.
    _driveFor(audioTarget) {
        switch (this._mode) {
        case 'audio':
            return audioTarget;
        case 'working': {
            // Steady breathing swell — never dies, so it reads as "busy".
            const breath = Math.sin(2 * Math.PI * WORK_BREATH_HZ * this._modeClock);
            return WORK_BASE + WORK_BREATH * 0.5 * (1 + breath);
        }
        case 'confirm': {
            // One-shot "done" flourish: a quick bright swell that eases back to
            // a calm hold, so a sighted user clearly sees the transcript land.
            const t = this._modeClock;
            const pop = Math.exp(-3.2 * t) * Math.sin(2 * Math.PI * 1.4 * t);
            return 0.6 + 0.4 * Math.max(0, pop);
        }
        case 'still':
        default:
            return 0.45;
        }
    }

    _draw() {
        const cr = this.get_context();
        const [w, h] = this.get_surface_size();
        const mid = h / 2;
        const intensity = this._intensity;
        // Half-thickness of the band at each x, as a fraction of half-height.
        const maxHalf = mid - 3;
        const thick = BASE_THICKNESS + SWELL * intensity;

        // 'working' mode runs an indeterminate scan: a bright bulge that sweeps
        // left→right across the ribbon. `sweepU` is its centre in [0,1] (NaN in
        // other modes = no scan).
        const working = this._mode === 'working';
        const sweepU = working
            ? (this._modeClock % SWEEP_PERIOD) / SWEEP_PERIOD
            : NaN;

        // Build the top and bottom envelopes (mirrored, with a slight vertical
        // asymmetry so it reads as liquid rather than a symmetric ribbon).
        const top = new Array(SAMPLES + 1);
        const bot = new Array(SAMPLES + 1);
        for (let i = 0; i <= SAMPLES; i++) {
            const u = i / SAMPLES;            // 0..1 across the width
            // Spindle window: fat in the middle, tapering to the ends so the
            // blob has rounded caps instead of hard edges.
            const window = Math.sin(Math.PI * u) ** 0.7;
            let wobble = 0;
            for (const wv of WAVES) {
                wobble += wv.amp *
                    Math.sin(2 * Math.PI * wv.k * u + wv.w * this._phase + wv.phase);
            }
            // Wave distortion scales with intensity — calm when quiet.
            let edge = thick * window * (1 + WOBBLE * intensity * wobble);
            // Progress scan: a Gaussian bulge travelling under the sweep centre.
            if (working) {
                const d = u - sweepU;
                const bulge = Math.exp(-(d * d) / (2 * SWEEP_SIGMA * SWEEP_SIGMA));
                edge += SWEEP_GAIN * thick * window * bulge;
            }
            const half = Math.max(0, Math.min(1, edge)) * maxHalf;
            // Slight phase-shifted asymmetry between the two edges.
            const skew = 0.12 * maxHalf * intensity *
                Math.sin(2 * Math.PI * 0.7 * u + 0.6 * this._phase);
            top[i] = mid - half + skew;
            bot[i] = mid + half + skew;
        }

        // Vertical gradient flowing core → accent for depth (Gemini-ish).
        const [r0, g0, b0] = this._rgb;
        const [r1, g1, b1] = this._accent;
        this._fillGoop(cr, w, h, top, bot, r0, g0, b0, r1, g1, b1, intensity, sweepU);
        cr.$dispose();
    }

    _fillGoop(cr, w, h, top, bot, r0, g0, b0, r1, g1, b1, intensity, sweepU) {
        // Trace the closed blob: top edge left→right, bottom edge right→left.
        const x = i => (i / SAMPLES) * w;
        cr.newPath();
        cr.moveTo(x(0), top[0]);
        for (let i = 1; i <= SAMPLES; i++)
            cr.lineTo(x(i), top[i]);
        for (let i = SAMPLES; i >= 0; i--)
            cr.lineTo(x(i), bot[i]);
        cr.closePath();

        // A soft vertical gradient (core at top, accent at bottom) whose alpha
        // lifts with intensity — the goop glows brighter as you speak.
        const alpha = 0.55 + 0.4 * intensity;
        try {
            const grad = new Cairo.LinearGradient(0, 0, 0, h);
            grad.addColorStopRGBA(0.0, r0 / 255, g0 / 255, b0 / 255, alpha);
            grad.addColorStopRGBA(0.5, (r0 + r1) / 510, (g0 + g1) / 510,
                (b0 + b1) / 510, alpha);
            grad.addColorStopRGBA(1.0, r1 / 255, g1 / 255, b1 / 255, alpha);
            cr.setSource(grad);
        } catch (_e) {
            // Fallback: flat core colour if gradients are unavailable.
            cr.setSourceRGBA(r0 / 255, g0 / 255, b0 / 255, alpha);
        }
        // Keep the path for a scan clip below (fill would consume it).
        if (Number.isNaN(sweepU)) {
            cr.fill();
            return;
        }
        cr.fillPreserve();

        // 'working' scan: paint a soft white highlight travelling with the
        // sweep, clipped to the blob — an obvious "processing, please wait" cue.
        {
            cr.clip();
            const cx = sweepU * w;
            const half = Math.max(1, SWEEP_SIGMA * w * 2);
            const hl = new Cairo.LinearGradient(cx - half, 0, cx + half, 0);
            hl.addColorStopRGBA(0.0, 1, 1, 1, 0);
            hl.addColorStopRGBA(0.5, 1, 1, 1, 0.35);
            hl.addColorStopRGBA(1.0, 1, 1, 1, 0);
            cr.setSource(hl);
            cr.rectangle(0, 0, w, h);
            cr.fill();
            cr.resetClip();
        }
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
        this._mode = null;
        this._lastLevel = 0;
        this._lastLevelAt = 0;
        this._lastFrameAt = 0;
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
        this._actor.setColor(treatment.rgb, treatment.accent);
        // Only reset the motion clock on an actual mode change, so re-`show()`
        // of the same phase (e.g. repeated status updates) doesn't restart the
        // 'confirm' flourish or jump the scan.
        if (treatment.mode !== this._mode) {
            this._mode = treatment.mode;
            this._actor.setMode(treatment.mode);
        }
        this._label.text = descriptor.statusText;
        this._actor.set_accessible_name(
            descriptor.statusText ? `Dictation: ${descriptor.statusText}` : 'Dictation');

        this._setPulsing(treatment.pulse);

        // An error is terminal on the wire and may NOT be followed by an idle
        // transition (e.g. no backend server running → nothing ever calls
        // hide()). Since the goop is Shell chrome with no window decoration to
        // close it, self-dismiss after the hold so the error shows its reason
        // and then fades away on its own.
        if (descriptor.isError)
            this._scheduleErrorDismiss();
    }

    setLevel(rms, _peak) {
        this._lastLevel = rms;
        this._lastLevelAt = GLib.get_monotonic_time();
    }

    hide() {
        // Errors linger with their reason instead of vanishing (view policy).
        // The dismiss may already be scheduled from show(); either way, hold.
        if (this._holdingError && this._actor !== null) {
            this._scheduleErrorDismiss();
            return;
        }
        this._dismiss();
    }

    // Arm (once) the error hold → self-dismiss timer. Idempotent: a live hold
    // is kept so repeated error/hide events don't restart the countdown.
    _scheduleErrorDismiss() {
        if (this._errorHoldTimer !== 0 || this._actor === null)
            return;
        this._errorHoldTimer = GLib.timeout_add(
            GLib.PRIORITY_DEFAULT, ERROR_HOLD_MS, () => {
                this._errorHoldTimer = 0;
                this._holdingError = false;
                this._dismiss();
                return GLib.SOURCE_REMOVE;
            });
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
            mode: Clutter.AnimationMode.EASE_OUT_BACK,
        });

        this._startVu();
    }

    _startVu() {
        if (this._vuTimer !== 0)
            return;
        this._lastFrameAt = GLib.get_monotonic_time();
        this._vuTimer = GLib.timeout_add(
            GLib.PRIORITY_DEFAULT, Math.floor(1000 / VU_FPS), () => {
                if (this._actor === null)
                    return GLib.SOURCE_REMOVE;
                const now = GLib.get_monotonic_time();
                const dt = Math.min(0.1, (now - this._lastFrameAt) / 1e6);
                this._lastFrameAt = now;
                const ageMs = this._lastLevelAt
                    ? (now - this._lastLevelAt) / 1000
                    : 9999;
                const target = levelToIntensity(this._lastLevel, ageMs);
                this._actor.tick(target, dt);
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
            opacity: 150,
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
        this._mode = null;
        if (actor !== null)
            actor.remove_all_transitions();
        // Collapse in from the sides and vanish in the middle: pivot at the
        // centre and squeeze scale_x → 0 (with a touch of vertical squeeze and
        // a fade) for a smooth pinch-out.
        box.remove_all_transitions();
        box.set_pivot_point(0.5, 0.5);
        box.ease({
            opacity: 0,
            scale_x: 0.0,
            scale_y: 0.85,
            duration: CLEAR_MS,
            mode: Clutter.AnimationMode.EASE_IN_OUT_CUBIC,
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
        this._mode = null;
    }
}
