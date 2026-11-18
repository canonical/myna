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
