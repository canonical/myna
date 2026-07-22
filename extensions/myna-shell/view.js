// view.js — the IndicatorView SEAM (feature 004-gnome-shell-indicator).
//
// This is the clear line between what we're certain about and the
// experimental UI. Everything the extension is confident in — the
// org.myna.Dictation contract, the proxy/lifecycle (dbus.js), the state set
// and semantic descriptors (states.js), the Rust level pump — lives on one
// side. Everything about *how it looks* — geometry, colour, animation, VU
// rendering, status text placement — lives behind this interface, in a view
// implementation. A redesign (the team's, or a future user theme) is: write a
// new IndicatorView and change the factory below. Nothing else moves.
//
// # IndicatorView interface
//
// A view renders dictation activity as Shell chrome. It is driven entirely by
// the extension with content-free inputs (state descriptor + normalized
// level) and must never take keyboard focus (X11). Methods:
//
//   show(descriptor)   Make the indicator visible (if not already) and render
//                      this semantic descriptor from states.js:
//                      `{key, statusText, isError, hidden}`. Never called with
//                      a hidden descriptor (the extension calls hide() for idle).
//   setLevel(rms, peak) Feed the latest normalized audio level in [0,1]
//                      (org.myna.Dictation AudioRms/AudioPeak) for the VU.
//                      May arrive before show() or after hide(); a view must
//                      tolerate that (ignore when not visible).
//   hide()             Return to idle: animate away and release actors. An
//                      error descriptor MAY be held briefly by the view before
//                      it honours a hide (so errors don't just vanish).
//   destroy()          Immediate teardown (extension disable / Shell restart):
//                      destroy every actor, cancel every timer/transition,
//                      drop every subscription — no leaks (X9).
//
// A view carries/renders no transcript content — only the descriptor's
// content-free statusText and the level (constitution V).

import {RibbonView} from './indicator.js';

/**
 * Construct the active IndicatorView. The single place a redesign is selected;
 * `name` leaves room for future user choice (settings/themes).
 *
 * @param {string} [name] - view id; defaults to the experimental ribbon.
 * @returns {object} an IndicatorView (see the interface above).
 */
export function createView(name = 'ribbon') {
    switch (name) {
    case 'ribbon':
    default:
        return new RibbonView();
    }
}
