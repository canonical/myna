import System from 'system';

import {
    createSelectedView,
    normalizeHudStyle,
} from '../view-selection.js';

let failures = 0;
function check(name, condition) {
    if (condition)
        print(`ok   ${name}`);
    else {
        failures++;
        print(`FAIL ${name}`);
    }
}

check('basic normalizes to basic', normalizeHudStyle('basic') === 'basic');
check('wave normalizes to wave', normalizeHudStyle('wave') === 'wave');
for (const value of [undefined, null, '', 'future'])
    check(`${value} falls back to basic`, normalizeHudStyle(value) === 'basic');

const calls = [];
const constructors = {
    basic: options => ({kind: 'basic', options}),
    wave: options => ({kind: 'wave', options}),
};
const onDismiss = () => calls.push('dismiss');
const basic = createSelectedView('unknown', {onDismiss}, constructors);
const wave = createSelectedView('wave', {onDismiss}, constructors);
check('unknown selects injected basic constructor', basic.kind === 'basic');
check('wave selects injected wave constructor', wave.kind === 'wave');
check('onDismiss forwarded unchanged to basic', basic.options.onDismiss === onDismiss);
check('onDismiss forwarded unchanged to wave', wave.options.onDismiss === onDismiss);

print(failures === 0
    ? 'PASS view-selection.test.js'
    : `FAIL view-selection.test.js: ${failures} failure(s)`);
System.exit(failures === 0 ? 0 : 1);
