# Phase 1 Data Model: Desktop Session Controller + Text Injection

**Feature**: 003-desktop-injection · **Date**: 2026-07-19

Entities are Rust types in the new `client/myna-desktop` crate, plus reused types
from `myna-orchestrator`/`myna-core` (named, not redefined). New types are given
fields, invariants, and lifecycle.

## Reused (unchanged) — for reference only

- **`Trigger`** / **`TriggerEdge::{Press,Release}`** (`myna-orchestrator::trigger`):
  the activation boundary. The portal hotkey implements `Trigger`; `ScriptedTrigger`
  is the test mock. **No shape change** — the portal is a new implementor.
- **`TextSink`** / **`OrchestratorEvent`** (`myna-orchestrator::sink`, `::fsm`):
  the event boundary. `OrchestratorEvent::{Loading, Ready, Transcribing, Snippet,
  Final, Done, Error, AudioDropped}` already exist; the injection adapter consumes
  them. **No shape change.**
- **`run_dictation` / FSM** (`myna-orchestrator::runner`, `::fsm`): the per-utterance
  session/residency machine (capture-at-press, push-gated-on-`ready`). Reused.
- **`CaptureSource` / `PipeWireBackend`** (`myna-audio`, feature 002): native
  capture. Reused unchanged.
- **`AudioFormat`** (`myna-core`): negotiated capture format. Reused.

## New: `DesktopController`

The desktop session controller (T21) — owns the multi-session lifecycle.

- **Fields (private)**: the composed boundaries (`Box<dyn Trigger>`,
  `Box<dyn Injector>`, `Box<dyn Indicator>`), the audio/backend factory, the
  negotiated `AudioFormat`, and the current `DictationState`.
- **Construction**: `DesktopController::builder()` taking the three boundaries +
  a session factory, so tests inject mocks and the binary injects the real
  portal/IBus/GTK implementations.
- **Behavior**: runs a loop — await a `Press` edge → acquire the injection target
  → (refuse if secure) → start a session (capture + inference), open the indicator
  → route `OrchestratorEvent`s (commit `Final`/`Done` to the injector, all states
  to the indicator) → on `Release` (or `FocusOut`, or target-gone, or terminal
  error) finalize/cancel and return to idle. Never captures audio outside an
  active session.
- **Invariants**: at most one active session; the target is fixed for a session's
  lifetime; only `Final`/`Done` text reaches the injector; audio discarded at end;
  push-to-talk only.

### State model (`DictationState`)

Carried into Rust from the retired Python `DictationState` (UD129 State
Management), extended with UD129's explicit `Cancelled`/`Completed`:

| State | Meaning |
|---|---|
| `Idle` | no capture; waiting for a `Press` |
| `Starting` | acquiring target + mic + inference session |
| `Recording` | capturing; audio streaming; awaiting/receiving events |
| `Transcribing` | inference decoding (may overlap Recording in streaming) |
| `Finalizing` | `Release` seen; no new audio; awaiting terminal event |
| `Completed` | terminal event received; committed text done |
| `Cancelled` | session aborted (focus lost / target gone / user cancel) |
| `Error` | unrecoverable failure; user feedback owed |

**Legal transitions** (anything else is a controller bug — encoded as a test):

```text
Idle        → Starting
Starting    → Recording | Cancelled | Error | Idle
Recording   → Transcribing | Finalizing | Cancelled | Error
Transcribing→ Recording | Finalizing | Cancelled | Error
Finalizing  → Completed | Cancelled | Error
Completed   → Idle
Cancelled   → Idle
Error       → Idle
```

## New: `Injector` (trait) + `InjectionTarget`

The text-injection boundary (T22, UD129 Text Injection Layer). Backend-agnostic
(FR-016); IBus is the first implementor.

- **`InjectionTarget`**: an opaque handle to the surface focused at session start,
  carrying its **secure flag** (from content-type) and enough identity to detect
  a focus change / disappearance. Never exposes text content.
- **`Injector` operations** (all async, mirror the retired `TextInjector` Protocol):
  - `acquire() -> Result<InjectionTarget, InjectError>` — bind the currently
    focused target; **`Err(SecureField)`** where a password/secure purpose is
    detectable; **`Err(NoTarget)`** where nothing editable is focused.
  - `set_activity(active: bool)` — reflect recording/transcription activity on the
    injection channel where the backend supports it.
  - `commit(text: &str)` — insert stable committed text; never modified after.
  - `set_preedit(text: &str)` / `supports_preedit() -> bool` — **future (R9)**:
    optional volatile-preedit channel for streaming partial-then-commit;
    no-op default, `false` by default, so commit-only backends degrade cleanly.
    Not used in the MVP.
  - `cancel()` — abort without further injection; **idempotent**.
  - `end()` — finalize and release the target/engine; **idempotent**.
  - `focus_changed() -> impl Stream<Item = FocusEvent>` (or a callback) — surfaces
    `FocusOut` / target-gone so the controller can end safely (FR-014, FR-022).
- **`InjectError`**: `SecureField` / `NoTarget` / `Unavailable(String)` (IBus not
  reachable) / `Backend(String)`.
- **Invariants**: commit-only (no preedit); literal text only (no synthesized
  keys, FR-015); one target per session, fixed at `acquire`; `cancel`/`end`
  idempotent; releases the engine/global-engine on `end`/`cancel` even on the
  error path.

### Backends

- **`IbusInjector`** (shipped): registers an IBus component/engine over `zbus`;
  `acquire` makes it the active engine and reads the focused context's
  content-type; `commit` → `CommitText`; `FocusOut`/context-gone → `focus_changed`;
  `end`/`cancel` restores the prior engine.
- **`MockInjector`** (tests): records `commit`/`cancel`/`end` calls and scripts
  `acquire` outcomes (ok / secure / no-target) + focus events. Hermetic.

## New: `Indicator` (trait) + `IndicatorState`

The activity indicator boundary (T22, UD129 Activity Indicator).

- **`IndicatorState`**: `Hidden` | `Recording` | `Transcribing` | `Finalizing` |
  `Error(message)` — the distinct, screen-reader-perceivable states (FR-017/019).
- **`Indicator` operations**: `set_state(IndicatorState)` (async or channel-based);
  `hide()`. Idempotent per state.
- **Invariants**: appears within the activation-latency target after `Recording`
  is set (SC-005); clears on `Hidden`; carries no transcript text.

### Backends

- **`GtkIndicator`** (`ui-gtk` feature): a borderless always-on-top GTK4 overlay;
  error state also raises a `notify-rust` notification. Runs on the GTK main
  thread; state pushed via a channel.
- **`NotifyIndicator`**: `notify-rust`-only, for error toasts / headless fallback.
- **`MockIndicator`** (tests): records the state sequence. Hermetic, no GTK.

## New: portal `Trigger` — `GlobalShortcutTrigger`

Implements the reused `Trigger` trait over the GlobalShortcuts portal.

- **Construction**: `GlobalShortcutTrigger::bind(shortcut_id, preferred_trigger)
  -> Result<Self, TriggerError>` — create portal session + `BindShortcuts`.
- **Behavior**: maps portal `Activated` → `TriggerEdge::Press` (deduped: first
  wins until `Deactivated`, FR-008), `Deactivated` → `TriggerEdge::Release`;
  `None` when the shortcut is unbound / portal session ends.
- **Invariants**: at most one `Press` outstanding at a time (autorepeat collapsed);
  no key grabbing outside the portal; no synthesized input.

## Relationships

```text
GlobalShortcutTrigger ──(Press/Release)──▶ DesktopController      (activation, FR-006/007)
DesktopController ── acquire ─────────────▶ Injector.InjectionTarget (target@start, FR-014)
DesktopController ── run_dictation ───────▶ orchestrator FSM ──▶ OrchestratorEvent
OrchestratorEvent::{Final,Done} ──────────▶ Injector.commit         (commit-only, FR-003/012)
OrchestratorEvent::{Loading,Ready,…} ─────▶ Indicator.set_state     (feedback, FR-017)
Injector.focus_changed (FocusOut/gone) ───▶ DesktopController → end safely (FR-014/022)
Injector.acquire → Err(SecureField) ──────▶ DesktopController → refuse + Error (FR-021)
```

`Trigger`, `Injector`, and `Indicator` are independent boundaries the controller
composes; each has a mock so the controller's policy (dedup, focus-end,
secure-refusal, commit-only, state transitions) is fully hermetic-testable
without D-Bus, IBus, a portal, or a display.
