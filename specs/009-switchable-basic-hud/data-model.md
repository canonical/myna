# Phase 1 Data Model: Switchable Basic Dictation HUD

**Feature**: 009-switchable-basic-hud | **Date**: 2026-07-31

This model extends feature 004's extension-side transient model. The
`org.myna.Dictation` wire entities are unchanged. Only `HudStylePreference` is
persisted; no audio, transcript, history, state, level, or notice is stored.

## E1 - HudStylePreference

| Field | Type | Default | Rules |
|---|---|---|---|
| `hud-style` | closed enum: `basic \| wave` | `basic` | Per-user; missing/unknown values normalize to `basic`; a valid choice survives extension and desktop-session restarts. |

Transitions:

```text
basic <── user preference change ──> wave
unknown/absent ── normalization ──> basic
```

A preference transition changes presentation only. It does not create a
dictation state event, alter a notice deadline, refresh a level timestamp, or
start/stop a session.

## E2 - SourceDescriptor

The latest pure descriptor derived from feature 004's wire state:

| Field | Type | Meaning |
|---|---|---|
| `key` | string | `idle`, `loading`, `recording`, `transcribing`, `finalizing`, `notice`, `error`, or neutral unknown mapping |
| `statusText` | content-free string | Current lifecycle label or fixed/user-safe reason |
| `severity` | `null \| recoverable \| critical` | Notice policy and visual severity |
| `hidden` | boolean | Whether ordinary presentation should be absent |

This is the latest source state. It may differ from `DisplayedDescriptor` after
a critical error is explicitly dismissed while the source still reports error.

## E3 - DisplayedDescriptor

The descriptor currently rendered or eligible for replay into a replacement
view.

| State | Meaning |
|---|---|
| `null` | Nothing should be visible; a style change must not call `show()`. |
| ordinary descriptor | Render while source is active. |
| held recoverable descriptor | Render until its deadline or a newer ordinary/problem state supersedes it. |
| held critical descriptor | Render until explicit dismissal or a newer state supersedes/replaces it. |

Rules:
- Style replay does not mutate this entity.
- Explicit critical dismissal sets it to `null` and marks that source occurrence
  dismissed so a style switch cannot resurrect it.
- A genuinely new source transition may establish a new displayed descriptor.

## E4 - HeldNotice

| Field | Type | Rules |
|---|---|---|
| `descriptor` | recoverable or critical descriptor | Exactly one slot; new problem state replaces in place, never queues. |
| `deadline` | monotonic timestamp or `null` | Recoverable: arrival + established hold duration; critical: `null`. |
| `dismissed` | boolean | Relevant to critical occurrence; explicit pointer dismissal survives style replacement. |

State transitions:

```text
none ── recoverable ──> recoverable(deadline)
recoverable ── new recoverable ──> recoverable(new full deadline)
recoverable ── critical ──> critical(no deadline)
critical ── new critical/recoverable ──> replacement
recoverable ── deadline ──> none/hidden
critical ── explicit dismiss ──> none/hidden
any held ── ordinary active state ──> none + ordinary display
style change ──> no held-state transition
service unavailable ──> no held-state transition
```

The controller owns one timer corresponding to the recoverable deadline. Views
own no notice timer. Service disappearance clears an ordinary active display,
but does not clear or restart an already-established held notice.

## E5 - TimestampedLevel

| Field | Type / range | Rules |
|---|---|---|
| `rms` | number, normalized/clamped to `[0,1]` by rendering logic | Derived energy only; no sample content. |
| `peak` | number, normalized/clamped to `[0,1]` by rendering logic | Repeated equal values are still fresh updates. |
| `receivedAt` | monotonic timestamp | Set at D-Bus callback arrival; never reset merely because a view changes. |

The replacement view receives the complete entity. Its age is
`now - receivedAt`, so stale audio cannot become fresh after switching.

## E6 - BasicMeterState

| Field | Type | Rules |
|---|---|---|
| `targetFill` | `[0,1]` | Zero unless `SourceDescriptor.key == recording`; calibrated from timestamped RMS/peak and zero-normalized from the shared meter floor. |
| `displayFill` | `[0,1]` | Smoothed toward target with fast attack/slower release; reduced motion may snap/minimize easing. |
| `lastFrameAt` | monotonic timestamp | Computes deterministic smoothing delta. |

The bar continues updating after the last level only until it reaches zero; it
must not retain a permanent animation timer while hidden.

## E7 - ActiveView

| Field | Type | Rules |
|---|---|---|
| `style` | `basic \| wave` | Matches normalized preference. |
| `instance` | one `IndicatorView` | Exactly one owned instance while extension is enabled. |
| `generation` | monotonically increasing local integer | Optional test/guard aid so retired callbacks cannot affect current state. |

Replacement is atomic from the controller's perspective: destroy old instance,
create one new instance, replay display and level. The dictation service remains
connected throughout.

## E8 - IndicatorControllerSnapshot

Aggregate in-memory state:

```text
selectedStyle
sourceDescriptor
displayedDescriptor
heldNotice
latestLevel
activeView
enabled
```

Invariants:
- At most one active view.
- A hidden source with no held notice produces no visible actor.
- View replacement never changes source/display/held/level semantics.
- Disable cancels the controller timer, destroys the active view, and makes all
  retired callbacks inert.
- Snapshot contains no transcript text or raw audio.

## Relationship to feature 004

Feature 004's `DictationState`, severity, error reason, audio level,
availability, accent preference, motion preference, and state descriptor remain
authoritative. Feature 009 moves held-notice and level-freshness ownership out of
the wave view into E8 so two views can safely share the same semantics.
