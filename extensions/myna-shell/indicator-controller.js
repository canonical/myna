export const RECOVERABLE_HOLD_MS = 3500;

import {normalizeHudStyle} from './view-selection.js';

export class IndicatorController {
    constructor({
        style = 'basic',
        createView,
        now = () => Date.now(),
        schedule = (delay, callback) => setTimeout(callback, delay),
        cancel = id => clearTimeout(id),
    }) {
        this._createView = createView;
        this._now = now;
        this._schedule = schedule;
        this._cancel = cancel;
        this._timer = 0;
        this._held = null;
        this._source = null;
        this._displayed = null;
        this._level = null;
        this._destroyed = false;
        this._generation = 0;
        this._style = normalizeHudStyle(style);
        this._view = this._newView(this._style);
    }

    setStyle(style) {
        if (this._destroyed)
            return;
        const normalized = normalizeHudStyle(style);
        if (normalized === this._style)
            return;
        this._view.destroy();
        this._style = normalized;
        this._view = this._newView(normalized);
        if (this._displayed !== null)
            this._view.show(this._displayed);
        if (this._level !== null) {
            this._view.setLevel(
                this._level.rms, this._level.peak, this._level.receivedAt);
        }
    }

    onDescriptor(descriptor) {
        if (this._destroyed)
            return;
        this._source = descriptor;
        if (descriptor.severity !== null) {
            this._setHeld(descriptor);
            return;
        }
        if (descriptor.hidden && this._held !== null) {
            this._render(this._held.descriptor);
            return;
        }
        this._clearHeld();
        if (descriptor.hidden) {
            this._displayed = null;
            this._view.hide();
        } else {
            this._render(descriptor);
        }
    }

    onLevel(rms, peak, receivedAt = this._now()) {
        if (this._destroyed)
            return;
        this._level = {rms, peak, receivedAt};
        this._view.setLevel(rms, peak, receivedAt);
    }

    dismiss() {
        if (this._destroyed || this._held?.descriptor.severity !== 'critical')
            return;
        this._clearHeld();
        this._displayed = null;
        this._view.hide();
    }

    onServiceUnavailable() {
        if (this._destroyed)
            return;
        if (this._held !== null) {
            this._render(this._held.descriptor);
            return;
        }
        this._source = null;
        this._displayed = null;
        this._view.hide();
    }

    destroy() {
        if (this._destroyed)
            return;
        this._destroyed = true;
        this._clearTimer();
        this._view.destroy();
    }

    _newView(style) {
        const generation = ++this._generation;
        return this._createView(style, {
            onDismiss: () => {
                if (!this._destroyed && generation === this._generation)
                    this.dismiss();
            },
        });
    }

    _render(descriptor) {
        this._displayed = descriptor;
        this._view.show(descriptor);
    }

    _setHeld(descriptor) {
        this._clearTimer();
        const deadline = descriptor.severity === 'recoverable'
            ? this._now() + RECOVERABLE_HOLD_MS
            : null;
        this._held = {descriptor, deadline};
        this._render(descriptor);
        if (deadline !== null) {
            this._timer = this._schedule(RECOVERABLE_HOLD_MS, () => {
                this._timer = 0;
                this._held = null;
                this._displayed = null;
                this._view.hide();
            });
        }
    }

    _clearTimer() {
        if (this._timer !== 0) {
            this._cancel(this._timer);
            this._timer = 0;
        }
    }

    _clearHeld() {
        this._clearTimer();
        this._held = null;
    }
}
