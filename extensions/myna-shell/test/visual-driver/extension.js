// extension.js - the in-shell half of the HUD presentation check (feature
// 004-gnome-shell-indicator; run by test/entrance-visual.sh, never shipped).
//
// The pill's entrance is Clutter easing an St actor on a real frame clock.
// None of that exists headless, so the pure-logic tests next door cannot see
// it: the bug this suite was written for (2026-08-20) was an `opacity` eased
// with EASE_OUT_BACK, whose overshoot past 255 wrapped a guint8 to 24 and
// blanked the pill mid-entrance, and every unit test passed throughout.
//
// So drive the real HudView inside a real Shell and sample what the compositor
// would show, once per presented frame. The assertions are about presentation
// only - is the pill continuously visible while it is meant to be - never
// about geometry or colour, which stay manual-acceptance (quickstart.md §5).
//
// Results go to the journal as `MYNA-VISUAL:` lines; the harness reads those.

import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';

// Nothing here waits a fixed length of time for something the frame clock
// drives. hud.js's animations advance per presented frame, so on a starved
// shell a wall-clock delay expires with the entrance barely begun - which is
// how both of this suite's own flakes started life. Every wait below is on the
// animation's actual state, with these only as give-up ceilings.
const SETTLE_TIMEOUT_MS = 5000;
const HIDE_TIMEOUT_MS = 5000;

// How far into the dismiss fade to restart the session, as the pill's scale
// rather than a delay. The fade runs 1.0 → 0.9, so this leaves plenty of room
// above the 0.9 an entrance re-run would snap it back to.
const REVERSAL_SCALE = 0.98;

// An animation sampled at only a handful of frames could hide a one-frame
// blank between samples, so a run that thin is inconclusive, not a pass. The
// entrance is 180 ms, about 11 frames at 60 Hz. A loaded machine gets retries
// before the suite gives up and tells the harness it could not measure.
const MIN_SAMPLES = 6;
const SAMPLE_ATTEMPTS = 3;

const RECORDING = {key: 'recording', statusText: 'Listening', severity: null};
const LOADING = {key: 'loading', statusText: 'Loading model…', severity: null};

function report(line) {
    log(`MYNA-VISUAL: ${line}`);
}

/** Every `.myna-hud-pill` currently parented anywhere under the Shell's
 * chrome - the count catches a second pill stacked over the first. */
function findPills() {
    const found = [];
    const walk = actor => {
        if (typeof actor.get_style_class_name === 'function' &&
            (actor.get_style_class_name() ?? '').split(/\s+/).includes('myna-hud-pill'))
            found.push(actor);
        for (const child of actor)
            walk(child);
    };
    walk(Main.layoutManager.uiGroup);
    return found;
}

export default class VisualDriverExtension {
    enable() {
        this._failures = 0;
        this._timers = [];
        this._samples = [];
        this._sampler = null;
        this._view = null;
        // Let the stage settle before measuring anything on its frame clock.
        this._defer(500, () => this._run().catch(e => {
            report(`FAIL the driver threw: ${e}`);
            report(`DONE ${this._failures + 1}`);
        }));
    }

    disable() {
        for (const id of this._timers)
            GLib.source_remove(id);
        this._timers = [];
        this._stopSampling();
        this._view?.destroy();
        this._view = null;
    }

    _defer(ms, fn) {
        const id = GLib.timeout_add(GLib.PRIORITY_DEFAULT, ms, () => {
            this._timers = this._timers.filter(t => t !== id);
            fn();
            return GLib.SOURCE_REMOVE;
        });
        this._timers.push(id);
    }

    /** Resolve true once `predicate` holds, false if it has not within
     * `timeoutMs`. Polled just under a frame, so nothing waits on wall-clock
     * time for something the frame clock drives. */
    _waitUntil(predicate, timeoutMs) {
        const deadline = GLib.get_monotonic_time() + timeoutMs * 1000;
        return new Promise(resolve => {
            const poll = () => {
                if (predicate())
                    resolve(true);
                else if (GLib.get_monotonic_time() >= deadline)
                    resolve(false);
                else
                    this._defer(8, poll);
            };
            poll();
        });
    }

    _check(name, condition, detail = '') {
        if (condition) {
            report(`ok   ${name}`);
        } else {
            this._failures++;
            report(`FAIL ${name}${detail ? ` - ${detail}` : ''}`);
        }
    }

    // Sample the pill once per presented frame. Bound to the stage rather than
    // to the pill: the stage is always mapped, so sampling survives the pill
    // being hidden - which is exactly the state a blanking bug produces. The
    // pill is looked up per frame rather than handed in, so a build that
    // creates it late still gets sampled and each scenario fails on its own
    // merits instead of being masked by an earlier one.
    _startSampling() {
        this._samples = [];
        this._sampler = new Clutter.Timeline({
            actor: global.stage,
            duration: 1000,
            repeat_count: -1,
        });
        this._sampler.connect('new-frame', () => {
            const box = findPills()[0] ?? null;
            this._samples.push({
                t: GLib.get_monotonic_time() / 1000,
                opacity: box?.opacity ?? 0,
                visible: box?.visible ?? false,
                scale: box?.scale_x ?? 0,
            });
        });
        this._sampler.start();
    }

    _stopSampling() {
        if (this._sampler === null)
            return;
        this._sampler.stop();
        this._sampler.set_actor(null);
        this._sampler = null;
    }

    /** Whether every transition hud.js starts on the pill has finished. This
     * is the end of an entrance or a dismiss, however many frames it took. */
    _settled(box) {
        return box.get_transition('opacity') === null &&
            box.get_transition('scale-x') === null;
    }

    /** Sample one full entrance, from off screen to settled. Retries a run too
     * thin to judge; null when it never got enough frames. */
    async _sampleEntrance(box) {
        for (let attempt = 1; attempt <= SAMPLE_ATTEMPTS; attempt++) {
            this._view.hide();
            if (!await this._waitUntil(() => !box.visible, HIDE_TIMEOUT_MS))
                continue;

            this._startSampling();
            this._view.show(LOADING);
            const settled = await this._waitUntil(
                () => this._settled(box), SETTLE_TIMEOUT_MS);
            this._stopSampling();

            if (settled && this._samples.length >= MIN_SAMPLES)
                return this._samples;
            report(`note entrance attempt ${attempt}: ` +
                `${this._samples.length} frames, settled=${settled}`);
        }
        return null;
    }

    /** Sample a session restarting inside the previous one's dismiss fade.
     * Returns the frames from the restart onward, plus where the fade had got
     * to at that instant; null when it never got enough frames. */
    async _sampleReversal(box) {
        for (let attempt = 1; attempt <= SAMPLE_ATTEMPTS; attempt++) {
            if (!box.visible || !this._settled(box)) {
                this._view.show(RECORDING);
                if (!await this._waitUntil(
                    () => box.visible && this._settled(box), SETTLE_TIMEOUT_MS))
                    continue;
            }

            this._startSampling();
            this._view.hide();
            const fading = await this._waitUntil(
                () => box.scale_x <= REVERSAL_SCALE, SETTLE_TIMEOUT_MS);
            const at = {opacity: box.opacity, scale: box.scale_x};
            this._view.show(RECORDING);
            const from = this._samples.length;
            const settled = await this._waitUntil(
                () => this._settled(box), SETTLE_TIMEOUT_MS);
            this._stopSampling();

            const after = this._samples.slice(from);
            if (fading && settled && after.length >= MIN_SAMPLES)
                return {at, after};
            report(`note reversal attempt ${attempt}: ${after.length} frames, ` +
                `fading=${fading} settled=${settled}`);
        }
        return null;
    }

    async _run() {
        const src = GLib.getenv('MYNA_SHELL_SRC');
        if (!src) {
            report('FAIL MYNA_SHELL_SRC unset - nothing to drive');
            report('DONE 1');
            return;
        }
        const {HudView} = await import(`file://${src}/hud.js`);
        // The extension system would normally do this. Without it the pill
        // resolves to no style at all, and the entrance being measured would
        // be of an unpadded, unstyled actor rather than the real one.
        St.ThemeContext.get_for_stage(global.stage).get_theme()
            .load_stylesheet(Gio.File.new_for_path(`${src}/stylesheet.css`));

        // Every assertion below is about an animation. With animations off,
        // ease() jumps straight to its target and the whole suite would pass
        // without measuring anything. St.Settings, not a GSettings read: it is
        // what ease() itself consults.
        //
        // The Shell inhibits animations whenever it is rendering in software,
        // which is every container and every hosted CI runner. That is a
        // performance heuristic about the machine, not behaviour under test, so
        // release it. The harness owns the enable-animations setting and sets
        // it true, so an inhibit is the only thing that can be holding them
        // off, and the count is therefore non-zero.
        const settings = St.Settings.get();
        if (!settings.enable_animations)
            settings.uninhibit_animations();
        this._check('animations are enabled (the suite is meaningless without them)',
            settings.enable_animations);

        // ── V1: the actor tree is built at construction ──────────────────────
        // Not on the first show(). Actor construction, a Gio.Settings open and
        // a full CSS resolve are not work to do in the frame the pill is
        // trying to appear in (osdWindow.js builds its OSD at startup).
        this._check('V1 no pill in the chrome before the view exists',
            findPills().length === 0, `found ${findPills().length}`);

        this._view = new HudView();
        const built = findPills();
        this._check('V1 constructing the view builds the pill, before any show()',
            built.length === 1, `found ${built.length}`);
        if (built.length === 1) {
            this._check('V1 the freshly built pill is not yet on screen',
                !built[0].visible || built[0].opacity === 0,
                `visible=${built[0].visible} opacity=${built[0].opacity}`);
        }

        // ── V2: the entrance never blanks ────────────────────────────────────
        // A build that only creates the pill on the first show() has already
        // failed V1; materialise it anyway, so the scenarios below still
        // report on their own merits instead of dying on a missing actor.
        if (findPills().length === 0) {
            this._view.show(LOADING);
            await this._waitUntil(() => findPills().length > 0, SETTLE_TIMEOUT_MS);
        }
        const box = findPills()[0];
        if (box === undefined) {
            report('FAIL no pill in the chrome even after a show()');
            this._failures++;
            report(`DONE ${this._failures}`);
            return;
        }
        const entrance = await this._sampleEntrance(box);
        if (entrance === null) {
            report('INCONCLUSIVE the entrance never sampled enough frames here');
            report(`DONE ${this._failures}`);
            return;
        }
        report(`note entrance sampled over ${entrance.length} frames`);

        // The bug: opacity overshot 255, wrapped its guint8, and the pill
        // dropped to ~9% for the back half of its own entrance. A monotone
        // fade-in cannot decrease, so any decrease at all is the defect.
        const dips = [];
        for (let i = 1; i < entrance.length; i++) {
            if (entrance[i].opacity < entrance[i - 1].opacity)
                dips.push(`${entrance[i - 1].opacity}→${entrance[i].opacity}`);
        }
        this._check('V2 opacity never decreases during the entrance',
            dips.length === 0, `dips: ${dips.join(', ')}`);
        this._check('V2 the pill stays visible throughout the entrance',
            entrance.every(s => s.visible),
            `${entrance.filter(s => !s.visible).length} hidden frames`);
        this._check('V2 the entrance ends fully opaque',
            entrance.at(-1).opacity === 255, `ended at ${entrance.at(-1).opacity}`);

        // ── V3: a show() during the dismiss fade reverses it ─────────────────
        // The pill is reused, so a session starting inside the previous one's
        // fade-out must pick it up where the fade left it. Re-running the
        // entrance over an actor the user can still see re-shrinks it (a
        // visible bounce) and collapses the live wave flat.
        const reversal = await this._sampleReversal(box);
        if (reversal === null) {
            report('INCONCLUSIVE the reversal never sampled enough frames here');
            report(`DONE ${this._failures}`);
            return;
        }
        const {at: atReversal, after} = reversal;
        report(`note reversal sampled over ${after.length} frames, ` +
            `restarted at scale ${atReversal.scale.toFixed(3)}`);

        this._check('V3 the pill is never hidden while the fade is reversed',
            after.every(s => s.visible),
            `${after.filter(s => !s.visible).length} hidden frames`);
        // EASE_OUT_BACK overshoots above 1.0 and settles back, so the scale is
        // not monotonic - but it must never fall below where the reversal
        // picked it up. Re-seeding it to 0.9 is exactly that fall.
        const minScale = Math.min(...after.map(s => s.scale));
        this._check('V3 the pill is never re-shrunk mid-reversal',
            minScale >= atReversal.scale - 0.001,
            `fell to ${minScale.toFixed(3)} from ${atReversal.scale.toFixed(3)}`);
        this._check('V3 the reversal ends fully opaque',
            after.at(-1).opacity === 255, `ended at ${after.at(-1).opacity}`);
        this._check('V3 still exactly one pill in the chrome',
            findPills().length === 1, `found ${findPills().length}`);

        // ── V4: the pill is never buried under other chrome ─────────────────
        // Chrome siblings paint in insertion order, so anything that assumes
        // it was added last is wrong: the Ubuntu dock re-adds itself on every
        // re-track, and with a bottom dock that does not reserve space (its
        // intellihide state) the two overlap, so losing this puts the pill
        // completely behind the dock. A plain chrome actor added after the
        // view stands in for it, and needs no dock installed to do so.
        const laterChrome = new Clutter.Actor({name: 'myna-visual-later-chrome'});
        Main.layoutManager.addChrome(laterChrome);
        this._view.hide();
        await this._waitUntil(() => !box.visible, HIDE_TIMEOUT_MS);
        this._view.show(RECORDING);
        await this._waitUntil(() => this._settled(box), SETTLE_TIMEOUT_MS);

        const siblings = [...Main.layoutManager.uiGroup];
        const pillAt = siblings.indexOf(box.get_parent());
        const otherAt = siblings.indexOf(laterChrome);
        this._check('V4 presenting raises the pill above chrome added after it',
            pillAt > otherAt && pillAt >= 0 && otherAt >= 0,
            `pill at ${pillAt}, later chrome at ${otherAt}`);

        Main.layoutManager.removeChrome(laterChrome);
        laterChrome.destroy();

        // ── Teardown leaves nothing behind (X9) ──────────────────────────────
        this._view.destroy();
        this._view = null;
        this._check('teardown removes the pill from the chrome',
            findPills().length === 0, `found ${findPills().length}`);

        report(`DONE ${this._failures}`);
    }
}
