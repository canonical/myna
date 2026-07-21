# Phase 1 Data Model: GNOME Shell Extension for Myna Dictation UI

**Feature**: 004-gnome-shell-indicator | **Date**: 2026-07-21

The entities crossing the extension↔`myna-desktop` seam and the extension's own
transient state. Nothing here is persisted; audio is never represented (only
derived level). Cross-refs to spec Key Entities and the D-Bus contract
(`contracts/dbus-interface.md`).

## E1 — DictationState (the wire state string)

The single source of truth for the goop's treatment. String enum on the
`org.myna.Dictation.State` property and in `StateChanged`.

| Value | Meaning | Derived in myna-desktop from |
|---|---|---|
| `idle` | No session; goop hidden. | `IndicatorState::Hidden` |
| `loading` | Cold model load in progress (distinct glow, FR-006). | `OrchestratorEvent::Loading` seen, `Ready` not yet |
| `recording` | Capturing / listening. | `IndicatorState::Recording` (post-`Ready`) |
| `transcribing` | Inference decoding. | `IndicatorState::Transcribing` |
| `finalizing` | Release seen; awaiting terminal transcript. | `IndicatorState::Finalizing` |
| `error` | Failure / secure-field refusal. | `IndicatorState::Error(_)` |

Rules:
- Additive/forward-compatible: an **unknown** value MUST map to a neutral "active"
  visual intent on the extension side, never an exception (spec FR-008, R3).
- Privacy: the value is a state label only — never transcript text (constitution
  V; the `Error` message is a user-facing reason string, still content-free).
- Legal transitions mirror the controller's audited `DictationState` model
  (`controller.rs`): idle→loading|recording, loading→recording|error|idle,
  recording→transcribing|finalizing|error|idle, transcribing→recording|finalizing|
  error|idle, finalizing→idle|error, error→idle. The extension does **not** enforce
  legality (it renders whatever arrives, degrading unknowns); the publisher is the
  authority.

## E2 — AudioLevel (the VU input)

| Field | Type / range | Source | Notes |
|---|---|---|---|
| `AudioRms` | `d`, `[0.0, 1.0]` linear full-scale | `AudioStats.rms` | drives glow radius/intensity |
| `AudioPeak` | `d`, `[0.0, 1.0]` linear full-scale | `AudioStats.peak` | gates the brighter rim / clip cue |

Rules:
- Published only while a session is active; `0.0` when `idle` (spec FR-011).
- Updated at ~15–20 Hz (R5); the extension applies **stale-decay** to floor if no
  update arrives within ~300 ms (spec FR-011 / SC-004).
- Carries energy only — never samples, never content (constitution V, R5).

## E3 — ErrorReason (optional)

A short, user-facing reason string carried by `StateChanged`'s second argument
(and mirrored in an `ErrorMessage` property) when `State == error` — e.g.
"no text field is focused", "refusing to type into a password field",
"inference backend unavailable". Empty when not in error. Content-free (never
transcript). Sourced from the controller's existing `IndicatorState::Error(msg)`
/ `abort_before_capture` messages.

## E4 — IndicatorSurface (extension-side, transient)

The extension's in-memory view; not on the wire.

| Aspect | Description |
|---|---|
| current state | last `DictationState` received (default `idle`) |
| current level | last `AudioLevel` + a timestamp (for stale-decay) |
| goop actor | the `St.DrawingArea`/`St.Widget` added to `Main.layoutManager`; exists only while state ≠ `idle` |
| panel button | optional `PanelMenu.Button`; reflects availability + Toggle (R8) |
| availability | whether `org.myna.Dictation` currently has a bus name owner (R9) |
| a11y label | `accessible_name` = human state label, updated per state (R10) |

Rules:
- No actor while `idle` (push-to-talk, spec FR-002); actors + timers + transitions
  torn down on `idle`, on name-vanished, and on `disable()` (spec FR-021 — no leaks).
- Nothing rendered, logged, or stored carries transcript content (constitution V).

## E5 — Availability (extension-side, transient)

Boolean derived from `Gio.bus_watch_name` on `org.myna.Dictation`
(name-appeared → available; name-vanished → unavailable). Drives dormancy: while
unavailable, the extension shows no overlay and surfaces no error (spec FR-018,
US1-5), and clears any goop to idle on transition to unavailable (crash mid-session
edge case).

## State → visual-intent mapping (pure; contract-tested)

Lives in `extensions/myna-shell/states.js` and is unit-testable without a Shell
(R11). Maps `DictationState` → a visual-intent record consumed by the actor:

| State | colour class | animation | a11y label |
|---|---|---|---|
| `idle` | — (hidden) | none | (actor absent) |
| `loading` | warm-amber | slow breathing pulse | "Dictation: loading model" |
| `recording` | active/brand | ripple + level-driven glow | "Dictation: listening" |
| `transcribing` | processing | shimmer / dots | "Dictation: transcribing" |
| `finalizing` | confirm | single flash then fade | "Dictation: finishing" |
| `error` | error-red | flash + shake, then clear | "Dictation: error — <reason>" |
| *(unknown)* | active/brand | neutral pulse (no throw) | "Dictation: active" |

The visual-intent record is CSS-class + animation-name + label — no transcript,
tunable in `stylesheet.css` without touching the mapping.
