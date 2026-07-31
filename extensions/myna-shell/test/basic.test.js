import System from 'system';

import {
    basicTargetFill,
    smoothBasicFill,
} from '../basic-logic.js';

let failures = 0;
function check(name, condition) {
    if (condition)
        print(`ok   ${name}`);
    else {
        failures++;
        print(`FAIL ${name}`);
    }
}

function between(value) {
    return value >= 0 && value <= 1;
}

const quiet = basicTargetFill('recording', 0.002, 0.004, 0);
const loud = basicTargetFill('recording', 0.08, 0.15, 0);
check('normal louder input increases fill', loud > quiet);
check('fresh recording produces nonzero fill', loud > 0);
check('shared VU floor is normalized to true zero', basicTargetFill('recording', 0, 0, 0) === 0);
for (const value of [NaN, -1, 0, 1, 5])
    check(`malformed/out-of-range ${value} remains bounded`, between(basicTargetFill('recording', value, value, 0)));
for (const key of ['idle', 'loading', 'transcribing', 'finalizing', 'notice', 'error'])
    check(`${key} targets zero`, basicTargetFill(key, 0.5, 0.8, 0) === 0);
check('stale recording targets zero', basicTargetFill('recording', 0.5, 0.8, 600) === 0);
check('same numeric level is fresh when age resets', basicTargetFill('recording', 0.02, 0.04, 0) > basicTargetFill('recording', 0.02, 0.04, 600));

const attack = smoothBasicFill(0, 1, 16);
const release = smoothBasicFill(1, 0, 16);
check('attack moves immediately without snapping', attack > 0 && attack < 1);
check('release eases rather than snapping', release > 0 && release < 1);
check('attack is faster than release', attack > 1 - release);
check('reduced motion snaps to target', smoothBasicFill(0.2, 0.8, 16, true) === 0.8);

let decayed = 1;
for (let i = 0; i < 40; i++)
    decayed = smoothBasicFill(decayed, 0, 16);
check('release reaches visually empty within 600ms', decayed < 0.01);

print(failures === 0 ? 'PASS basic.test.js' : `FAIL basic.test.js: ${failures} failure(s)`);
System.exit(failures === 0 ? 0 : 1);
