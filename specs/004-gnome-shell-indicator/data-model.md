# Phase 1 Data Model: GNOME Shell Extension for Myna Dictation UI

**Feature**: 004-gnome-shell-indicator | **Date**: 2026-07-21 (HUD redesign: 2026-07-30)

The entities crossing the extension↔`myna-desktop` seam and the extension's own
transient state. Nothing here is persisted; audio is never represented (only
derived level). Cross-refs to spec Key Entities and the D-Bus contract
(`contracts/dbus-interface.md`).

## E1 — DictationState (the wire state string)

The single source of truth for the HUD pill's treatment. String enum on the
`org.myna.Dictation.State` property (updates pushed via `PropertiesChanged`).

| Value | Meaning | Derived in myna-desktop from |
|---|---|---|
| `idle` | No session; HUD pill hidden. | `IndicatorState::Hidden` |
| `loading` | Cold model load in progress (distinct treatment, FR-006). | `OrchestratorEvent::Loading` seen, `Ready` not yet |
| `recording` | Capturing / listening. | `IndicatorState::Recording` (post-`Ready`) |
| `transcribing` | Inference decoding. | `IndicatorState::Transcribing` |
| `finalizing` | Release seen; awaiting terminal transcript. | `IndicatorState::Finalizing` |
| `notice` | **(2026-07-30)** Session completed with an empty/blank transcript — a recoverable, non-blocking issue (e.g. "no speech detected"). | `IndicatorState::Error{recoverable: true, ..}`, from `completion_indicator_state()` on an empty `SessionOutcome::Completed{transcript}` |
| `error` | Failure / secure-field refusal — a critical, persistent issue. | `IndicatorState::Error{recoverable: false, ..}` |

Rules:
- Additive/forward-compatible: an **unknown** value (including `notice` seen by
  an unpatched extension build) MUST map to a neutral "active" visual intent on
  the extension side, never an exception (spec FR-008, R3).
- Privacy: the value is a state label only — never transcript text (constitution
  V; the `Error`/`Notice` message is a user-facing reason string, still
  content-free). The empty-transcript check that produces `notice` happens
  server-side in `myna-desktop`; only the boolean outcome (as a `State` value)
  crosses the bus.
- Legal transitions mirror the controller's audited `DictationState` model
  (`controller.rs`): idle→loading|recording, loading→recording|error|idle,
  recording→transcribing|finalizing|error|idle, transcribing→recording|finalizing|
  error|idle, finalizing→idle|error|notice, notice→idle, error→idle. The
  extension does **not** enforce legality (it renders whatever arrives,
  degrading unknowns); the publisher is the authority.
- `notice` and `error` are mutually exclusive per transition (a single
  `IndicatorState::Error{recoverable, ..}` maps to exactly one of the two wire
  values, never both) — see E1a (Severity) and R13.

## E1a — Severity (recoverable vs. critical, 2026-07-30)

Realized as the choice between the `notice` and `error` wire state values
themselves (E1) — not a separate D-Bus property. Backed in Rust by
`IndicatorState::Error { message: String, recoverable: bool }`.

| Severity | Wire `State` | UX treatment | Auto-dismiss? | Wave ribbon (2026-07-30, R17a) |
|---|---|---|---|---|
| `recoverable` | `notice` | Non-blocking notice; a new session may start while it's showing | Yes — ~3.5 s hold, restarts in full if a new recoverable notice arrives while one is showing (R15) | **Visible**, tinted amber, audio-reactivity paused (gentle idle pulse) — not hidden |
| `critical` | `error` | Persistent notice with a dismiss (×) control | No — remains until the user dismisses it; a new critical error while one is undismissed replaces the reason in place without waiving the dismiss requirement (R15) | Hidden (the pill's icon/border/message carry the state instead) |

This is an **interim, client-inferred classification** (spec Assumptions;
research R13) pending T31/T62's wire-level error taxonomy — not itself that
taxonomy.

## E2 — AudioLevel (the wave-ribbon input)

| Field | Type / range | Source | Notes |
|---|---|---|---|
| `AudioRms` | `d`, `[0.0, 1.0]` linear full-scale | `AudioStats.rms` | drives the wave ribbon's envelope/strand generation (dominant input, stable) |
| `AudioPeak` | `d`, `[0.0, 1.0]` linear full-scale | `AudioStats.peak` | blended in at reduced weight so transients/consonants are visible without pinning the meter |

Rules:
- Published only while a session is active; `0.0` when `idle` (spec FR-011).
- Updated at ~15–20 Hz (R5); the extension applies **stale-decay** to floor if no
  update arrives within ~300 ms (spec FR-011 / SC-004), based on **arrival
  time**, not value — a repeated identical RMS/peak still counts as fresh
  (R16a; a 2026-07-30 manual-test regression found the extension had been
  dropping "unchanged" updates and treating a steady voice as stale).
- **(2026-07-30, R17 — wave-ribbon redesign)**: RMS+peak are combined through
  the same hardware-calibrated dBFS scale R16a established
  (`ribbon.js`'s envelope smoothing, reusing `vumeter.js`'s `boostLevel`/
  stale-decay unchanged), then used to generate ~3 strands × 12–20 control
  points each (small per-strand phase/delay/amplitude offsets off the *same*
  envelope value — never independent per-strand state) painted by the shared
  `ribbon-paint.js`. Replaces R16's left-to-right segmented-bar rendering of
  the same underlying intensity value; the envelope math itself (R16a's
  calibration + stale-decay-by-arrival-time) is unchanged and reused verbatim.
- **(2026-07-30, R17a — "fabric in gentle airflow" refinement)**: a SECOND
  smoothing stage sits between the calibrated instantaneous envelope above
  and the wave shape: `ribbon.js`'s `applyEnvelopeSmoothing`, a one-pole
  low-pass with a ~300 ms time constant (`SMOOTHING_TAU_MS`, 250-400 ms
  design range), maintained as caller-owned state across repaint frames
  (same pattern as phase/phaseStartedAt) so `ribbon.js` itself stays a pure
  function of its inputs. This is what keeps the ribbon reading as a
  smoothed, controlled interpretation of loudness rather than a literal,
  oscilloscope-like reproduction of the envelope tick-by-tick.
- Carries energy only — never samples, never content (constitution V, R5).

## E2a — AccentColorPreference (extension-side, sourced from the desktop, 2026-07-30)

| Field | Type | Source | Notes |
|---|---|---|---|
| chosen | `string \| null` | `Gio.Settings.get_user_value('accent-color')` on `org.gnome.desktop.interface`, schema/key-existence guarded (R18) | `null` only when never actively written by the user — including the untouched factory default (itself `'blue'`) |
| resolvedColor | derived palette (main / highlight / darker-complement / translucent secondary) | 9-entry libadwaita hex table (R18) keyed by `chosen`, or a fixed Ubuntu-orange (`#E95420`) fallback when `chosen == null` or the schema/key is absent | drives the ribbon's strand colors in `ribbon-paint.js`. The darker-complement tone is a computed colour complement of the main colour, **except when the main colour is orange, where it is a fixed aubergine tone** (matching the reference design decision) rather than a generic computed complement |

Rules:
- Read live via `changed::accent-color`, so an in-session accent-color change
  re-colors the ribbon without restart.
- Sourced entirely from the desktop environment, not from `myna-desktop` or
  the D-Bus contract — no wire change (data-model E2/dbus-interface.md
  unaffected).
- Safe on pre-GNOME-47 shells (schema/key absent): degrades to the same
  Ubuntu-orange fallback, never an exception (R18).

## E2b — MotionPreference (extension-side, sourced from the desktop, 2026-07-30)

| Field | Type | Source | Notes |
|---|---|---|---|
| reducedMotion | `boolean` | `org.gnome.desktop.interface`'s `enable-animations` (inverted), schema/key-existence guarded (R19) | when `true`, the ribbon renders a static level line / gently-scaling mic indicator instead of the flowing wave (FR-022a) |

Rules:
- Read live via `changed::enable-animations`, same pattern as E2a.
- Drives only the *rendering* choice (static vs. flowing) — the underlying
  level/state inputs (E2) are unaffected either way (FR-022a: "still conveys
  state and level").

## E3 — ErrorReason (optional)

A short, user-facing reason string carried by the `ErrorMessage` property
when `State == error` **or `State == notice`** (2026-07-30: broadened —
previously `error`-only) — e.g. "no text field is focused", "refusing to type
into a password field", "inference backend unavailable" (critical/`error`), or
"No speech detected" (recoverable/`notice`). Empty when neither. Content-free
(never transcript). Sourced from the controller's existing
`IndicatorState::Error{message, ..}` / `abort_before_capture` messages, and
(2026-07-30) from `completion_indicator_state()`'s fixed reason for the
empty-transcript case.

## E4 — IndicatorSurface (extension-side, transient)

The extension's in-memory view; not on the wire.

| Aspect | Description |
|---|---|
| current state | last `DictationState` received (default `idle`) |
| current level | last `AudioLevel` + a timestamp (for stale-decay) |
| HUD pill actor | the bottom-center `St.Widget` (2026-07-30: replaces the top-of-panel ribbon/goop) added via `Main.layoutManager.addChrome`; exists only while state ≠ `idle`; its level sub-actor is `WaveRibbonActor` (2026-07-30, R17 — replaces `BarMeterActor`), painted via the shared `ribbon-paint.js` |
| held notice slot | **(2026-07-30)** one severity-scoped slot (reason + optional dismiss-timer handle) implementing the replace-in-place/restart-timer rules (R15) |
| dismiss control | **(2026-07-30)** the critical-error pill's × button: pointer-reactive (`reactive: true`), never keyboard-focusable (`can_focus: false`) — FR-007c |
| ribbon severity tint | **(2026-07-30, R17a)** `descriptor.severity` passed straight through to the ribbon as its `severityTint` (`null \| 'recoverable' \| 'critical'`); the ribbon stays visible/amber/paused-pulsing for `'recoverable'`, hidden for `'critical'` (FR-010e, `hud-logic.js`'s `ribbonVisibleForSeverity`) |
| panel button | optional `PanelMenu.Button`; reflects availability + Toggle (R8) — unaffected by this redesign |
| availability | whether `org.myna.Dictation` currently has a bus name owner (R9) |
| a11y label | `accessible_name` = human state label, updated per state (R10) |
| accent color | current `AccentColorPreference` (E2a), re-read live; colors the ribbon strands |
| motion preference | current `MotionPreference` (E2b), re-read live; selects flowing vs. static ribbon rendering |

Rules:
- No actor while `idle` (push-to-talk, spec FR-002); actors + timers + transitions
  torn down on `idle`, on name-vanished, and on `disable()` (spec FR-021 — no leaks).
- Nothing rendered, logged, or stored carries transcript content (constitution V).
- **(2026-07-30)** A second `notice`/`error` of the same severity while one is
  showing updates the held slot in place; it never creates a second concurrent
  actor/notice (R15 — no stacking or queuing).

## E5 — Availability (extension-side, transient)

Boolean derived from `Gio.bus_watch_name` on `org.myna.Dictation`
(name-appeared → available; name-vanished → unavailable). Drives dormancy: while
unavailable, the extension shows no overlay and surfaces no error (spec FR-018,
US1-5), and clears any HUD pill to idle on transition to unavailable (crash mid-session
edge case).

## State → visual-intent mapping (pure; contract-tested)

Lives in `extensions/myna-shell/states.js` and is unit-testable without a Shell
(R11). Maps `DictationState` → a visual-intent record consumed by the HUD:
**(2026-07-30)** the descriptor shape is reshaped from `{key, statusText,
isError, hidden}` to `{key, statusText, severity, hidden}`, where `severity` is
`'recoverable' | 'critical' | null` (replacing the old boolean `isError`) so
the HUD can distinguish the two problem tiers.

| State | icon | severity | a11y label |
|---|---|---|---|
| `idle` | — (hidden) | `null` | (actor absent) |
| `loading` | mic (filled) | `null` | "Dictation: loading model" |
| `recording` | mic (filled) | `null` | "Dictation: listening" |
| `transcribing` | mic (filled) | `null` | "Dictation: transcribing" |
| `finalizing` | mic (filled) | `null` | "Dictation: finishing" |
| `notice` | mic (filled — the mic itself isn't the fault) | `'recoverable'` | "Dictation: no speech detected" |
| `error` | mic-with-slash | `'critical'` | "Dictation: error — <reason>" |
| *(unknown)* | mic (filled) | `null` | "Dictation: active" |

The visual-intent record is CSS-class + icon-choice + label — no transcript,
tunable in `stylesheet.css` without touching the mapping.

