# Desktop session controller + text injection (T21/T22)

The dictation **last-mile**: a global shortcut activates push-to-talk, the
*existing* client capture→FSM→transcript path runs (feature 002 native capture +
`myna-orchestrator`), and committed transcripts are inserted into the application
focused when the session started. Shipped as the Rust `client/myna-desktop` crate
(feature 003-desktop-injection). This is the settled contract; per-branch history
is in `specs/003-desktop-injection/`.

## Shape

```text
GlobalShortcutTrigger ──Press/Release──▶ ┌────────────────────┐
   (portal hotkey)                       │  DesktopController  │
                                         │  (per-utterance FSM │
IbusInjector ◀── commit(Final) ──────────│   over run_dictation)│
   (IBus engine)   focus/secure events ─▶│                     │
                                         │  Indicator.set_state│──▶ GtkIndicator
myna-audio (PipeWire) ──PCM──▶ run_dictation ──OrchestratorEvent─┘    / NotifyIndicator
```

Three boundary seams, each with a mock so the controller is fully
hermetic-testable (no D-Bus / IBus / portal / display):

- **`Trigger`** (reused from `myna-orchestrator`) — activation edges. Production:
  `shortcut::portal::GlobalShortcutTrigger`; MVP stand-in: `StdinTrigger`; tests:
  `ScriptedTrigger`.
- **`Injector`** (`inject::Injector`) — text injection. Production:
  `inject::ibus::IbusInjector`; tests: `inject::mock::MockInjector`.
- **`Indicator`** (`indicator::Indicator`) — activity surface. Production:
  `indicator::gtk::GtkIndicator` (feature `ui-gtk`) + `indicator::notify::NotifyIndicator`
  (error toasts / headless); tests: `indicator::mock::MockIndicator`.

## Controller state model

`DictationState` (carried from the retired Python `DictationState`, extended with
`Cancelled`/`Completed`). Legal transitions are a table; any other transition is a
bug (`advance` panics, asserted in tests):

```text
Idle → Starting → Recording | Cancelled | Error | Idle
Recording   → Transcribing | Finalizing | Cancelled | Error
Transcribing→ Recording | Finalizing | Cancelled | Error
Finalizing  → Completed | Cancelled | Error
Completed/Cancelled/Error → Idle
```

Per utterance: await `Press` → `acquire()` the focused target (refuse secure
fields, no capture) → start `run_dictation` (capture-at-press, push gated on
`ready`) → route events → finalize on `Release` / focus-loss / terminal → `Idle`.
Capture exists **only** between Press and end — never while Idle (push-to-talk,
FR-004/SC-004).

Event routing (`route_event`): `Final` → `Injector::commit` (commit-only — never
`Snippet`); every liveness event → `Indicator::set_state` via the
`OrchestratorEvent → IndicatorState` mapping (`Loading`/`Ready`→Recording,
`Transcribing`→Transcribing, `Done`→Hidden, `Error`→Error(msg)); `Release`/
focus-loss → Finalizing. The select loop is **biased** (drain buffered liveness
before a coincident Release/focus edge), and focus-loss is handled **before** the
trigger so it wins (end safely).

## Injection: IBus engine over zbus (R1)

`IbusInjector` speaks the IBus wire protocol (D-Bus / GVariant) directly via
`zbus` — pure Rust, no FFI, no GObject-introspection, no subprocess:

- **Address discovery**: `$IBUS_ADDRESS`, else the socket file under
  `~/.config/ibus/bus/<machine-id>-<display>` (`IBUS_ADDRESS=` line).
- **Register**: `RegisterComponent(v)` with a serialized `IBusComponent`
  (`(sa{sv}ssssssssavav)`) carrying one `IBusEngineDesc`
  (`(sa{sv}ssssssssussssssss)` — layout confirmed against the live daemon).
- **Serve**: a `Factory` object (`CreateEngine → engine path`) and an `Engine`
  object (`FocusIn`/`FocusOut`/`SetContentType`/`ProcessKeyEvent`/…) on the bus.
- **Activate**: save the prior global engine (`GetGlobalEngine`), `SetGlobalEngine`
  ours; restore on `end`/`cancel` (idempotent, restore-once).
- **Commit**: emit the engine's `CommitText` signal with a serialized `IBusText`
  (`(sa{sv}sv)`). Commit-only — the engine never calls `UpdatePreeditText`.
- **Focus/secure (R4/R5)**: `FocusOut` → a `FocusEvent::FocusOut` on the focus
  stream (controller ends safely, suppresses further commits); `SetContentType`
  with `PASSWORD`/`PIN` purpose → `acquire` returns `Err(SecureField)`.

Verified: the connection handshake + GVariant shapes against the running daemon;
the full register→activate→commit→restore cycle against an isolated IBus daemon
(`dbus-run-session`) via the gated `ibus_hw` suite. Injection into a focused GUI
field is the manual spoken-run / gated-suite acceptance on hardware.

## Activation: GlobalShortcuts portal (R2)

`GlobalShortcutTrigger::bind(id, preferred_trigger)` creates a portal session and
binds one hold-to-talk shortcut (default `Super+D`, confirmed/rebound in the
desktop's own dialog — the app ships no shortcut-config UI). `Activated`→`Press`,
`Deactivated`→`Release`, session-end→`None`. Compositor autorepeat is collapsed to
a single `Press` until `Deactivated` by the pure `Dedup` state machine
(first-Activated-wins, FR-008), which is fully hermetic-tested via a scripted
`PortalSignal` stream.

## Activity indicator: GTK4 overlay (R6)

`GtkIndicator` is a borderless, non-focusable GTK4 overlay with distinct visuals
per state and AT-SPI labels (FR-019); the error state also raises a `notify-rust`
toast (FR-020). GTK owns the process **main thread** + GLib loop; the tokio
controller runs on a worker thread; states flow over an `async-channel`. Gated
behind the `ui-gtk` feature so the hermetic suite never links GTK; without it, the
headless `NotifyIndicator` runs on a plain tokio runtime.

## Testing (Principles I/II/III)

- **Hermetic** (`cargo test -p myna-desktop --no-default-features`): the whole
  controller lifecycle, commit-only routing, no-capture-while-idle, cold-load,
  focus-out/target-gone/secure-field safety, indicator timeline, and the portal
  autorepeat-dedup — all through the three mocks. No D-Bus/IBus/portal/display.
- **Env-gated integration** (`MYNA_IBUS_TESTS` / `MYNA_PORTAL_TESTS` / display):
  real IBus commit/restore, portal bind, GTK overlay — identical code on the VM
  and on hardware. `tests/{ibus_hw,portal_hw,indicator_hw}.rs`.
- **Watermarks**: activation→indicator-visible (≤200 ms, SC-005), press→capture
  (<100 ms), per-segment commit (<50 ms) — gated perf baselines; capture-path
  baselines inherited from feature 002.

## Future (R3/R9)

- **Wayland-native `input_method_v2`** backend for wlroots compositors (mutter
  lacks it; IBus is the only path on GNOME) — addable behind the same `Injector`
  seam.
- **Streaming preedit**: `Injector::set_preedit`/`supports_preedit` are scaffolded
  (`IbusInjector` reports `true`) but unused — the MVP is commit-only. Flipping it
  on routes `Snippet` → preedit without reshaping the seam or the FSM.
