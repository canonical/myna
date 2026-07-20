# Contract: activity indicator (`Indicator` seam)

**Feature**: 003-desktop-injection · **Crate**: `client/myna-desktop`

The activity-indicator boundary (T22, UD129 Activity Indicator). `GtkIndicator`
(feature `ui-gtk`) + `NotifyIndicator` are the shipped implementors;
`MockIndicator` is the test fixture. Restates the guarantees for TDD.

## Interface (new)

```rust
pub enum IndicatorState { Hidden, Recording, Transcribing, Finalizing, Error(String) }

pub trait Indicator: Send {
    async fn set_state(&mut self, state: IndicatorState);
    async fn hide(&mut self);
}
```

The controller maps `OrchestratorEvent` → `IndicatorState`:
`Loading`→(preparing, shown as `Recording` with a loading hint) · `Ready`/capture→
`Recording` · `Transcribing`→`Transcribing` · `Release`/`Finalizing`→`Finalizing`
· `Done`→`Hidden` · `Error{message}`→`Error(message)`.

## Guarantees (each row → at least one test)

| # | Given | When | Then | Spec |
|---|-------|------|------|------|
| N1 | a session starts | capture begins | the indicator shows a distinct `Recording`/listening state | FR-017, US3-1 |
| N2 | inference is decoding | `Transcribing` set | a distinct transcribing state is shown | FR-017, US3-2 |
| N3 | the session ends normally | `Done` → `hide()` | the indicator clears (`Hidden`) | FR-018, US3-3 |
| N4 | an error / secure-field refusal | `Error(msg)` set | a distinct error state is shown with the message | FR-017, FR-023, US3-4 |
| N5 | the session becomes active | `Recording` set | the indicator becomes visible within the activation-latency target (≈100–200 ms) | FR-018, SC-005 |
| N6 | any state change | — | the state is exposed to assistive tech (screen-reader perceivable) | FR-019, US3-5 |
| N7 | the preferred surface is unavailable | shown | falls back to a secondary desktop-visible indicator; commit-only preserved | FR-020 |
| N8 | any state | indicator runs | no transcript text is rendered or logged (commit-only; privacy) | FR-024, Principle V |

## Test homes

- **Hermetic (no GTK/display)**: N1–N4, N8 and the `OrchestratorEvent →
  IndicatorState` mapping are covered against `MockIndicator` (records the state
  sequence); the mapping is a pure unit test. Built with the `ui-gtk` feature
  **off**, so hermetic runs never link GTK.
- **Integration (env-gated, real display)**: N5 (latency watermark), N6 (AT-SPI
  exposure), N7 (fallback) against `GtkIndicator` on a session with a display —
  part of `tests/` behind the display gate + the perf watermark suite (Principle
  III). Screen-reader exposure asserted via the GTK accessibility API.

## Non-goals

- No provisional/preedit text in the indicator (commit-only MVP).
- No tray-icon backend in iteration 1 (GNOME needs an extension; R6) — addable
  later behind this same trait.
