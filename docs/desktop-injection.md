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
`Transcribing`→**Recording**, `Done`→Hidden, `Error`→Error(msg)); `Release`/
focus-loss → Finalizing. The indicator walks Recording → [release] → Finalizing
→ Hidden. **`Transcribing` maps to `Recording` (listening), not a distinct
"working" look**: streaming / re-decode adapters emit `transcribing` progress
*while the key is still held and the user is still speaking*, so projecting it
mid-utterance reads wrong — the visible phase is trigger-driven (listening while
held, finishing after release). The internal `DictationState::Transcribing`
still advances; it just isn't shown during capture. The select loop is
**biased** (drain buffered liveness before a coincident Release/focus edge), and
focus-loss is handled **before** the trigger so it wins (end safely).

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

**Spacing between commits (2026-07-27).** Contract I2 (008
emission-semantics) makes committed deltas **verbatim-concatenable**: they
carry their own natural whitespace (whisper word/segment texts keep their
leading spaces; only the utterance's first delta sheds its leading space), so
an injector that inserts each delta as it lands reproduces the transcript
exactly. The controller is additionally *defensive*: when joining a burst or
flushing a spaced commit after earlier text, it inserts a separator **only
if neither side already has whitespace** — so legacy stripped-segment servers
(the fake adapter, pre-fix whisper) get single spaces restored and contract
servers never get doubles. Regression tests:
`stripped_streaming_segments_are_spaced_across_flushes` (the live "Thisis
notworking that well." bug) and `natural_spacing_segments_get_no_double_spaces`.

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
  coalescing above). Commit-only in batch mode; when preedit is on the engine
  also drives `UpdatePreeditText`/`HidePreeditText` (see *Streaming preedit*
  below).
- **Focus/secure (R4/R5)**: `FocusOut` → a `FocusEvent::FocusOut` on the focus
  stream (controller ends safely, suppresses further commits). The stream is a
  **broadcast** — every utterance subscribes its own receiver, so focus-loss
  safety holds for session N, not just the first (a single-consumer channel
  silently disabled it after utterance 1: observed live as text crossing a
  mid-session focus change into another window); a lagging receiver is treated
  as focus-lost (fail-safe). `SetContentType`
  with `PASSWORD`/`PIN` purpose → refused. Checked at **two** points (F2):
  `acquire` waits for `FocusIn` then lets `SetContentType` settle
  (`FOCUS_WAIT`=400ms + `CONTENT_TYPE_GRACE`=50ms) before reading the purpose,
  and `commit` re-reads the latest purpose each call — closing the window where
  `SetContentType` arrives after `acquire` returned (late delivery, or a
  mid-session focus change into a secure field). A slow/absent `FocusIn` is NOT
  a hard-fail: that is the ordinary-field case, and refusing there breaks
  legitimate dictation.

**Residual risk (FR-021 "where undetectable", US4-4).** Detection is a *push*
signal to our engine. **X11/XWayland apps**: GTK's IBus module talks to the
daemon directly, so `SetContentType` reaches us and refusal works. **Wayland
apps**: the field's content-type stops at the compositor — verified on GNOME
Shell (libmutter-18, and even the newer mutter-51): the `ClutterInputMethod`
bridge between Mutter's `text-input-v3` relay and the shell's IBus client has
NO content-type entry point (its whole surface is commit/focus/preedit/
surrounding/keys), and `ibusManager.js` has no content-type handling. The
signal never enters the IBus daemon, so `purpose` stays 0 for every Wayland
field — a password field is indistinguishable from a notes field (confirmed
live with `MYNA_DEBUG=1`: `FocusIn` arrives promptly, `SetContentType` never
does). This is a platform gap, not a bypassable check: hard-failing on "no
content-type seen" would refuse every ordinary Wayland field too. The fix
belongs upstream (mutter/gnome-shell IM bridge). Diagnose per-field with
`MYNA_DEBUG=1` — the acquire/commit paths log `focus_received`, `purpose`,
every `SetContentType`/`FocusIn`/`FocusOut`, and commit refusals.

Verified: the connection handshake + GVariant shapes against the running daemon;
the full register→activate→commit→restore cycle against an isolated IBus daemon
(`dbus-run-session`) via the gated `ibus_hw` suite. Injection into a focused GUI
field is the manual spoken-run / gated-suite acceptance on hardware.

## Streaming preedit (R9)

Partials in the field: each `Unstable` hypothesis is rendered in the target's
**preedit region** — the same volatile-text channel IMEs use — instead of
waiting silently for commits:

- **Render**: `UpdatePreeditText(IBusText, cursor, visible, mode)` — note the
  **four** args: the daemon parses the engine signal strictly as `(vubu)`
  (ibus ≥ 1.5.29, `bus/engineproxy.c`), and a 3-arg `(vub)` emission fails
  `g_variant_get` there and is dropped *silently*. `mode` is
  `IBUS_ENGINE_PREEDIT_CLEAR` — focus-out discards the volatile text, never
  commits it. The `IBusText` carries a single underline attribute spanning
  the whole string (IBus indexes are **char**-based, not bytes) — the
  conventional "uncommitted" visual. Two serialization traps, both
  root-caused live (2026-07-28) by diffing our signal bytes against
  libibus-serialized canonical bytes and driving a probe `IBus.InputContext`:
  the attr list must be **variant-wrapped** (`(sa{sv}sv)`, not inline
  `(sa{sv}s(sa{sv}av))` — tolerated for `CommitText`, dropped for preedit),
  and the `mode` arg above. The daemon's parser is the reference; the
  structure-print comparison alone is insufficient (both shapes *print*
  alike).
- **Replace**: successive `Unstable` deltas *replace* the region; nothing
  accumulates, and preedit text is never sent via `CommitText`.
- **Clear**: a `commit` hides the region first (the controller also flushes any
  pending stable burst *before* drawing the next preedit tail, so volatile text
  always lands after the committed text it extends); `cancel`/`end` hide it
  defensively; an empty `set_preedit("")` hides explicitly.
- **Same guards as commit**: the secure-purpose re-check (F2/I5) applies to
  preedit too — nothing, not even volatile text, is rendered into a
  known-secure field; after focus-loss both commits and preedit are suppressed
  (FR-014/SC-007).

This is the R9 extension the `Injector` seam was shaped for
(`set_preedit`/`supports_preedit`): no FSM or wire change — the controller
routes `OrchestratorEvent::Unstable` → `set_preedit` only when preedit is on
AND the backend reports `supports_preedit()` (IBus: yes; a future uinput/wtype
fallback: no). FR-012's commit-only guarantee is relaxed **only for the
preedit region**; every other injection invariant stands.

### When it is on (resolved, not asked)

Hypotheses only exist in streaming mode, so "show them in the field" is not a
separate preference — `myna-desktop` derives it from the persisted
`streaming_mode` through the RTF tier gate (`myna_core::effective_mode`, see
`docs/streaming-mode-settings.md`), with no server round-trip. Batch-resolving
machines stay commit-only; a batch-only backend never emits `Unstable` anyway,
so the two gates agree by construction.

`--preedit` / `--no-preedit` force it either way for debugging.

> **⚠ Open (UD136 / plan T64):** this makes in-field partials the default
> *wherever the tier gate opens streaming*, which is the substance of T64(a).
> What T64 still owns is (b) the differentiating formatting: on GNOME/Wayland
> the text-input-v3 path strips IBus preedit attributes, so the underline is
> app/toolkit-dependent and a cross-app-consistent "unstable look" may have to
> come from the shell extension instead. Nothing here settles that.

**Styling reality (for the design meeting, plan T64).** The preedit *text*
is delivered and replaced/committed correctly everywhere (probe-verified),
but the *differentiating style* is not ours to control on GNOME: on native
Wayland the app talks text-input-v3 to Mutter, which strips IBus preedit
attributes (ibus ChangeLog: "GNOME Wayland uses text-input version 3 which
deletes the preedit style") — GTK renders preedit with its own default
look, and our underline attribute is honored only where the app/toolkit
supports it (X11/XWayland IBus clients, some GTK paths). So a *consistent*
cross-app "unstable" visual may need to live in the shell extension's chrome
(feature 004) rather than in-field styling. Whether partials graduate from
opt-in to default, and with what visual treatment, is a UD136 design call. Hermetic coverage: `tests/controller.rs`
(`streaming_unstable_is_preedit_only_never_committed`,
`preedit_is_off_by_default_even_when_supported`,
`preedit_suppressed_with_commits_after_focus_loss`) + the GVariant shape test
in `inject/ibus.rs`; the live wire check is `MYNA_IBUS_TESTS=1 cargo test -p
myna-desktop --test ibus_hw ibus_preedit_cycle` (observable against a probe
`IBus.InputContext`, which receives the underlined updates verbatim), and
`ibus_preedit_visual_probe` renders replace-then-commit preedit into a real
focused field for the visual acceptance.

## Activation

Dictation injects into *another* app, so activation must not depend on terminal
focus. Three mechanisms, behind the reused `Trigger` seam:

- **Control socket + GNOME custom shortcut (default).** `shortcut::control::
  ControlTrigger` listens on a Unix socket; `myna-desktop --toggle` pokes it
  (first poke `Press`, next `Release` — toggle-to-talk). A GNOME custom keyboard
  shortcut bound to `myna-desktop --toggle` (via `--install-shortcut`, gsettings)
  fires it globally. Works for an unsandboxed binary: no terminal focus, no
  portal, no app id. This is the works-today path on GNOME/Wayland.
- **GlobalShortcuts portal (portal activation, R2).** `shortcut::portal::
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
wrong-target safety), cutting dictation short. The clean fix (an always-on-top
HUD that never takes focus) is the **`wlr-layer-shell`** protocol
(`gtk4-layer-shell`, `KeyboardMode::None`), as the reference Handy app does —
**but that only works on wlroots compositors and KWin; Mutter does not implement
layer-shell**, so it is not a fix on our primary target (GNOME). See the
platform survey below for why, and what the actually-portable options are. When
used it owns the process main thread + GLib loop, with the tokio controller on a
worker thread bridged by an `async-channel`; the error state also raises a
`notify-rust` toast.

## Wayland input-stealing: the platform landscape (survey)

Everything hard about the last-mile is one theme: **on Wayland the compositor
owns focus, input routing, and surface stacking; a client cannot reach outside
its own surface** the way an X11 client could (XTEST synthetic input, global key
grabs, override-redirect always-on-top). That isolation is deliberate. It splits
into three problems, each with a different protocol story and a different
GNOME/Mutter answer. This is a fast-moving area; the notes below are the state
as we found it and are the reason several choices above look like workarounds
rather than the "obvious" solution.

**1. Getting text *into* the focused app (injection).**
- *X11 legacy:* XTEST synthesises keystrokes into whatever is focused. No
  Wayland equivalent by design.
- *Wayland protocols:* `text-input-v3` (app-side; the app opts in to receive
  IM text — Mutter implements this, it's how committed text reaches GTK/Qt
  apps) vs `input-method-v2` (lets a *client* be the input method) and
  `virtual-keyboard-v1` (inject raw keycodes). The latter two are wlroots
  protocols and **Mutter implements neither** — GNOME routes input methods
  through **IBus** (in-compositor), not a client-facing Wayland protocol.
- *So on GNOME* the only sanctioned client→app text paths are **IBus** (be an
  engine — what we do), AT-SPI (accessibility; not designed for insertion,
  unreliable), or synthesising input via the **RemoteDesktop portal**
  (keycode/pointer emulation, not text commit).
- *The "right" upstream fix, once it settles:* for generic input emulation the
  clear direction is **libei/libeis** (emulated input) mediated by the
  `org.freedesktop.portal.RemoteDesktop` portal — cross-desktop,
  compositor-arbitrated, user-consented; Mutter already carries libei support.
  But libei is keystrokes/pointer, not *semantic text commit*; for dictation,
  committing text through an IM is more correct than faking keycodes (IME
  composition, non-Latin scripts, autocorrect fields). There is **no**
  cross-desktop *text-commit* portal yet, so IBus stays the GNOME path and
  `input-method-v2` the wlroots path (see the `Injector` seam in *Future*). The
  genuinely-unsettled question is whether a portable IM/text-injection
  interface ever standardises; until then the `Injector` trait is our
  portability boundary and IBus is the shipping backend.

**2. Showing UI without stealing focus (the indicator overlay).**
- *X11 legacy:* override-redirect + `_NET_WM_STATE_ABOVE` gives an always-on-top
  surface that never takes focus. No portable Wayland equivalent for ordinary
  clients.
- *Wayland protocols:* `wlr-layer-shell` (`zwlr_layer_shell_v1`) is the
  panel/OSD/overlay protocol with explicit keyboard-interactivity control
  (`KeyboardMode::None` = never focus). Implemented by wlroots compositors and
  **KWin** — **not by Mutter**. GNOME's long-standing position is that on-screen
  shell chrome belongs to **GNOME Shell extensions** (JS, in the compositor
  process), not arbitrary client surfaces, so it has declined to adopt
  layer-shell.
- *So on GNOME* there is currently **no sanctioned way for a normal client to
  show an always-on-top, non-focus-stealing overlay.** The realistic options are
  (a) a **GNOME Shell extension** for the indicator (the GNOME-blessed path, a
  separate JS component with its own packaging/review — **shipped**: see
  `extensions/myna-shell/` / feature 004, a bottom-center HUD pill consuming
  `org.myna.Dictation`), (b) lean on the **notification/OSD** facilities the
  shell already owns (what `NotifyIndicator` does — the fallback when the
  extension is absent), or (c) ship layer-shell for KDE/wlroots users and fall
  back to notifications on GNOME (`gtk4-layer-shell` gates on
  `is_supported()`; this is what Handy effectively does).
- *The "right" upstream fix, once it settles:* a **standardised cross-desktop
  layer-shell** in `wayland-protocols` (an `ext-`namespace successor has been
  discussed for years) that Mutter would implement — or GNOME converging on some
  other client-overlay mechanism. Nothing is merged/adopted, so this is the most
  genuinely-open of the three. Until then, treat the persistent overlay as
  **compositor-dependent** and keep notifications as the portable floor.

**3. Activating without focus (the global hotkey).**
- *X11 legacy:* any client could grab a global key.
- *Wayland:* the `org.freedesktop.portal.GlobalShortcuts` portal is the
  sanctioned cross-desktop answer and GNOME implements it — but the GNOME
  backend only serves callers with an **app identity** (Flatpak/snap or a
  `.desktop` launch), so a bare unsandboxed binary is refused.
- *This one is essentially settled:* the portal is the right interface; the only
  friction is app-identity. Hence the daemon resolves activation from
  packaging rather than asking: `$SNAP` set → portal; otherwise the **GNOME
  custom keybinding** (gsettings → `--toggle`) for the unsandboxed dev binary.
  `--portal` / `--control` / `--stdin` force one.

**Net:** activation is solved (portal, modulo packaging); injection is solved on
GNOME via IBus with a clean seam for the wlroots path and no portable successor
yet; the focus-safe overlay is the one with no good GNOME answer today, which is
why the shipped default is notifications and the overlay is opt-in and
compositor-dependent. Re-survey when: Mutter ships layer-shell (or an `ext-`
equivalent lands), a text-injection portal appears, or libei-based injection
becomes the norm for assistive input.

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

- **Wayland-native `input_method_v2`** backend for wlroots compositors (Mutter
  lacks it — IBus is the only path on GNOME; see the platform survey above) —
  addable behind the same `Injector` seam. The seam is exactly the portability
  boundary the survey argues for: IBus today, `input_method_v2` for wlroots, and
  whatever portable text-injection path standardises later, all behind
  `Injector` with no controller/FSM changes.
- **Focus-safe overlay** is compositor-dependent (survey §2): a `gtk4-layer-shell`
  backend would serve KDE/wlroots; GNOME needs a Shell extension or the
  notification floor. Kept behind the `Indicator` seam.
- **Streaming preedit**: **landed** (2026-07-27), and since 2026-08-24 no
  longer opt-in — it follows the streaming tier gate (see *Streaming preedit*
  above). The remaining piece is the UD136 hypothesis-*display* question
  (T64(b)): what differentiating formatting is achievable when Wayland's
  text-input-v3 strips preedit attributes.
