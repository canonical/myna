import System from 'system';

import {IndicatorController} from '../indicator-controller.js';

let failures = 0;

function check(name, condition) {
    if (condition)
        print(`ok   ${name}`);
    else {
        failures++;
        print(`FAIL ${name}`);
    }
}

function eq(name, actual, expected) {
    check(`${name} (got ${JSON.stringify(actual)})`,
        JSON.stringify(actual) === JSON.stringify(expected));
}

function harness(style = 'wave') {
    let now = 1000;
    let nextTimer = 1;
    const timers = new Map();
    const views = [];
    const createView = (name, {onDismiss}) => {
        const calls = [];
        const view = {
            name,
            calls,
            show: descriptor => calls.push(['show', descriptor]),
            setLevel: (rms, peak, receivedAt) => calls.push(['level', rms, peak, receivedAt]),
            hide: () => calls.push(['hide']),
            destroy: () => calls.push(['destroy']),
            dismiss: () => onDismiss(),
        };
        views.push(view);
        return view;
    };
    const controller = new IndicatorController({
        style,
        createView,
        now: () => now,
        schedule: (delay, callback) => {
            const id = nextTimer++;
            timers.set(id, {at: now + delay, callback});
            return id;
        },
        cancel: id => timers.delete(id),
    });
    return {
        controller,
        views,
        timers,
        setNow: value => now = value,
        fireDue: () => {
            for (const [id, timer] of [...timers]) {
                if (timer.at <= now) {
                    timers.delete(id);
                    timer.callback();
                }
            }
        },
    };
}

const recording = {key: 'recording', statusText: 'Listening…', severity: null, hidden: false};
const idle = {key: 'idle', statusText: '', severity: null, hidden: true};
const notice = {key: 'notice', statusText: 'No speech detected', severity: 'recoverable', hidden: false};
const error = {key: 'error', statusText: 'Microphone unavailable', severity: 'critical', hidden: false};

{
    const h = harness();
    h.controller.onDescriptor(recording);
    h.controller.onLevel(0.2, 0.4, 1010);
    eq('current view receives descriptor', h.views[0].calls[0], ['show', recording]);
    eq('current view receives timestamped level', h.views[0].calls[1], ['level', 0.2, 0.4, 1010]);
    h.controller.onDescriptor(idle);
    eq('idle hides ordinary state', h.views[0].calls.at(-1), ['hide']);
}

{
    const h = harness('wave');
    h.controller.onDescriptor(recording);
    h.controller.onLevel(0.3, 0.5, 1010);
    const retired = h.views[0];
    h.controller.setStyle('basic');
    eq('switch destroys old view first', retired.calls.at(-1), ['destroy']);
    check('switch creates one replacement', h.views.length === 2 && h.views[1].name === 'basic');
    eq('switch replays descriptor', h.views[1].calls[0], ['show', recording]);
    eq('switch replays original level timestamp', h.views[1].calls[1], ['level', 0.3, 0.5, 1010]);
    h.controller.setStyle('basic');
    check('unchanged style is a no-op', h.views.length === 2);
    retired.dismiss();
    check('retired dismiss callback is inert', h.views[1].calls.at(-1)[0] === 'level');
}

{
    const h = harness('wave');
    h.controller.onDescriptor(idle);
    h.controller.setStyle('future');
    check('invalid style selects basic', h.views.at(-1).name === 'basic');
    check('hidden switch does not show', h.views.at(-1).calls.length === 0);
}

{
    const h = harness('wave');
    h.controller.onDescriptor(notice);
    const timer = [...h.timers.values()][0];
    h.setNow(2000);
    h.controller.setStyle('basic');
    check('recoverable switch preserves absolute deadline', [...h.timers.values()][0].at === timer.at);
    eq('recoverable descriptor replays', h.views.at(-1).calls[0], ['show', notice]);
}

{
    const h = harness('wave');
    h.controller.onDescriptor(error);
    h.views[0].dismiss();
    h.controller.setStyle('basic');
    check('dismissed critical does not resurrect after switch', h.views.at(-1).calls.length === 0);
}

{
    const h = harness('wave');
    h.controller.onDescriptor(recording);
    h.controller.onServiceUnavailable();
    eq('service loss clears ordinary state', h.views[0].calls.at(-1), ['hide']);
    h.controller.onDescriptor(notice);
    const deadline = [...h.timers.values()][0].at;
    h.controller.onServiceUnavailable();
    eq('service loss preserves held notice', h.views[0].calls.at(-1), ['show', notice]);
    check('service loss preserves held deadline', [...h.timers.values()][0].at === deadline);
}

{
    const h = harness('wave');
    for (let i = 0; i < 100; i++)
        h.controller.setStyle(i % 2 === 0 ? 'basic' : 'wave');
    check('100 switches create one current generation', h.views.length === 101);
    check('every retired view destroyed exactly once',
        h.views.slice(0, -1).every(v => v.calls.filter(c => c[0] === 'destroy').length === 1));
    check('final view remains live', !h.views.at(-1).calls.some(c => c[0] === 'destroy'));
}

{
    const h = harness();
    h.controller.onDescriptor(notice);
    eq('recoverable creates one timer', h.timers.size, 1);
    const firstDeadline = [...h.timers.values()][0].at;
    h.setNow(1500);
    h.controller.onDescriptor(notice);
    check('genuine repeated recoverable restarts full deadline',
        [...h.timers.values()][0].at > firstDeadline);
    h.setNow(2000);
    h.controller.onDescriptor(idle);
    eq('idle does not hide held recoverable notice', h.views[0].calls.at(-1), ['show', notice]);
    h.setNow(5000);
    h.fireDue();
    eq('recoverable deadline hides notice', h.views[0].calls.at(-1), ['hide']);
    eq('recoverable timer consumed', h.timers.size, 0);
}

{
    const h = harness();
    h.controller.onDescriptor(error);
    h.controller.onDescriptor(idle);
    eq('critical remains held through idle', h.views[0].calls.at(-1), ['show', error]);
    h.views[0].dismiss();
    eq('critical dismiss hides', h.views[0].calls.at(-1), ['hide']);
}

{
    const h = harness();
    h.controller.onDescriptor(notice);
    h.controller.destroy();
    eq('destroy cancels controller timer', h.timers.size, 0);
    eq('destroy tears down view', h.views[0].calls.at(-1), ['destroy']);
    h.controller.destroy();
    eq('destroy is idempotent', h.views[0].calls.filter(c => c[0] === 'destroy').length, 1);
}

print(failures === 0
    ? 'PASS indicator-controller.test.js'
    : `FAIL indicator-controller.test.js: ${failures} failure(s)`);
System.exit(failures === 0 ? 0 : 1);
