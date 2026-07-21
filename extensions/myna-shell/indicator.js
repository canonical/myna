// indicator.js — the goop actor + its Shell-chrome lifecycle (feature 004,
// contracts extension.md X8–X12; geometry/animation per research R6).
//
// The goop is an St.DrawingArea added to Main.layoutManager as *chrome* —
// never a window, never reactive, never in the input region — so it cannot
// take keyboard focus by construction (X11/SC-001). It exists only while the
// dictation state ≠ idle (push-to-talk, X3): show() eases it in, hide() eases
// it out and destroys it, destroy() (extension disable) tears down
// immediately with no leaked actors/transitions/signals (X9).
//
// Visual intent (cssClass/animation/a11yLabel) comes from the pure states.js;
// the state colour comes from the `-myna-goop-color` custom property in
// stylesheet.css so visuals are tunable without touching actor code (R6).
// Per-state animations (breathe/ripple/shimmer/…) land with US2 (T021).

import Clutter from 'gi://Clutter';
import GObject from 'gi://GObject';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';

const GOOP_WIDTH = 120;
const GOOP_HEIGHT = 96;
// Ease-in/out well inside the activation/teardown latency targets (SC-003).
const APPEAR_MS = 160;
const CLEAR_MS = 180;

const FALLBACK_COLOR = {red: 0x35, green: 0x84, blue: 0xe4};

const GoopActor = GObject.registerClass(
class GoopActor extends St.DrawingArea {
    _init() {
        super._init({
            style_class: 'myna-goop',
            // Never focusable, never clickable: focus-safety is the feature.
            reactive: false,
            can_focus: false,
            width: GOOP_WIDTH,
            height: GOOP_HEIGHT,
            opacity: 0,
        });
        this._color = FALLBACK_COLOR;
        this.set_accessible_role('status');
        this.connect('repaint', () => this._draw());
    }

    /** Apply a states.js visual-intent record (cssClass + a11y label). */
    setIntent(intent) {
        this.style_class = intent.cssClass
            ? `myna-goop ${intent.cssClass}`
            : 'myna-goop';
        if (intent.a11yLabel)
            this.set_accessible_name(intent.a11yLabel);

        const themeNode = this.get_theme_node();
        const [found, color] = themeNode.lookup_color('-myna-goop-color', false);
        this._color = found ? color : FALLBACK_COLOR;
        this.queue_repaint();
    }

    // The base goop geometry (R6): a droplet hanging from the top bar — flat
    // top edge tucked under the panel, sides curving out into a round bulb.
    _draw() {
        const cr = this.get_context();
        const [width, height] = this.get_surface_size();

        const cx = width / 2;
        const bulbR = Math.min(width, height) * 0.42;
        const bulbCy = height - bulbR - 4;

        cr.moveTo(cx - 14, 0);
        cr.lineTo(cx + 14, 0);
        cr.curveTo(
            cx + 30, bulbCy - bulbR * 0.3,
            cx + bulbR, bulbCy - bulbR * 0.5,
            cx + bulbR, bulbCy);
        cr.arc(cx, bulbCy, bulbR, 0, Math.PI);
        cr.curveTo(
            cx - bulbR, bulbCy - bulbR * 0.5,
            cx - 30, bulbCy - bulbR * 0.3,
            cx - 14, 0);
        cr.closePath();

        const {red, green, blue} = this._color;
        cr.setSourceRGBA(red / 255, green / 255, blue / 255, 0.92);
        cr.fillPreserve();
        cr.setSourceRGBA(red / 255, green / 255, blue / 255, 0.35);
        cr.setLineWidth(2.5);
        cr.stroke();

        cr.$dispose();
    }
});

/**
 * Owns the goop's presence on the Shell chrome: at most one actor, created on
 * the first non-idle intent, eased out and destroyed on idle.
 */
export class GoopIndicator {
    constructor() {
        this._actor = null;
        this._monitorsChangedId = 0;
    }

    /** Show (or retarget) the goop for a non-idle visual intent. */
    show(intent) {
        if (this._actor === null) {
            this._actor = new GoopActor();
            Main.layoutManager.addTopChrome(this._actor, {
                affectsInputRegion: false,
                trackInput: false,
            });
            this._monitorsChangedId = Main.layoutManager.connect(
                'monitors-changed', () => this._position());
            this._position();

            // Appear: fade + grow from the panel edge (R6), pivoting at the
            // top-center the goop hangs from.
            this._actor.set_pivot_point(0.5, 0);
            this._actor.set_scale(0.7, 0.7);
            this._actor.ease({
                opacity: 255,
                scale_x: 1.0,
                scale_y: 1.0,
                duration: APPEAR_MS,
                mode: Clutter.AnimationMode.EASE_OUT_CUBIC,
            });
        }
        this._actor.setIntent(intent);
    }

    /** Ease the goop out and destroy it (state returned to idle). */
    hide() {
        const actor = this._actor;
        if (actor === null)
            return;
        this._detach();
        actor.ease({
            opacity: 0,
            scale_x: 0.7,
            scale_y: 0.7,
            duration: CLEAR_MS,
            mode: Clutter.AnimationMode.EASE_IN_CUBIC,
            onComplete: () => actor.destroy(),
        });
    }

    /** Immediate teardown (extension disable / Shell restart) — no leaks. */
    destroy() {
        if (this._actor !== null) {
            this._actor.remove_all_transitions();
            this._actor.destroy();
        }
        this._detach();
    }

    _detach() {
        if (this._monitorsChangedId !== 0) {
            Main.layoutManager.disconnect(this._monitorsChangedId);
            this._monitorsChangedId = 0;
        }
        this._actor = null;
    }

    // Center-top of the primary monitor, hanging just under the panel.
    _position() {
        if (this._actor === null)
            return;
        const monitor = Main.layoutManager.primaryMonitor;
        this._actor.set_position(
            monitor.x + Math.round((monitor.width - GOOP_WIDTH) / 2),
            monitor.y + Main.panel.height);
    }
}
