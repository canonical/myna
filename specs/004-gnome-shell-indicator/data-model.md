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

| Severity | Wire `State` | UX treatment | Auto-dismiss? |
|---|---|---|---|
| `recoverable` | `notice` | Non-blocking notice; a new session may start while it's showing | Yes — ~3.5 s hold, restarts in full if a new recoverable notice arrives while one is showing (R15) |
| `critical` | `error` | Persistent notice with a dismiss (×) control | No — remains until the user dismisses it; a new critical error while one is undismissed replaces the reason in place without waiving the dismiss requirement (R15) |

This is an **interim, client-inferred classification** (spec Assumptions;
research R13) pending T31/T62's wire-level error taxonomy — not itself that
taxonomy.

## E2 — AudioLevel (the VU-meter input)

| Field | Type / range | Source | Notes |
|---|---|---|---|
| `AudioRms` | `d`, `[0.0, 1.0]` linear full-scale | `AudioStats.rms` | drives the segmented VU meter's active-segment count (dominant input, stable) |
| `AudioPeak` | `d`, `[0.0, 1.0]` linear full-scale | `AudioStats.peak` | blended in at reduced weight so transients/consonants are visible without pinning the meter |

Rules:
- Published only while a session is active; `0.0` when `idle` (spec FR-011).
- Updated at ~15–20 Hz (R5); the extension applies **stale-decay** to floor if no
  update arrives within ~300 ms (spec FR-011 / SC-004), based on **arrival
  time**, not value — a repeated identical RMS/peak still counts as fresh
  (R16a; a 2026-07-30 manual-test regression found the extension had been
  dropping "unchanged" updates and treating a steady voice as stale).
- **(2026-07-30, R16/R16a)**: the bar meter maps RMS+peak through a
  hardware-calibrated dBFS scale (`vumeter.js`'s `levelsToIntensity`), then
  lights a left-to-right count of 24 segments (`intensityToActiveSegments`)
  colour-zoned green/yellow/red by position (`segmentColor`) — a conventional
  VU meter, not the ribbon-era symmetric spindle shape.
- Carries energy only — never samples, never content (constitution V, R5).

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
| HUD pill actor | the bottom-center `St.Widget` (2026-07-30: replaces the top-of-panel ribbon/goop) added via `Main.layoutManager.addChrome`; exists only while state ≠ `idle` |
| held notice slot | **(2026-07-30)** one severity-scoped slot (reason + optional dismiss-timer handle) implementing the replace-in-place/restart-timer rules (R15) |
| dismiss control | **(2026-07-30)** the critical-error pill's × button: pointer-reactive (`reactive: true`), never keyboard-focusable (`can_focus: false`) — FR-007c |
| panel button | optional `PanelMenu.Button`; reflects availability + Toggle (R8) — unaffected by this redesign |
| availability | whether `org.myna.Dictation` currently has a bus name owner (R9) |
| a11y label | `accessible_name` = human state label, updated per state (R10) |

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

