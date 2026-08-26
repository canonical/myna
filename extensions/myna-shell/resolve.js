// resolve.js — PURE renderer-binary resolution (feature
// 004-gnome-shell-indicator, 2026-08-26 architecture revision; research
// R21/R27; contract extension.md XH2). No Shell/gi imports: the caller
// supplies the environment and an "is this executable?" predicate, so the
// order and the failure states are unit-testable headless
// (test/resolve.test.js).
//
// The host launches the renderer by a well-known path (FR-027) — there is
// no discovery protocol, on purpose: the snap ships the binary as a snap
// command, distributions ship it in the system path, and a developer points
// the override at a build tree.

/** Resolution order (first hit wins). `$MYNA_HUD_BINARY` is the developer
 * override — a cargo target dir during development; `/snap/bin/myna-hud` is
 * the packaged command (R27); `/usr/bin/myna-hud` is the distribution path. */
export const CANDIDATE_PATHS = ['/snap/bin/myna-hud', '/usr/bin/myna-hud'];

/** The environment variable that overrides the search entirely. */
export const OVERRIDE_ENV = 'MYNA_HUD_BINARY';

/**
 * Resolve the renderer binary.
 *
 * @param {object} deps
 * @param {function(string): (string|null)} deps.getenv - environment lookup.
 * @param {function(string): boolean} deps.isExecutable - existence +
 *     executability predicate.
 * @returns {{path: (string|null), source: string}} the resolved path and
 *     where it came from (`'override'`, `'candidate'`, or `'missing'`).
 *     `path` is null only when nothing was found — the host then stays
 *     dormant and logs once, rather than throwing (XH2).
 */
export function resolveHudBinary({getenv, isExecutable}) {
    const override = getenv(OVERRIDE_ENV);
    if (override !== null && override !== undefined && override !== '') {
        // An override that does not exist is a *configuration error*, not a
        // reason to silently fall back to a packaged binary the developer
        // did not ask for: report it as missing so the host logs the path
        // the user actually set.
        return isExecutable(override)
            ? {path: override, source: 'override'}
            : {path: null, source: 'missing'};
    }
    for (const candidate of CANDIDATE_PATHS) {
        if (isExecutable(candidate))
            return {path: candidate, source: 'candidate'};
    }
    return {path: null, source: 'missing'};
}
