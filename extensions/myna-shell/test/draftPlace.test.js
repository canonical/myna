// draftPlace.test.js - GJS contract test for the draft's placement and text.
// No Shell, no D-Bus.
//
//     gjs -m test/draftPlace.test.js        (from extensions/myna-shell/)

import System from 'system';

import {CARET_GAP, FALLBACK_BOTTOM, draftEnd, draftMarkup, placeDraft} from '../draftPlace.js';

let failures = 0;

function eq(name, actual, expected) {
    const a = JSON.stringify(actual);
    const e = JSON.stringify(expected);
    if (a === e) {
        print(`ok   ${name}`);
    } else {
        failures++;
        print(`FAIL ${name}: got ${a}, want ${e}`);
    }
}

const WORK_AREA = {x: 0, y: 32, width: 1920, height: 1048};
const SIZE = {width: 300, height: 40};

// --- Next to the caret -----------------------------------------------------

eq('below the caret, starting at it',
    placeDraft({caret: {x: 500, y: 400, width: 2, height: 20}, size: SIZE, workArea: WORK_AREA}),
    {x: 500, y: 400 + 20 + CARET_GAP});

eq('above the caret when there is no room below',
    placeDraft({caret: {x: 500, y: 1050, width: 2, height: 20}, size: SIZE, workArea: WORK_AREA}),
    {x: 500, y: 1050 - CARET_GAP - SIZE.height});

eq('kept inside the right edge',
    placeDraft({caret: {x: 1900, y: 400, width: 2, height: 20}, size: SIZE, workArea: WORK_AREA}),
    {x: 1920 - SIZE.width, y: 426});

eq('on the caret\'s own monitor',
    placeDraft({
        caret: {x: 2000, y: 100, width: 2, height: 20},
        size: SIZE,
        workArea: {x: 1920, y: 0, width: 2560, height: 1440},
    }),
    {x: 2000, y: 126});

eq('a caret with a height and no width still counts',
    placeDraft({caret: {x: 10, y: 100, width: 0, height: 18}, size: SIZE, workArea: WORK_AREA}),
    {x: 10, y: 124});

// --- No caret --------------------------------------------------------------

const fallback = {
    x: (1920 - SIZE.width) / 2,
    y: 32 + 1048 - SIZE.height - FALLBACK_BOTTOM,
};
eq('bottom centre with no caret',
    placeDraft({caret: null, size: SIZE, workArea: WORK_AREA}), fallback);
eq('bottom centre when the field reports an empty caret',
    placeDraft({caret: {x: 0, y: 0, width: 0, height: 0}, size: SIZE, workArea: WORK_AREA}),
    fallback);

eq('a draft larger than the work area starts at its origin',
    placeDraft({caret: null, size: {width: 4000, height: 4000}, workArea: WORK_AREA}),
    {x: 0, y: 32});

// --- What it says ----------------------------------------------------------

eq('nothing to show', draftMarkup('', ''), '');
eq('only whitespace is nothing to show', draftMarkup(' ', ''), '');
eq('settled text alone', draftMarkup('hello', ''), 'hello');
eq('the guess is dimmed after the settled text',
    draftMarkup('hello', 'wor'), 'hello <span alpha="55%">wor</span>');
eq('a guess alone is dimmed',
    draftMarkup('', 'hel'), '<span alpha="55%">hel</span>');
eq('markup in the text is escaped',
    draftMarkup('a < b & c', '>'), 'a &lt; b &amp; c <span alpha="55%">&gt;</span>');

eq('a short text is shown whole', draftEnd('one two', 20), 'one two');
eq('a long text shows its end',
    draftEnd('one two three four', 10), '…three four');
eq('a long text is cut at a word, not inside one',
    draftEnd('one two three four', 8), '…four');
eq('a long text with no space is cut where it must be',
    draftEnd('abcdefghij', 4), '…ghij');
eq('long drafts keep the guess dimmed',
    draftMarkup('one two three', 'four', 10), '…three <span alpha="55%">four</span>');

System.exit(failures ? 1 : 0);
