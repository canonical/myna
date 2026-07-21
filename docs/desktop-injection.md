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

- **`Trigger`** (reused from `myna-orchestrator`) — activation edges. Default:
  `shortcut::control::ControlTrigger` (control socket + GNOME shortcut);
  packaged: `shortcut::portal::GlobalShortcutTrigger`; debug: `StdinTrigger`;
  tests: `ScriptedTrigger`.
- **`Injector`** (`inject::Injector`) — text injection. Production:
  `inject::ibus::IbusInjector`; tests: `inject::mock::MockInjector`.
- **`Indicator`** (`indicator::Indicator`) — activity surface. Default:
  `indicator::notify::NotifyIndicator` (notifications); opt-in experimental:
  `indicator::gtk::GtkIndicator` (feature `ui-gtk`); tests:
  `indicator::mock::MockIndicator`.

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

Event routing (`route_event`): `Final` → committed text (commit-only — never
`Snippet`); every liveness event → `Indicator::set_state` via the
`OrchestratorEvent → IndicatorState` mapping (`Loading`/`Ready`→Recording,
`Transcribing`→Transcribing, `Done`→Hidden, `Error`→Error(msg)); `Release`/
focus-loss → Finalizing. The select loop is **biased** (drain buffered liveness
before a coincident Release/focus edge), and focus-loss is handled **before** the
trigger so it wins (end safely).

**Commit coalescing (2026-07-20).** `Final`s are **buffered** and inserted as a
single `CommitText` at the next boundary (the terminal `done`, or any non-`Final`
event between spaced streaming finals), rather than one commit per segment. This
is load-bearing: **rapid successive IBus commits race and only the last lands**
in the target, so a commit-on-finalize adapter — which emits the whole utterance
as a back-to-back burst — would otherwise insert only its final segment (the
"only the last second or two appears" bug, root-caused via `MYNA_DEBUG` on
2026-07-20). Coalescing joins the burst into one commit (which is reliable);
spaced streaming finals (separated by a liveness event) still flush and insert
individually. A segment buffered but not yet flushed when focus is lost is
discarded, not inserted into the now-wrong surface (SC-007).

**Live instrumentation (`MYNA_DEBUG=1`).** Every pipeline stage streams
timestamped stderr diagnostics — `capture:` (bytes forwarded, end-of-audio
total), `runner:` (ready gate), `ws:` (audio frames sent, events received with
text preview), `ctrl:` (press/release/focus), `inject:` (committed text) — so
"where did my utterance go?" is answerable live without a debugger. Off by
default; when on it prints transcript text (`myna_core::debug`).

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
- **Commit**: buffer `Final` segments and emit the engine's `CommitText` signal
  with a serialized `IBusText` (`(sa{sv}sv)`) **once per burst** (see commit
  coalescing above). Commit-only — the engine never calls `UpdatePreeditText`.
- **Focus/secure (R4/R5)**: `FocusOut` → a `FocusEvent::FocusOut` on the focus
  stream (controller ends safely, suppresses further commits); `SetContentType`
  with `PASSWORD`/`PIN` purpose → `acquire` returns `Err(SecureField)`.

Verified: the connection handshake + GVariant shapes against the running daemon;
the full register→activate→commit→restore cycle against an isolated IBus daemon
(`dbus-run-session`) via the gated `ibus_hw` suite. Injection into a focused GUI
field is the manual spoken-run / gated-suite acceptance on hardware.

## Activation

Dictation injects into *another* app, so activation must not depend on terminal
focus. Three mechanisms, behind the reused `Trigger` seam:

- **Control socket + GNOME custom shortcut (default).** `shortcut::control::
  ControlTrigger` listens on a Unix socket; `myna-desktop --toggle` pokes it
  (first poke `Press`, next `Release` — toggle-to-talk). A GNOME custom keyboard
  shortcut bound to `myna-desktop --toggle` (via `--install-shortcut`, gsettings)
  fires it globally. Works for an unsandboxed binary: no terminal focus, no
  portal, no app id. This is the works-today path on GNOME/Wayland.
- **GlobalShortcuts portal (`--portal`, R2).** `shortcut::portal::
  GlobalShortcutTrigger::bind(id, preferred_trigger)` — real hold-to-talk
  (`Activated`→`Press`, `Deactivated`→`Release`), autorepeat collapsed by the
  hermetic-tested `Dedup` state machine (FR-008). **But** GNOME's backend refuses
  callers without an app identity ("an app id is required"), which it only grants
  sandboxed / `.desktop`-launched apps — so this is the activation for the
  **packaged** (snap/flatpak) build, not a bare dev binary.
- **stdin (`--stdin`).** The orchestrator's `StdinTrigger` — terminal debug only
  (the terminal keeps focus, so text injects back into the terminal).

## Activity indicator: notifications (default) + GTK4 overlay (R6)

Feedback defaults to **desktop notifications** (`indicator::notify::
NotifyIndicator`) — no window, so it never perturbs focus. It drives one
**updating** toast through the lifecycle (🎤 listening → 💬 transcribing →
⏳ finishing, closed on `Hidden`, a critical toast on `Error`), replacing it in
place by notification id. Urgency is **Normal** for the live states — GNOME
Shell suppresses the banner for low-urgency notifications and drops them
straight to the tray, which read as "no UI at all" (2026-07-20). It carries
state labels only, never transcript text (N8).

An opt-in GTK4 overlay (`--overlay`, `indicator::gtk::GtkIndicator`) gives a
persistent per-state surface with AT-SPI labels (FR-019), but is
**experimental**: on GNOME/Wayland mapping a top-level can shift keyboard focus
off the target — our IBus engine then sees `FocusOut` and ends the session (its
wrong-target safety), cutting dictation short. The fix (a proper always-on-top
HUD that never takes focus) is to move it to the **layer-shell** protocol
(`gtk4-layer-shell`, `KeyboardMode::None`), as the reference Handy app does;
tracked for the UI-improvements pass. When used it owns the process main thread
+ GLib loop, with the tokio controller on a worker thread bridged by an
`async-channel`; the error state also raises a `notify-rust` toast.

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
