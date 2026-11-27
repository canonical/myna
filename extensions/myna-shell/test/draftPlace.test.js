// draftPlace.test.js - GJS contract test for the draft's placement and text.
// No Shell, no D-Bus.
//
//     gjs -m test/draftPlace.test.js        (from extensions/myna-shell/)

import System from 'system';

import {
    BUBBLE_RADIUS, CARET_GAP, FALLBACK_BOTTOM, PILL_GAP, TAIL_SIZE, draftEnd, draftMarkup,
    placeAbovePill, placeCentre, placeDraft, tailAbovePill,
} from '../draftPlace.js';

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

// --- In the middle -----------------------------------------------------------

eq('centred in the work area',
    placeCentre({size: SIZE, workArea: WORK_AREA}),
    {x: (1920 - SIZE.width) / 2, y: 32 + (1048 - SIZE.height) / 2});
eq('centred on a second monitor',
    placeCentre({size: SIZE, workArea: {x: 1920, y: 0, width: 2560, height: 1440}}),
    {x: 1920 + (2560 - SIZE.width) / 2, y: (1440 - SIZE.height) / 2});
eq('a draft larger than the work area starts at its origin when centred',
    placeCentre({size: {width: 4000, height: 4000}, workArea: WORK_AREA}),
    {x: 0, y: 32});

// --- Above the pill ---------------------------------------------------------

const PILL = {x: 800, y: 1000, width: 200, height: 40};
const BUBBLE = (pill) =>
    ({...placeAbovePill({pill, size: SIZE, workArea: WORK_AREA}), ...SIZE});

eq('above the pill, centred on it',
    placeAbovePill({pill: PILL, size: SIZE, workArea: WORK_AREA}),
    {x: 750, y: 1000 - PILL_GAP - SIZE.height});

eq('kept off the left edge above a pill at it',
    placeAbovePill({pill: {...PILL, x: 0}, size: SIZE, workArea: WORK_AREA}),
    {x: 0, y: 946});

eq('kept off the right edge above a pill at it',
    placeAbovePill({pill: {...PILL, x: 1820}, size: SIZE, workArea: WORK_AREA}),
    {x: 1920 - SIZE.width, y: 946});

eq('kept off the top when the draft is taller than the room above',
    placeAbovePill({pill: {...PILL, y: 300}, size: {width: 300, height: 400}, workArea: WORK_AREA}),
    {x: 750, y: 32});
eq('a draft larger than the work area starts at its origin above a pill',
    placeAbovePill({pill: PILL, size: {width: 4000, height: 4000}, workArea: WORK_AREA}),
    {x: 0, y: 32});

// --- Its tail ----------------------------------------------------------------

eq('the tail points at the pill, from the bubble\'s bottom edge',
    tailAbovePill({pill: PILL, bubble: BUBBLE(PILL)}),
    {x: 800 + 100 - TAIL_SIZE / 2, y: 946 + SIZE.height - 1 - TAIL_SIZE / 2});
// The bubble against the left edge still has the pill's centre (100) over
// its flat bottom, so the tail stays pointed at the pill.
eq('the tail follows the pill when the bubble had to move left',
    tailAbovePill({pill: {...PILL, x: 0}, bubble: BUBBLE({...PILL, x: 0})}),
    {x: 100 - TAIL_SIZE / 2, y: 977});
// Against the right edge it no longer does: the tail stops at the corner.
const movedRight = BUBBLE({...PILL, x: 1820});
eq('the tail stops at the corner when the pill is past the bubble\'s edge',
    tailAbovePill({pill: {...PILL, x: 1820}, bubble: movedRight}),
    {x: movedRight.x + movedRight.width - BUBBLE_RADIUS - TAIL_SIZE, y: 977});
eq('a bubble too narrow for the tail keeps it at the corner it can reach',
    tailAbovePill({pill: PILL, bubble: {x: 700, y: 946, width: 20, height: 40}}),
    {x: 700 + BUBBLE_RADIUS, y: 977});

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
