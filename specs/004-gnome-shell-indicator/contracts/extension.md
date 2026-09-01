# Contract: GNOME Shell extension (GJS) — the overlay host

**Feature**: 004-gnome-shell-indicator | **Date**: 2026-07-21 (HUD redesign: 2026-07-30; wave-ribbon: 2026-07-30; rewritten as host contract: 2026-08-26)

**(2026-08-26 architecture revision; RC- numbering: 2026-08-31)** The
extension no longer draws anything and no longer consumes
`com.canonical.Myna.Dictation`. It is a **thin window-management host** for
the renderer application (`myna-hud`, Rust GTK4 — `contracts/` sibling
guarantees live in the publisher/renderer tasks; the rendering
guarantees X11–X31 previously listed here move to the renderer application's
own test suite in `client/myna-hud`): it launches the binary through the
compositor's Wayland-client API, adopts its window, makes it a focus-safe
overlay, positions it, supervises the process.
The pure mapping/rendering guarantees (formerly X1–X6, X19, X24–X26; now
**RC1–RC6, RC19, RC24–RC26**) are re-homed verbatim as Rust unit tests of
`myna-hud`'s ported pure modules; the visual/focus acceptance guarantees
(formerly X11–X18, X20–X23, X27–X31) remain manual on-hardware acceptance
items, now exercised through the hosted window. The host is pure
window-management. The guarantees below are the **host's** own.

Harness-tier note: the host is GJS by platform necessity (an extension cannot
be another language), but it is a *thin shim* — launch/adopt/position/supervise
— with no drawing, no animation, and no dictation data crossing it. Its logic
is factored into pure modules so everything except live compositor behavior is
GJS-unit-tested without a Shell; the compositor behavior is verified by the
manual acceptance plus a headless-Shell integration test where available.

## Renderer contract (myna-hud, `client/myna-hud`)

The pure guarantees the standalone GTK4 renderer application upholds.
Every row here is encoded as a Rust unit test in `client/myna-hud/tests/`.
The `RC-` prefix disambiguates from the host contract (`XH-`,
above), the wire contract (`C-`, in `dbus-interface.md`), and the
research references (`R-`, in `research.md`).

| # | Guarantee | Spec |
|---|---|---|
| RC1 | Wire `State` strings map to a content-free semantic descriptor; unknown values pass through verbatim. | FR-008 |
| RC2 | The state→descriptor mapping is additive: a `State` value the renderer does not recognise is treated as the closest match, never as a crash. | FR-008 |
| RC3 | The idle `State` keeps the renderer dormant — no actor, no window content shown. | FR-002 |
| RC5 | `levels_to_intensity` is calibrated: louder inputs produce higher intensity monotonically across the speech range; ST levels decays toward a floor within `STALE_MS`; NaN inputs are safe. | FR-009, SC-004 |
| RC6 | No transcript content ever crosses a render boundary — only `State`, `StatusMessage`, `AudioRms`, `AudioPeak`. | constitution V |
| RC7 | The consumer is dormant while `com.canonical.Myna.Dictation` has no owner: no proxy, no `State` emission, no error surfaced. | FR-018, FR-026 |
| RC8 | Name-appeared connects the proxy and reflects the current `State`; name-vanished returns to idle. Levels are forwarded on every `PropertiesChanged` (never deduplicated — R16a). | FR-026 |
| RC9 | `disable()` removes the watch, drops the proxy, and disconnects every subscription (no leaks). | FR-026 |
| RC10 | Re-`enable()` after `disable()` re-establishes the watch and proxy cleanly. | FR-026 |
| RC19 | Severity maps to a content-free icon choice and pill colour class. | FR-005, FR-007 |
| RC20 | A held notice replaces in place — never queues. The replacement policy and the timing are deterministic. | FR-007a, FR-007b, FR-007d |
| RC21 | Recoverable notices auto-dismiss after their dynamic hold; critical errors persist until the publisher sends a new `State. | FR-007b, FR-007d |
| RC24 | The ribbon model composes layered strands (base/secondary/voice), is deterministic, and the `Morph` phase produces travelling dots. | FR-010 |
| RC25 | Accent resolution prefers the live theme (`@accent_bg_color`), then `AdwStyleManager:accent-color-rgba` when available, then a documented Ubuntu-orange fallback. | FR-005, R26 |
| RC26 | Reduced-motion resolution is absent-safe — the renderer never crashes when neither `GtkSettings:gtk-interface-reduced-motion` nor `org.gnome.desktop.a11y.interface` is readable; the modern property wins, and the legacy `enable-animations` setting (inverted) is the fallback. | R26, E2b |

## Pure, unit-tested (GJS contract tests, no Shell required)

| # | Guarantee | Spec |
|---|---|---|
| XH1 | The bottom-center placement math maps (monitor work-area geometry, window size, bottom margin) → the window's target frame position; it centers horizontally, respects the work area's bottom edge, never produces off-screen coordinates for any tested monitor layout, and recomputes correctly when any input changes (monitors-changed / work-area change / window size change). | FR-004, FR-024 |
| XH2 | The launch-resolution order is `$MYNA_HUD_BINARY` (an absolute path to a locally built renderer) → `snap run myna.hud` (the packaged snap app, **2026-08-26**). There is no bare `/usr/bin` fallback: the renderer ships in the snap, and the packaged path launches *through* `snap run` (not by exec'ing a file) so snap-confine sets up the sandbox and the Wayland socket. A missing/unlaunchable renderer produces a bounded, non-spamming failure state (never a crash loop faster than the backoff floor, never an unhandled exception in `enable()`). | FR-027, FR-026 |
| XH3 | The respawn policy is a pure function of exit history: unexpected exit while enabled → restart after a bounded backoff (with a restart budget so a permanently-crashing binary stops being retried and the extension degrades to dormant, logging once); normal `disable()` → no restart. | FR-026, FR-021 |
| XH4 | Adoption is idempotent **per window, and happens on every map**: `window-created` events for windows the client does not own are ignored; a given window is adopted exactly once; a second window from the same client (a lab window, a dialog) is not adopted. The renderer **hides its window entirely at idle** (FR-002/X3 — the resting state is an absent HUD, not an empty one), so the surface legitimately comes and goes across sessions, and the host must adopt, dock-type and place each new one. It must do so before the window is first presented, so the pill never appears briefly at an unplaced position. This is the same path a respawn takes (XH3), not a special case. | FR-024, FR-002, FR-026 |

## Lifecycle, tested against a stub bus / fake client

| # | Guarantee | Spec |
|---|---|---|
| XH6 | On `enable()` the extension spawns `myna-hud` via `Meta.WaylandClient.new_subprocess` and begins supervision; on `disable()` it terminates the subprocess (or its window), disconnects all signals, and clears all timers (no leaks, no orphans). | FR-021, FR-026 |
| XH7 | Re-`enable()` after `disable()` re-establishes cleanly (Shell restart / relogin): fresh spawn, fresh adoption, name re-acquired. | FR-021 |
| XH8 | If the subprocess exits while enabled, respawn follows XH3's policy; the extension never surfaces a user-facing error for this. | FR-026 |
| XH9 | On Shell shutdown the subprocess does not outlive the session (termination is requested; an orphaned window must not remain on screen). | FR-021 |

## Compositor behaviour (manual on-hardware acceptance; headless-Shell test where available)

| # | Guarantee | Spec |
|---|---|---|
| XH10 | The adopted window is dock-typed, hidden from dash/alt-tab/window lists, shown on all workspaces, kept above normal windows, and never takes keyboard focus on map — typing in the focused application is uninterrupted by the indicator appearing. **(2026-09-01)** The dock-typing happens at `window-created`, **before the first map**: mutter's focus-on-map decision reads the window type before the shell's `map` signal fires, so typing it in the map handler is too late and the first map steals focus while the window is still NORMAL. The map handler re-asserts the type and positions. | FR-001, FR-024, SC-001 |
| XH10a | The overlay stays visible when the **overview** opens (a dictation session may span opening the overview to find a window). The host reparents the window's actor into `Main.layoutManager.uiGroup` while the overview is showing and returns it to the window group when it hides. **(2026-08-26; needs on-hardware verification.)** | FR-001 |
| XH11 | The window is positioned bottom-center of the primary monitor's work area, follows monitor/work-area changes and the window's own size changes without flicker loops (programmatic moves do not re-trigger themselves). **(2026-08-26)** When an overlay dock (dash-to-dock in auto-hide mode) reserves the bottom of the primary monitor via `Main.layoutManager.dashToDockStruts`, the host shrinks the work area to sit above the dock's reserved extent, so the pill is never covered when the dock slides out; absent the object, placement falls back to the plain work area. | FR-004 |
| XH12 | Clicks on the indicator pass through to the application underneath in **every** state (empty input region, client-side — R22). **(2026-08-26)** There is no longer any exception: the dismiss control is gone, the HUD takes no pointer input, and a critical error is cleared by the client publishing a new state. | FR-025, SC-015 |
| XH13 | The extension declares `shell-version: ["46", "47", "48", "49", "50", "51"]`. It supports both trusted-client API generations: on Mutter 14–16 (GNOME Shell 46–48), `Meta.WaylandClient.new(global.context, launcher)` then `client.spawnv(global.display, argv)` launches the renderer and `client.make_dock(window)` / `client.hide_from_window_list(window)` configure it; on Mutter 17+ (GNOME Shell 49+), `Meta.WaylandClient.new_subprocess(...)` then `client.get_subprocess()` launches it and `window.set_type(Meta.WindowType.DOCK)` / `window.hide_from_window_list()` configure it. The branch is capability-based, not version-string-based. **(2026-09-01)** The extension only runs under Wayland: `Meta.is_wayland_compositor` is checked at `enable()` on the mutter generations that still support X11, and the probe is treated as absent (⇒ Wayland) on the X11-less Mutter 17+ — under X11 `this._host` is left unset and the extension does nothing, so the daemon falls back to desktop notifications instead of retrying `MetaWaylandClient` forever. | FR-020, SC-008 |

## Constraints

- No network; no audio capture; the host never reads, renders, logs, or
  persists dictation state, levels, or transcript content — it has no bus
  surface at all (privacy, constitution V — FR-019).
- `metadata.json` declares `shell-version: ["46", "47", "48", "49", "50", "51"]`,
  a unique `uuid`, and no settings schema (no picker — Out of Scope).
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
- Every change under `extensions/myna-shell/` MUST follow the feature's
  [`extension-best-practices.md`](../extension-best-practices.md) review
  baseline. The maintained upstream source is the
  [GJS Guide best-practices Markdown](https://gitlab.gnome.org/World/javascript/gjs-guide/-/raw/main/docs/extensions/review-guidelines/best-practices.md).
  Capability checks are permitted only for APIs that genuinely differ across
  the declared GNOME Shell 46–51 range (currently the trusted-client paths in
  XH13).
