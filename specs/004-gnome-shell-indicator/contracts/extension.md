# Contract: GNOME Shell extension (GJS) — the overlay host

**Feature**: 004-gnome-shell-indicator | **Date**: 2026-07-21 (HUD redesign: 2026-07-30; wave-ribbon: 2026-07-30; rewritten as host contract: 2026-08-26)

**(2026-08-26 architecture revision)** The extension no longer draws anything
and no longer consumes `org.myna.Dictation`. It is a **thin window-management
host** for the renderer application (`myna-hud`, Rust GTK4 — `contracts/`
sibling guarantees live in the publisher/renderer tasks; the rendering
guarantees X11–X31 previously listed here move to the renderer application's
own test suite in `client/myna-hud`): it launches the binary through the
compositor's Wayland-client API, adopts its window, makes it a focus-safe
overlay, positions it, supervises the process, and owns the `org.myna.Shell`
presence name (`contracts/dbus-interface.md` §Presence). The pure
mapping/rendering guarantees (formerly X1–X6, X19, X24–X26) are re-homed
verbatim as Rust unit tests of `myna-hud`'s ported pure modules; the
visual/focus acceptance guarantees (formerly X11–X18, X20–X23, X27–X31) remain
manual on-hardware acceptance items, now exercised through the hosted window.
The guarantees below are the **host's** own.

Harness-tier note: the host is GJS by platform necessity (an extension cannot
be another language), but it is a *thin shim* — launch/adopt/position/supervise
— with no drawing, no animation, and no dictation data crossing it. Its logic
is factored into pure modules so everything except live compositor behavior is
GJS-unit-tested without a Shell; the compositor behavior is verified by the
manual acceptance plus a headless-Shell integration test where available.

## Pure, unit-tested (GJS contract tests, no Shell required)

| # | Guarantee | Spec |
|---|---|---|
| XH1 | The bottom-center placement math maps (monitor work-area geometry, window size, bottom margin) → the window's target frame position; it centers horizontally, respects the work area's bottom edge, never produces off-screen coordinates for any tested monitor layout, and recomputes correctly when any input changes (monitors-changed / work-area change / window size change). | FR-004, FR-024 |
| XH2 | The launch-resolution order is `$MYNA_HUD_BINARY` (an absolute path to a locally built renderer) → `snap run myna.hud` (the packaged snap app, **2026-08-26**). There is no bare `/usr/bin` fallback: the renderer ships in the snap, and the packaged path launches *through* `snap run` (not by exec'ing a file) so snap-confine sets up the sandbox and the Wayland socket. A missing/unlaunchable renderer produces a bounded, non-spamming failure state (never a crash loop faster than the backoff floor, never an unhandled exception in `enable()`). | FR-027, FR-026 |
| XH3 | The respawn policy is a pure function of exit history: unexpected exit while enabled → restart after a bounded backoff (with a restart budget so a permanently-crashing binary stops being retried and the extension degrades to dormant, logging once); normal `disable()` → no restart. | FR-026, FR-021 |
| XH4 | Adoption is idempotent **per window, and happens on every map**: `window-created` events for windows the client does not own are ignored; a given window is adopted exactly once; a second window from the same client (a lab window, a dialog) is not adopted. The renderer **hides its window entirely at idle** (FR-002/X3 — the resting state is an absent HUD, not an empty one), so the surface legitimately comes and goes across sessions, and the host must adopt, dock-type and place each new one. It must do so before the window is first presented, so the pill never appears briefly at an unplaced position. This is the same path a respawn takes (XH3), not a special case. | FR-024, FR-002, FR-026 |
| XH5 | The presence-name lifecycle maps 1:1 onto `enable()`/`disable()`: name owned while enabled, released on disable, re-acquired on re-enable; owning fails soft (extension keeps hosting even if the bus is unavailable — presence is advisory, not load-bearing). | FR-017a, FR-021 |

## Lifecycle, tested against a stub bus / fake client

| # | Guarantee | Spec |
|---|---|---|
| XH6 | On `enable()` the extension acquires `org.myna.Shell`, spawns `myna-hud` via `Meta.WaylandClient.new_subprocess`, and begins supervision; on `disable()` it terminates the subprocess (or its window), releases the name, disconnects all signals, and clears all timers (no leaks, no orphans). | FR-021, FR-026 |
| XH7 | Re-`enable()` after `disable()` re-establishes cleanly (Shell restart / relogin): fresh spawn, fresh adoption, name re-acquired. | FR-021 |
| XH8 | If the subprocess exits while enabled, respawn follows XH3's policy; the extension never surfaces a user-facing error for this. | FR-026 |
| XH9 | On Shell shutdown the subprocess does not outlive the session (termination is requested; an orphaned window must not remain on screen). | FR-021 |

## Compositor behaviour (manual on-hardware acceptance; headless-Shell test where available)

| # | Guarantee | Spec |
|---|---|---|
| XH10 | The adopted window is dock-typed, hidden from dash/alt-tab/window lists, shown on all workspaces, kept above normal windows, and never takes keyboard focus on map — typing in the focused application is uninterrupted by the indicator appearing. | FR-001, FR-024, SC-001 |
| XH11 | The window is positioned bottom-center of the primary monitor's work area, follows monitor/work-area changes and the window's own size changes without flicker loops (programmatic moves do not re-trigger themselves). | FR-004 |
| XH12 | Clicks on the indicator pass through to the application underneath in **every** state (empty input region, client-side — R22). **(2026-08-26)** There is no longer any exception: the dismiss control is gone, the HUD takes no pointer input, and a critical error is cleared by the client publishing a new state. | FR-025, SC-015 |
| XH13 | The extension declares `shell-version: ["50", "51"]` and refuses to load elsewhere; both target mutter ABI generations (18/51) expose the host APIs it uses. | FR-020, SC-008 |

## Constraints

- No network; no audio capture; the host never reads, renders, logs, or
  persists dictation state, levels, or transcript content — its only bus
  surface is the member-less presence name (privacy, constitution V — FR-019).
- `metadata.json` declares `shell-version: ["50", "51"]`, a unique `uuid`, and
  no settings schema (no picker — Out of Scope).
- Bundle is directly loadable at
  `~/.local/share/gnome-shell/extensions/<uuid>/` (no build step); it carries
  no drawing assets (no CSS for the pill, no ribbon modules — those live in
  `myna-hud`).
- **(2026-08-26)** The former in-extension renderer files (`hud.js`,
  `hudLogic.js`, `view.js`, `states.js`, `vumeter.js`, `ribbon.js`,
  `ribbonPaint.js`, `ribbonGlsl.js`, `ribbonShader.js`, `accent.js`,
  `stylesheet.css`), the `dbus.js` proxy, the gettext shim, `dev-lab/`,
  `dev-lab-gpu/`, and their tests are **deleted outright** (spec Assumptions)
  — not retained behind a flag.
- The host MUST NOT depend on private Shell UI internals (no `OsdWindow`, no
  `Main.wm._checkDimming`-style injections): the permitted surface is the
  public extension API plus the mutter window/Wayland-client APIs named above.
