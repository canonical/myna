// draft.js - the draft of a dictation, drawn as a bubble above the HUD pill.
//
// With processing on, the daemon holds a dictation back from the field until
// it ends and publishes it as DraftText and DraftTail. This draws them. PLACE
// picks where: above the pill, in the middle of the screen, or next to the
// text caret, which is the Shell's own knowledge (Main.inputMethod).

import Clutter from 'gi://Clutter';
import Pango from 'gi://Pango';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';

import {
    BUBBLE_RADIUS, TAIL_SIZE, draftMarkup, placeAbovePill, placeCentre, placeDraft,
    tailAbovePill,
} from './draftPlace.js';

// 'pill', 'centre' or 'caret'.
const PLACE = 'pill';
const MAX_WIDTH = PLACE === 'pill' ? 640 : 900;

// The HUD pill's own colour (`client/myna-hud/src/style.css`, `.myna-hud-pill`),
// so the bubble reads as the same surface it grows out of. Opaque: the tail
// overlaps the bubble by design, and two translucent layers of it would show
// the seam where they meet.
const BACKGROUND = '#2f2f2f';
const STYLE = [
    `background-color: ${BACKGROUND}`,
    'color: #ffffff',
    `border-radius: ${PLACE === 'pill' ? BUBBLE_RADIUS : 14}px`,
    'padding: 14px 20px',
    `font-size: ${PLACE === 'pill' ? 15 : 17}pt`,
].join('; ');
const TAIL_STYLE = `background-color: ${BACKGROUND}; border-radius: 3px`;

const GROW_FROM = 0.6;
const DRAIN_TO = 0.3;
const GROW_MS = 180;
const DRAIN_MS = 160;

/** The Shell turns animations off for reduced motion and on slow machines. */
function animationsOn() {
    return St.Settings.get().enable_animations;
}

export class DictationDraft {
    /**
     * @param {object} deps
     * @param {DictationProxy} deps.proxy - the shared daemon proxy.
     * @param {function(): object|null} deps.getWorkArea - the work area of
     *   the monitor being typed on, as {x, y, width, height}.
     * @param {function(): object|null} deps.getPillRect - the HUD pill's
     *   frame, or null while it is hidden.
     */
    constructor({proxy, getWorkArea, getPillRect = () => null}) {
        this._proxy = proxy;
        this._getWorkArea = getWorkArea;
        this._getPillRect = getPillRect;
        this._label = null;
        this._tail = null;
        this._caret = null;
        this._pill = null;
        this._shown = false;
        this._draining = false;
    }

    enable() {
        this._label = new St.Label({
            style: STYLE,
            visible: false,
            reactive: false,
            can_focus: false,
        });
        const text = this._label.clutter_text;
        text.line_wrap = true;
        text.line_wrap_mode = Pango.WrapMode.WORD_CHAR;
        text.ellipsize = Pango.EllipsizeMode.NONE;
        // The corner of a square turned 45 degrees, pointing at the pill.
        this._tail = new St.Widget({
            style: TAIL_STYLE,
            width: TAIL_SIZE,
            height: TAIL_SIZE,
            rotation_angle_z: 45,
            visible: false,
            reactive: false,
        });
        this._tail.set_pivot_point(0.5, 0.5);
        Main.layoutManager.uiGroup.add_child(this._tail);
        Main.layoutManager.uiGroup.add_child(this._label);

        Main.inputMethod.connectObject('cursor-location-changed',
            (_im, rect) => {
                this._caret = {
                    x: rect.origin.x,
                    y: rect.origin.y,
                    width: rect.size.width,
                    height: rect.size.height,
                };
                this._place();
            }, this);
        // The caret of one field says nothing about the next.
        global.display.connectObject('notify::focus-window',
            () => (this._caret = null), this);

        this._proxy.whenReady(() => {
            if (!this._label)
                return;
            this._proxy.proxy.connectObject('g-properties-changed',
                () => this._reflect(), this);
            this._reflect();
        });
    }

    disable() {
        this._proxy.proxy?.disconnectObject(this);
        Main.inputMethod.disconnectObject(this);
        global.display.disconnectObject(this);
        this._label?.destroy();
        this._label = null;
        this._tail?.destroy();
        this._tail = null;
        this._caret = null;
        this._pill = null;
        this._shown = false;
        this._draining = false;
    }

    _property(name) {
        return this._proxy.proxy?.get_cached_property(name)?.unpack() ?? '';
    }

    _reflect() {
        if (!this._label)
            return;
        const markup = draftMarkup(
            this._property('DraftText'), this._property('DraftTail'));
        if (!markup) {
            this._drain();
            return;
        }
        this._label.clutter_text.set_markup(markup);
        this._label.show();
        this._place();
        this._grow();
    }

    // Out of the pill as the first words are heard.
    _grow() {
        const fromHidden = !this._shown;
        this._shown = true;
        if (PLACE !== 'pill')
            return;
        this._label.remove_all_transitions();
        this._tail.remove_all_transitions();
        this._tail.visible = !!this._pill;
        // Full size at once when there is nothing to grow from -- the bubble
        // is still up from a drain that had not finished, so a word arriving
        // 100ms after the last one must not flash -- or when the Shell has
        // animations off.
        if (!fromHidden || !animationsOn()) {
            this._draining = false;
            this._label.set_scale(1, 1);
            this._label.opacity = 255;
            this._tail.opacity = 255;
            return;
        }
        this._label.set_pivot_point(0.5, 1);
        this._label.set_scale(GROW_FROM, GROW_FROM);
        this._label.opacity = 0;
        this._tail.opacity = 0;
        this._label.ease({
            scale_x: 1,
            scale_y: 1,
            opacity: 255,
            duration: GROW_MS,
            mode: Clutter.AnimationMode.EASE_OUT_BACK,
        });
        this._tail.ease({opacity: 255, duration: GROW_MS});
    }

    // Back into the pill as the text goes to the field.
    _drain() {
        if (this._draining)
            return;
        if (!this._shown) {
            this._label.remove_all_transitions();
            this._tail.remove_all_transitions();
            this._label.hide();
            this._tail.hide();
            return;
        }
        this._shown = false;
        if (PLACE !== 'pill' || !animationsOn()) {
            this._label.hide();
            this._tail.hide();
            return;
        }
        this._draining = true;
        this._label.remove_all_transitions();
        this._tail.remove_all_transitions();
        const done = () => {
            this._draining = false;
            if (this._shown)
                return;
            this._label?.hide();
            this._tail?.hide();
        };
        this._label.ease({
            scale_x: DRAIN_TO,
            scale_y: DRAIN_TO,
            opacity: 0,
            duration: DRAIN_MS,
            mode: Clutter.AnimationMode.EASE_IN_QUAD,
            onStopped: done,
        });
        this._tail.ease({opacity: 0, duration: DRAIN_MS, onStopped: done});
    }

    _place() {
        if (!this._label?.visible)
            return;
        const workArea = this._getWorkArea();
        if (!workArea)
            return;
        this._label.set_width(-1);
        const [, natural] = this._label.get_preferred_width(-1);
        const width = Math.min(natural, MAX_WIDTH, workArea.width);
        this._label.set_width(width);
        const [, height] = this._label.get_preferred_height(width);
        const size = {width, height};
        const pill = PLACE === 'pill' ? this._getPillRect() : null;
        let position;
        if (pill)
            position = placeAbovePill({pill, size, workArea});
        else if (PLACE === 'centre')
            position = placeCentre({size, workArea});
        else
            position = placeDraft({caret: PLACE === 'caret' ? this._caret : null, size, workArea});
        const {x, y} = position;
        this._label.set_position(Math.round(x), Math.round(y));
        this._pill = pill;
        // Not while the bubble is draining: the caret moving during those
        // 160ms must not bring the tail back out.
        this._tail.visible = !!pill && !this._draining;
        if (pill) {
            // Above the pill, and just clear of it. With the pill hidden or
            // gone (the HUD restarting) the draft stays where the pill sits,
            // which is nearer to it than the middle of the screen would be.
            const tail = tailAbovePill({pill, bubble: {x, y, width, height}});
            this._tail.set_position(tail.x, tail.y);
        }
        // Above windows and the hosted HUD alike.
        const group = this._label.get_parent();
        group?.set_child_above_sibling(this._tail, null);
        group?.set_child_above_sibling(this._label, null);
    }
}
