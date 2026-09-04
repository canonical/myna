// resolve.js — PURE renderer-launch resolution (feature
// 004-gnome-shell-indicator, 2026-08-26 architecture revision; research
// R21/R27; contract extension.md XH2). No Shell/gi imports: the caller
// supplies the environment and an "is this executable?" predicate, so the
// order and the failure states are unit-testable headless
// (test/resolve.test.js).
//
// The host launches the renderer with no discovery protocol, on purpose:
//
//   1. `$MYNA_HUD_BINARY` — the developer override. An absolute path to a
//      build tree's binary (`…/target/debug/myna-hud`); when set and
//      executable it wins over everything, so a packaged snap can never
//      shadow the build under test.
//   2. `snap run myna.hud` — the packaged command (R27/FR-027). The snap
//      exposes the renderer as the app `myna.hud`; it is launched THROUGH
//      `snap run`, not by exec'ing a path, so snap-confine sets up the
//      sandbox and passes the Wayland socket the child needs. (The dotted
//      `myna.hud` is the snap app name — `<snap>.<app>` — not a filesystem
//      path.)
//
// There is deliberately no bare `/usr/bin/myna-hud` fallback: the renderer
// ships in the snap, and a stray system binary would be an unconfined,
// unversioned surprise rather than a supported install.

/** The environment variable that overrides the launch entirely — an
 * absolute path to a locally built renderer. */
export const OVERRIDE_ENV = 'MYNA_HUD_BINARY';

/** The launcher used for the packaged renderer, and the snap app it runs. */
export const SNAP_LAUNCHER = 'snap';
export const SNAP_APP = 'myna.hud';

/**
 * Resolve the argv that launches the renderer.
 *
 * @param {object} deps
 * @param {function(string): (string|null)} deps.getenv - environment lookup.
 * @param {function(string): boolean} deps.isExecutable - existence +
 *     executability predicate. Used for the override path and to confirm
 *     `snap` itself is present (looked up on `PATH` by the caller's
 *     predicate, or as `/usr/bin/snap`).
 * @returns {{argv: (string[]|null), source: string}} the launch argv and
 *     where it came from (`'override'`, `'snap'`, or `'missing'`).
 *     `argv` is null only when nothing can be launched — the host then
 *     stays dormant and logs once, rather than throwing (XH2).
 */
export function resolveHudLaunch({getenv, isExecutable}) {
    const override = getenv(OVERRIDE_ENV);
    if (override !== null && override !== undefined && override !== '') {
        // An override that does not exist is a *configuration error*, not a
        // reason to silently fall back to the packaged snap the developer
        // did not ask for: report it missing so the host logs the path the
        // user actually set.
        return isExecutable(override)
            ? {argv: [override], source: 'override'}
            : {argv: null, source: 'missing'};
    }
    // The packaged path: `snap run myna.hud`. We can only confirm the
    // launcher exists, not stat the confined app — but if `snap` is present
    // the snap is the supported install, and `snap run` reports its own
    // clean error if the app is absent (which the exit-watch treats as a
    // failed launch, XH3).
    if (isExecutable(SNAP_LAUNCHER) || isExecutable(`/usr/bin/${SNAP_LAUNCHER}`)) {
        return {argv: [SNAP_LAUNCHER, 'run', SNAP_APP], source: 'snap'};
    }
    return {argv: null, source: 'missing'};
}
