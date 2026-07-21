# Contract: GNOME Shell extension (GJS)

**Feature**: 004-gnome-shell-indicator | **Date**: 2026-07-21

The consumer half: `extensions/myna-shell/`. Harness-tier (Complexity Tracking) —
the **pure mapping/lifecycle** guarantees below are GJS-unit-tested against a stub;
the **compositor/animation/focus** guarantees are verified by the manual
on-hardware acceptance (`quickstart.md`). Consumes `org.myna.Dictation`
(`contracts/dbus-interface.md`).

## Pure, unit-tested (GJS contract test, `test/states.test.js`)

| # | Guarantee | Spec |
|---|---|---|
| X1 | `states.js` maps every known `State` to its visual-intent record (colour class + animation + a11y label) per data-model E-mapping. | FR-005 |
| X2 | An **unknown** `State` maps to the neutral "active" intent and never throws. | FR-008 |
| X3 | `idle` maps to "hidden" (no actor). | FR-002 |
| X4 | `loading` and `recording` map to distinct intents (FR-006). | FR-006 |
| X5 | `vumeter.js` maps a level `[0,1]` to a glow intensity monotonically, clamps out-of-range, and **decays to floor** when the last update is older than the stale window (~300 ms). | FR-010/011, SC-004 |
| X6 | No mapping output contains transcript text — inputs are state + level only (privacy). | constitution V |

## Lifecycle, tested against a stub proxy

| # | Guarantee | Spec |
|---|---|---|
| X7 | On `enable()` with the name absent, the extension stays dormant (no actor, no error surfaced). | FR-018, US1-5 |
| X8 | On name-appeared it connects and reflects the current `State`; on name-vanished it clears the goop to idle. | FR-018, edge cases |
| X9 | On `disable()` it disconnects the proxy, removes the name watch, destroys actors, and cancels all timers/transitions (no leaks). | FR-021 |
| X10 | Re-`enable()` after `disable()` re-establishes cleanly (Shell restart / relogin). | FR-021, edge cases |

## Compositor behaviour, manual on-hardware acceptance

| # | Guarantee | Spec |
|---|---|---|
| X11 | The goop is added as Shell chrome and **never takes keyboard focus**: typing continues to land in the focused app while it is visible. | FR-001, SC-001 |
| X12 | It becomes visible within the activation-latency target after `recording` and clears within the teardown target after `idle`. | FR-003, SC-003 |
| X13 | Each state shows a visually distinct treatment; a viewer can identify loading/listening/transcribing/finalizing/error without seeing transcript. | FR-005/006/007, SC-002 |
| X14 | The glow/VU tracks captured level and eases to floor on silence/stale; nothing shown when idle. | FR-010/011, SC-004 |
| X15 | Animations look smooth (≈60 fps) and don't accumulate across rapid start/stop cycles. | FR-009, SC-007 |
| X16 | The optional panel button (if enabled) toggles a session equivalently to the hotkey, preserving commit-only behaviour, and dims when the daemon is absent. | FR-013/014, SC-010 |
| X17 | State changes are announced by Orca and the goop is legible in high-contrast mode. | FR-022, SC-009 |
| X18 | Loads on GNOME 50/51 (per `metadata.json` `shell-version`) and refuses to load on unsupported versions. | FR-020, SC-008 |

## Constraints

- No network; no audio capture; renders/logs/persists no transcript content
  (privacy, constitution V — FR-019).
- `metadata.json` declares `shell-version: ["50", "51"]`, a unique `uuid`, and no
  settings schema (no picker — Out of Scope).
- Bundle is directly loadable at
  `~/.local/share/gnome-shell/extensions/<uuid>/` (no build step).
