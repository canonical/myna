# Contract: GNOME Shell extension (GJS)

**Feature**: 004-gnome-shell-indicator | **Date**: 2026-07-21 (HUD redesign: 2026-07-30)

The consumer half: `extensions/myna-shell/`. Harness-tier (Complexity Tracking) —
the **pure mapping/lifecycle** guarantees below are GJS-unit-tested against a stub;
the **compositor/animation/focus** guarantees are verified by the manual
on-hardware acceptance (`quickstart.md`). Consumes `org.myna.Dictation`
(`contracts/dbus-interface.md`). **(2026-07-30)**: the "goop"/`RibbonView`
presentation is replaced by a bottom-center HUD pill (`hud.js`); `states.js`'s
descriptor is reshaped from `{key, statusText, isError, hidden}` to `{key,
statusText, severity, hidden}` (data-model E1a).

## Pure, unit-tested (GJS contract test, `test/states.test.js`)

| # | Guarantee | Spec |
|---|---|---|
| X1 | `states.js` maps every known `State` to its visual-intent record (icon choice + severity + a11y label) per data-model E-mapping. | FR-005 |
| X2 | An **unknown** `State` maps to the neutral "active" intent (`severity: null`) and never throws. | FR-008 |
| X3 | `idle` maps to "hidden" (no actor). | FR-002 |
| X4 | `loading` and `recording` map to distinct intents (FR-006). | FR-006 |
| X5 | `vumeter.js` maps RMS+peak `[0,1]` to a VU intensity monotonically (in the calibrated speech range), clamps out-of-range/NaN, and **decays to floor** when the last update is older than the stale window (~300 ms) — regardless of whether the repeated value is numerically identical to the previous one (R16a). | FR-010/011, SC-004 |
| X6 | No mapping output contains transcript text — inputs are state + level only (privacy). | constitution V |
| X19 | **(2026-07-30)** `notice` maps to `severity: 'recoverable'` and `error` maps to `severity: 'critical'`; the two are mutually exclusive and each carries a distinct icon choice (mic-with-slash for `critical` only). | FR-007, data-model E1a |
| X23 | **(2026-07-30, R16a)** `segmentColor(position)` returns `'green'` below the yellow threshold, `'yellow'` below the red threshold, and `'red'` at/above it — a conventional VU colour zoning by position, not by raw level. | FR-010 |

## Lifecycle, tested against a stub proxy

| # | Guarantee | Spec |
|---|---|---|
| X7 | On `enable()` with the name absent, the extension stays dormant (no actor, no error surfaced). | FR-018, US1-5 |
| X8 | On name-appeared it connects and reflects the current `State`; on name-vanished it clears the HUD pill to idle. | FR-018, edge cases |
| X9 | On `disable()` it disconnects the proxy, removes the name watch, destroys actors, and cancels all timers/transitions (no leaks). | FR-021 |
| X10 | Re-`enable()` after `disable()` re-establishes cleanly (Shell restart / relogin). | FR-021, edge cases |
| X20 | **(2026-07-30)** A second `notice` while one is showing replaces the held reason in place and **restarts** the auto-dismiss timer in full; a second `error` while one is undismissed replaces the reason in place without waiving or restarting the dismiss requirement (no timer exists for `error`). Neither stacks or queues a second concurrent notice. | FR-007a/FR-007d, R15 |

## Compositor behaviour, manual on-hardware acceptance

| # | Guarantee | Spec |
|---|---|---|
| X11 | The HUD pill is added as Shell chrome and **never takes keyboard focus**: typing continues to land in the focused app while it is visible — including when the critical-error dismiss (×) control is clicked. | FR-001/FR-007c, SC-001 |
| X12 | It becomes visible within the activation-latency target after `recording` and clears within the teardown target after `idle`. | FR-003, SC-003 |
| X13 | Each state/severity shows a visually distinct treatment; a viewer can identify loading/listening/transcribing/finalizing/recoverable-notice/critical-error without seeing transcript. | FR-005/006/007, SC-002 |
| X14 | The segmented VU meter tracks captured level (calibrated to real speech, not raw linear gain — R16a) and eases to floor on silence/stale; nothing shown when idle. | FR-010/011, SC-004 |
| X15 | Animations look smooth (≈60 fps) and don't accumulate across rapid start/stop cycles. | FR-009, SC-007 |
| X16 | The optional panel button (if enabled) toggles a session equivalently to the hotkey, preserving commit-only behaviour, and dims when the daemon is absent. | FR-013/014, SC-010 |
| X17 | The HUD pill is legible in high-contrast mode. (Screen-reader/AT-SPI announcement of state transitions is tracked separately as T56 — not a guarantee of this contract.) | FR-022 |
| X18 | Loads on GNOME 50/51 (per `metadata.json` `shell-version`) and refuses to load on unsupported versions. | FR-020, SC-008 |
| X21 | **(2026-07-30)** The HUD pill renders bottom-center of the primary monitor (matching GNOME's native volume/brightness OSD position), repositioning correctly across `monitors-changed`, and does not appear off-screen on any tested monitor layout. | FR-004 |
| X22 | **(2026-07-30)** The critical-error pill's dismiss (×) control is clickable with the mouse and clears the notice immediately; it never receives keyboard focus at any point. | FR-007b/FR-007c |

## Constraints

- No network; no audio capture; renders/logs/persists no transcript content
  (privacy, constitution V — FR-019).
- `metadata.json` declares `shell-version: ["50", "51"]`, a unique `uuid`, and no
  settings schema (no picker — Out of Scope).
- Bundle is directly loadable at
  `~/.local/share/gnome-shell/extensions/<uuid>/` (no build step).
- **(2026-07-30)** `RibbonView`/`indicator.js` is deleted, not retained as a
  selectable alternate view (spec Assumptions).

