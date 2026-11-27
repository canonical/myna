// draftPlace.js - where the draft of a dictation is drawn, and what it says.
// Pure (no gi imports), so it runs under plain gjs in test/.

/** Space between the caret and the draft. */
export const CARET_GAP = 6;

/** How far above the work area's bottom the draft sits with no caret to
 * follow: clear of the HUD pill, which is anchored there. */
export const FALLBACK_BOTTOM = 120;

/** The most text shown; a long dictation shows its end. */
export const DRAFT_LIMIT = 600;

function clamp(value, low, high) {
    return Math.max(low, Math.min(value, high));
}

/**
 * Place a draft of `size` in `workArea`: just below the text caret and
 * starting at it, above when there is no room below, and bottom-centre when
 * the focused field reports no caret (`caret` null or empty).
 *
 * @returns {{x: number, y: number}}
 */
export function placeDraft({caret, size, workArea}) {
    const right = workArea.x + workArea.width - size.width;
    const bottom = workArea.y + workArea.height - size.height;
    if (!caret || (caret.width <= 0 && caret.height <= 0)) {
        return {
            x: clamp(workArea.x + Math.floor((workArea.width - size.width) / 2),
                workArea.x, Math.max(workArea.x, right)),
            y: clamp(bottom - FALLBACK_BOTTOM, workArea.y, Math.max(workArea.y, bottom)),
        };
    }
    const below = caret.y + caret.height + CARET_GAP;
    const above = caret.y - CARET_GAP - size.height;
    const y = below <= bottom || above < workArea.y ? below : above;
    return {
        x: clamp(caret.x, workArea.x, Math.max(workArea.x, right)),
        y: clamp(y, workArea.y, Math.max(workArea.y, bottom)),
    };
}

/** Place a draft of `size` in the middle of `workArea`. */
export function placeCentre({size, workArea}) {
    return {
        x: workArea.x + Math.max(0, Math.floor((workArea.width - size.width) / 2)),
        y: workArea.y + Math.max(0, Math.floor((workArea.height - size.height) / 2)),
    };
}

/** Space between the HUD pill and a draft drawn above it. */
export const PILL_GAP = 14;

/** Side of the square, turned 45 degrees, that points at the pill. */
export const TAIL_SIZE = 16;

/** The bubble's corner radius. Its tail must stay on the flat bottom edge,
 * clear of the corners, or it reads as detached from it. */
export const BUBBLE_RADIUS = 20;

/**
 * Place a draft of `size` just above the HUD pill and centred on it, kept
 * inside `workArea`.
 */
export function placeAbovePill({pill, size, workArea}) {
    const right = workArea.x + workArea.width - size.width;
    const bottom = workArea.y + workArea.height - size.height;
    return {
        x: clamp(pill.x + Math.floor((pill.width - size.width) / 2),
            workArea.x, Math.max(workArea.x, right)),
        y: clamp(pill.y - PILL_GAP - size.height,
            workArea.y, Math.max(workArea.y, bottom)),
    };
}

/**
 * Place the tail of the bubble at `bubble`, pointing at the pill's centre.
 *
 * The square is centred on the bubble's bottom edge, 1px inside it, so the
 * two meet without a hairline seam. When the bubble had to move off the pill
 * to stay inside the work area, the tail slides along the flat bottom edge so
 * it follows the pill without leaving the bubble.
 *
 * @returns {{x: number, y: number}} the square's top-left, before rotation.
 */
export function tailAbovePill({pill, bubble}) {
    const left = bubble.x + BUBBLE_RADIUS;
    const right = bubble.x + bubble.width - BUBBLE_RADIUS - TAIL_SIZE;
    const centre = pill.x + pill.width / 2;
    return {
        x: Math.round(clamp(centre - TAIL_SIZE / 2, left, Math.max(left, right))),
        y: Math.round(bubble.y + bubble.height - 1 - TAIL_SIZE / 2),
    };
}

/** The end of `text`, cut at a word and marked, when it is over `limit`. */
export function draftEnd(text, limit = DRAFT_LIMIT) {
    const chars = Array.from(text);
    if (chars.length <= limit)
        return text;
    const start = chars.length - limit;
    const end = chars.slice(start).join('');
    const space = chars[start - 1] === ' ' ? -1 : end.indexOf(' ');
    return `…${end.slice(space + 1)}`;
}

function escape(text) {
    return text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

/**
 * Pango markup for a draft: the settled text, then the recogniser's current
 * guess dimmed. Empty when there is nothing to show.
 */
export function draftMarkup(stable, tail, limit = DRAFT_LIMIT) {
    const joined = stable && tail ? `${stable} ${tail}` : stable + tail;
    if (!joined.trim())
        return '';
    const shown = draftEnd(joined, limit);
    const guess = Math.min(tail.length, shown.length);
    const settled = shown.slice(0, shown.length - guess);
    const dimmed = shown.slice(shown.length - guess);
    return escape(settled) +
        (dimmed ? `<span alpha="55%">${escape(dimmed)}</span>` : '');
}
