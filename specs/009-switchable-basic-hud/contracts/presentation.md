# Contract: Switchable Indicator Presentation

**Feature**: 009-switchable-basic-hud | **Date**: 2026-07-31

This contract supersedes feature 004's assumption that the sole view owns held
notice lifetime. `org.myna.Dictation` and `stateToDescriptor()` remain unchanged.

## Controller input contract

```text
setStyle(style)
onDescriptor(descriptor, occurrence)
onLevel(rms, peak, receivedAt)
dismiss()
destroy()
```

- `style`: `basic | wave`; any other value normalizes to `basic`.
- `descriptor`: feature-004 `{key, statusText, severity, hidden}` record.
- `occurrence`: distinguishes a genuinely new state occurrence from a replay;
  controller-internal style replay is never an occurrence.
- `receivedAt`: monotonic arrival timestamp, mandatory for freshness.

## IndicatorView rendering contract

```text
show(descriptor)
setLevel(rms, peak, receivedAt)
hide()
destroy()
```

Views receive `onDismiss` at construction. Views MUST NOT own notice deadlines,
decide whether idle overrides a held notice, or infer a fresh level timestamp.

Style normalization and constructor choice form a separate pure contract:

```text
normalizeHudStyle(style) -> basic | wave
createSelectedView(style, options, constructors) -> IndicatorView
```

This module MUST have no Shell/GI imports. Headless tests inject fake basic/wave
constructors and assert fallback, selection, and unchanged `onDismiss` option
forwarding. The Shell-dependent `view.js` only supplies real constructors.

| Method | Guarantee |
|---|---|
| `show` | Create/reuse this view's actor and render the supplied content-free descriptor. |
| `setLevel` | Cache/render the timestamped normalized level; tolerate calls before show or after hide. |
| `hide` | Return this presentation to hidden idle; no semantic notice veto. |
| `destroy` | Immediately remove all actors and transitions, disconnect monitor/preference signals, cancel rendering timers, and make callbacks inert. Idempotent. |

## Switching guarantees

| # | Guarantee | Spec |
|---|---|---|
| P1 | Exactly one view instance is active; replacement destroys the old view before showing the new one. | FR-005/008 |
| P2 | Switching never calls the D-Bus publisher, trigger, capture, transcription, or injection paths. | FR-006/023 |
| P3 | A visible descriptor is replayed into the replacement; hidden idle remains hidden. | FR-007 |
| P4 | Latest RMS/peak is replayed with its original `receivedAt`; switching cannot make stale audio fresh. | FR-007/015/016 |
| P5 | A recoverable notice retains its absolute deadline and remaining duration; style replay never restarts it. | edge case, FR-019 |
| P6 | A critical error remains held and dismissible through switching. | FR-020 |
| P7 | An explicitly dismissed critical occurrence remains dismissed through later style changes and is not reconstructed from unchanged source state. | FR-020 |
| P8 | A genuinely new problem occurrence replaces the held slot according to feature 004; no stack or queue is created. | FR-019/020 |
| P9 | After 100 rapid switches, only the final view responds to state, level, monitor, timer, preference, or dismiss callbacks. | SC-008 |
| P10 | Both styles use the primary monitor and the same descriptor semantics. | FR-018/022 |

## Basic view guarantees

| # | Guarantee | Spec |
|---|---|---|
| B1 | The view is a compact bottom-center pill with mic icon, status label, and horizontal track/fill. | FR-010/011 |
| B2 | The bar target is monotonic and clamped `[0,1]` for finite, missing, NaN, negative, and over-range inputs. | FR-013/014 |
| B3 | Shared calibrated intensity is normalized so the basic bar reaches true zero; no wave “alive” floor remains. | FR-015 |
| B4 | Only `recording` may target nonzero fill; all other lifecycle/severity states decay to zero while the label remains. | FR-015 |
| B5 | Repeated equal-valued fresh updates refresh `receivedAt`; stale input decays to zero within 600 ms. | FR-015/016, SC-005 |
| B6 | Reduced motion retains level information but removes/minimizes decorative interpolation. | FR-022 |
| B7 | Recoverable and critical states reuse the established icon, colour, text, persistence, replacement, and dismiss semantics. | FR-017–020 |
| B8 | The dismiss control is pointer-reactive and never keyboard-focusable. | FR-020/021 |

## Wave view guarantees

| # | Guarantee | Spec |
|---|---|---|
| W1 | Existing wave geometry, palette, phase behavior, reduced-motion behavior, and state/severity visuals remain unchanged. | FR-009 |
| W2 | Removing notice ownership from `HudView` changes policy location only; observable feature-004 notice behavior remains controlled by the controller. | FR-018–020 |
| W3 | Timestamp-aware level input preserves existing stale-decay during ordinary use and prevents stale resurrection on switch. | FR-009/015 |

## Lifecycle and privacy

- Extension disable destroys the controller, cancels its notice timer,
  disconnects settings and D-Bus service, then destroys the active view.
- Name absence remains dormant. Service disappearance clears an ordinary active
  descriptor, but an already-established held notice remains independent of
  service availability: recoverable expires on its original absolute deadline
  and critical remains until explicit dismissal. Service loss and style
  replacement restart neither lifetime.
- Unknown states retain feature 004's neutral content-free descriptor.
- No controller/view/settings payload contains raw audio or transcript content.
- Core operation requires no network.
