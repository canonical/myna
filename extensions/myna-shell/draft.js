// draft.js - the draft of a dictation, drawn next to the text caret.
//
// With processing on, the daemon holds a dictation back from the field until
// it ends and publishes it as DraftText and DraftTail. This draws them, in
// the middle of the screen. It can follow the text caret instead
// (FOLLOW_CARET), which is the Shell's own knowledge (Main.inputMethod).

import Pango from 'gi://Pango';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';

import {draftMarkup, placeCentre, placeDraft} from './draftPlace.js';

// In the middle of the screen, where it is plain that the dictation is being
// held and not typed. `true` puts it next to the text caret instead.
const FOLLOW_CARET = false;
const MAX_WIDTH = 900;
const STYLE = [
    'background-color: rgba(36, 36, 36, 0.94)',
    'color: #ffffff',
    'border-radius: 14px',
    'padding: 14px 20px',
    'font-size: 17pt',
].join('; ');

export class DictationDraft {
    /**
     * @param {object} deps
     * @param {DictationProxy} deps.proxy - the shared daemon proxy.
     * @param {function(): object|null} deps.getWorkArea - the work area of
     *   the monitor being typed on, as {x, y, width, height}.
     */
    constructor({proxy, getWorkArea}) {
        this._proxy = proxy;
        this._getWorkArea = getWorkArea;
        this._label = null;
        this._caret = null;
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
        this._caret = null;
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
            this._label.hide();
            return;
        }
        this._label.clutter_text.set_markup(markup);
        this._label.show();
        this._place();
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
        const {x, y} = FOLLOW_CARET
            ? placeDraft({caret: this._caret, size, workArea})
            : placeCentre({size, workArea});
        this._label.set_position(Math.round(x), Math.round(y));
        // Above windows and the hosted HUD alike.
        this._label.get_parent()?.set_child_above_sibling(this._label, null);
    }
}
