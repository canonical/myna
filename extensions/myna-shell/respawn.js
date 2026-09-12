// respawn.js — PURE supervision policy for the renderer subprocess (feature
// 004-gnome-shell-indicator, 2026-08-26 architecture revision; FR-026;
// contract extension.md XH3). No Shell/gi imports — the caller owns the
// clock and the actual spawning, this decides only *whether* and *how long
// to wait* (test/respawn.test.js).
//
// The user must never have to relaunch the indicator by hand, but a
// permanently-crashing binary must not become a spawn loop either: the
// policy backs off, and after a bounded budget gives up and goes dormant
// (logging once), which is what "bounded, non-aggressive backoff" in
// FR-026 means.

/** First retry delay (ms); each subsequent retry doubles it. */
export const BASE_BACKOFF_MS = 500;
/** Never wait longer than this between retries (ms). */
export const MAX_BACKOFF_MS = 30000;
/** How many consecutive failures to tolerate before going dormant. */
export const RESTART_BUDGET = 5;
/** A process that stayed up at least this long counts as healthy, and
 * resets the budget: a crash after ten minutes of use is a fresh incident,
 * not a continuation of an old one. */
export const HEALTHY_UPTIME_MS = 60000;

/**
 * The supervision decision for an exit.
 *
 * @param {object} state - the caller's running tally.
 * @param {number} state.consecutiveFailures - failures since the last
 *     healthy run (start at 0).
 * @param {object} exitInfo
 * @param {boolean} exitInfo.expected - true when the host asked the process
 *     to stop (disable(), Shell shutdown) — never respawn those.
 * @param {number} exitInfo.uptimeMs - how long the process ran.
 * @returns {{restart: boolean, delayMs: number, consecutiveFailures: number,
 *     dormant: boolean}} the decision plus the tally to carry forward.
 */
export function planRestart(state, {expected, uptimeMs}) {
    if (expected) {
        return {
            restart: false,
            delayMs: 0,
            consecutiveFailures: 0,
            dormant: false,
        };
    }

    // A run that lasted long enough to be useful clears the tally, so
    // long-lived sessions never accumulate their way into dormancy.
    const priorFailures = uptimeMs >= HEALTHY_UPTIME_MS ? 0 : state.consecutiveFailures;
    const consecutiveFailures = priorFailures + 1;

    if (consecutiveFailures > RESTART_BUDGET) {
        return {
            restart: false,
            delayMs: 0,
            consecutiveFailures,
            dormant: true,
        };
    }

    const delayMs = Math.min(
        MAX_BACKOFF_MS, BASE_BACKOFF_MS * Math.pow(2, consecutiveFailures - 1));
    return {restart: true, delayMs, consecutiveFailures, dormant: false};
}

/** The tally a freshly-enabled host starts from. */
export function initialState() {
    return {consecutiveFailures: 0};
}
