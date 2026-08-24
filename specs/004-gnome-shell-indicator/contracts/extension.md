# Contract: GNOME Shell extension (GJS)

**Feature**: 004-gnome-shell-indicator | **Date**: 2026-07-21 (HUD redesign: 2026-07-30; wave-ribbon: 2026-07-30)

The consumer half: `extensions/myna-shell/`. Harness-tier (Complexity Tracking) —
the **pure mapping/lifecycle** guarantees below are GJS-unit-tested against a stub;
the **compositor/animation/focus** guarantees are verified by the manual
on-hardware acceptance (`quickstart.md`). Consumes `org.myna.Dictation`
(`contracts/dbus-interface.md`). **(2026-07-30)**: the "goop"/`RibbonView`
presentation is replaced by a bottom-center HUD pill (`hud.js`); `states.js`'s
descriptor is reshaped from `{key, statusText, isError, hidden}` to `{key,
statusText, severity, hidden}` (data-model E1a). **(2026-07-30, R17)**: the
segmented bar meter (`BarMeterActor`) is replaced by a flowing wave ribbon
(`WaveRibbonActor`), colored from the desktop's accent-color preference (E2a)
with a reduced-motion fallback (E2b) — both sourced locally from GSettings, not
the D-Bus contract, which is unchanged.

## Pure, unit-tested (GJS contract test, `test/states.test.js`)

| # | Guarantee | Spec |
|---|---|---|
| X1 | `states.js` maps every known `State` to its visual-intent record (icon choice + severity + a11y label) per data-model E-mapping. | FR-005 |
| X2 | An **unknown** `State` maps to the neutral "active" intent (`severity: null`) and never throws. | FR-008 |
| X3 | `idle` maps to "hidden" (no actor). | FR-002 |
| X4 | `loading` and `recording` map to distinct intents (FR-006). | FR-006 |
| X5 | `ribbon.js`'s envelope smoothing (reusing `vumeter.js`'s `boostLevel`/stale-decay unchanged) maps RMS+peak `[0,1]` to an intensity monotonically (in the calibrated speech range), clamps out-of-range/NaN, and **decays to floor** when the last update is older than the stale window (~300 ms) — regardless of whether the repeated value is numerically identical to the previous one (R16a). | FR-010/011, SC-004 |
| X6 | No mapping output contains transcript text — inputs are state + level only (privacy). | constitution V |
| X19 | **(2026-07-30)** `notice` maps to `severity: 'recoverable'` and `error` maps to `severity: 'critical'`; the two are mutually exclusive and each carries a distinct icon choice (mic-with-slash for `critical` only). | FR-007, data-model E1a |
| X24 | **(2026-07-30, R17/R17a)** `ribbon.js` generates layered strands (`base`/`voice`/`secondary` roles, 3-5 total) from a single SMOOTHED envelope value (a ~300 ms one-pole low-pass over the calibrated instantaneous envelope, `applyEnvelopeSmoothing`) with fixed per-strand phase/delay/amplitude offsets, deterministically (same smoothed envelope + elapsed time → same control points), and each of the lifecycle phases (unfold/flow/morph/complete) is a pure, independently-callable timing function. **(2026-08-24)** Was 5 phases: `relax` is removed. It was never reachable — `ribbonPhaseForStateKey` never returned it and `hud.js` never selected it — and a pause detector to reach it has no safe threshold (it fires on ordinary inter-word gaps at ~400 ms, and by ~1.5 s the envelope's `RELEASE_TAU_MS` release has already eased the wave to within a hair of it). FR-010a's pause behaviour is delivered by those release ballistics instead, continuously and in proportion to the audio; `ribbon.test.js` asserts it there. | FR-010/FR-010a |
| X25 | **(2026-07-30, R18)** `accent.js` resolves a chosen accent-color name to a derived palette (main/highlight/darker-complement/translucent) via the fixed 9-entry hex table — the darker-complement tone is a computed colour complement, **except for orange, whose darker-complement is a fixed aubergine tone** — and falls back to the fixed Ubuntu-orange palette when the resolved user-value is `null` (never actively set, including the untouched default) or the schema/key is absent — never throwing. | FR-010b |
| X26 | **(2026-07-30, R19)** The reduced-motion query resolves to a boolean without throwing when the schema/key is absent (defaults to full motion in that case). | FR-022a |

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
| X14 | **(2026-07-30, R17)** The wave ribbon tracks captured level (calibrated to real speech, not raw linear gain — R16a), unfolds on start, relaxes toward a thin idle line on pause/stale, morphs into a simplified processing motion on stop, and shows nothing when idle. | FR-010/010a/011, SC-004 |
| X15 | Animations look smooth (≈60 fps) and don't accumulate across rapid start/stop cycles. | FR-009, SC-007 |
| X16 | The optional panel button (if enabled) toggles a session equivalently to the hotkey, preserving commit-only behaviour, and dims when the daemon is absent. | FR-013/014, SC-010 |
| X17 | The HUD pill is legible in high-contrast mode. (Screen-reader/AT-SPI announcement of state transitions is tracked separately as T56 — not a guarantee of this contract.) | FR-022 |
| X18 | Loads on GNOME 50/51 (per `metadata.json` `shell-version`) and refuses to load on unsupported versions. **(2026-08-24)** On 50 this requires the ribbon to fall back to Cairo: `Clutter.ShaderEffect`'s snippet vfunc only exists from mutter 51.alpha, and registering the subclass eagerly would abort the extension's `import`. See `ribbonShaderSupported()`. | FR-020, SC-008 |
| X21 | **(2026-07-30)** The HUD pill renders bottom-center of the primary monitor (matching GNOME's native volume/brightness OSD position), repositioning correctly across `monitors-changed`, and does not appear off-screen on any tested monitor layout. | FR-004 |
| X22 | **(2026-07-30)** The critical-error pill's dismiss (×) control is clickable with the mouse and clears the notice immediately; it never receives keyboard focus at any point. | FR-007b/FR-007c |
| X27 | **(2026-07-30, R17/R18)** The ribbon is visibly rendered in the user's chosen system accent color (verified across at least 3 chosen colors) or the fixed Ubuntu-orange default when none is actively chosen (including the untouched default), and re-colors live if the accent color is changed while a session is active. | FR-010b, SC-011 |
| X28 | **(2026-07-30, R19)** With the system reduced-motion preference enabled, the HUD pill shows the static/minimal-motion alternative instead of the flowing ribbon, while still reflecting state/level. | FR-022a, SC-012 |
| X29 | **(new)** On a session that completes successfully, the ribbon briefly shows a quiet success indication before the pill clears, and this never delays the pill's dismissal or a new session starting. | FR-010d |
| X30 | **(2026-07-30, R17a)** During a `morph` phase (transcribing), the ribbon crossfades from the flowing wave into 3 travelling dots rather than switching abruptly. During `complete`, it converges toward a single centred point with a brightness pulse. | FR-010a, FR-010d |
| X31 | **(2026-07-30, R17a)** A recoverable notice keeps the ribbon **visible**, tinted amber (matching the pill's existing amber treatment) with audio-reactivity paused (a gentle idle pulse, not frozen), rather than hidden; a critical error still hides the ribbon entirely. | FR-010e, SC-014 |

## Constraints

- No network; no audio capture; renders/logs/persists no transcript content
  (privacy, constitution V — FR-019).
- `metadata.json` declares `shell-version: ["50", "51"]`, a unique `uuid`, and no
  settings schema (no picker — Out of Scope).
- Bundle is directly loadable at
  `~/.local/share/gnome-shell/extensions/<uuid>/` (no build step).
- **(2026-07-30)** `RibbonView`/`indicator.js` is deleted, not retained as a
  selectable alternate view (spec Assumptions).
- **(2026-07-30, R17/R20)** `extensions/myna-shell/dev-lab/` (a standalone
  GTK4/libadwaita app sharing `accent.js`/`ribbon.js`/`ribbon-paint.js`/`dbus.js`
  with the extension) is **not** part of this bundle: excluded from
  `metadata.json`'s file set and the install step in `quickstart.md` step 4;
  it carries none of this contract's guarantees as its own obligations.

