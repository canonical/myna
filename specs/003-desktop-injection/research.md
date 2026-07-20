# Phase 0 Research: Desktop Session Controller + Text Injection

**Feature**: 003-desktop-injection · **Date**: 2026-07-19

Consolidated technical decisions for T21 (session controller + global-shortcut
activation) and T22 (IBus injection + activity indicator). Each entry: the
decision, why, and the alternatives weighed. Grounded in the reference code
(`reference/speech-to-text-poc`, `reference/Handy`), the upstream sources under
`~/probe/ubuntu` (gnome-shell, xdg-desktop-portal, wayland-protocols), and the
verified host state (GNOME 50, Wayland, `ibus-1.0` 1.5.34 with a live daemon).

## R1 — Text-injection backend: IBus engine over D-Bus (`zbus`)

**Decision**: Implement the shipped injector as an **IBus engine** that speaks
the IBus wire protocol (D-Bus / GVariant) directly via the already-vendored
`zbus`. The engine registers an IBus component + engine, and per session is made
the active engine, commits committed segments through `CommitText`, and is
restored on session end. Focus and secure-field state come from the engine's
`FocusIn`/`FocusOut` and `SetContentType` callbacks.

**Rationale**:
- **Constitution-clean**: pure Rust, no FFI, no GObject-introspection gap, and
  **no subprocess** — consistent with feature 002 retiring the `pw-record`
  subprocess. `zbus` is already in the vendored cache.
- **UD129-mandated**: IBus is the iteration-1 backend; the reference PoC
  (`ibus/src/input_methods/ibus/sttengine.py`) proves an STT IBus engine commits
  text across GNOME apps on Ubuntu 24.04.
- **Signals we need for free**: `FocusIn`/`FocusOut` give the focus-change
  detection FR-014 requires; `SetContentType(purpose, hints)` exposes
  `IBUS_INPUT_PURPOSE_PASSWORD` for the secure-field refusal FR-021 requires.
- **`CommitText`** inserts stable text without preedit — exactly the commit-only
  MVP (FR-012); the engine simply never calls `UpdatePreeditText`.

**Alternatives considered**:
- **`libibus` FFI + GLib loop** — rejected: no maintained Rust binding; IBus
  ships no introspection XML (verified: none under `/usr/share/dbus-1`), so
  bindings would be hand-written anyway; a GLib main loop would compete with
  tokio. With `zbus` we hand-write only the small set of interfaces we use.
- **Python IBus helper subprocess** (as the PoC) — rejected: reintroduces Python
  + a subprocess into a shipped component, the opposite of feature 002.
- **Virtual keyboard / uinput** (`wtype`/`ydotool`/`xdotool`, as `reference/Handy`
  does) — rejected for iteration 1: synthesizes keystrokes (unsafe-combo and
  layout/Unicode hazards, FR-015), and loses the focus + content-type signals.
  Kept as a *future* alternate backend behind the same `Injector` seam.

**Open implementation risks** (for the plan/tasks, not blockers): making our
engine the active one uses IBus global-engine control (`set_global_engine` in the
PoC); the set/restore must be prompt and must not strand the user's prior IME if
the session aborts — covered by the `Injector::end`/`cancel` idempotency contract
(C-lifecycle) and an env-gated restore test.

## R2 — Activation: `org.freedesktop.portal.GlobalShortcuts` (hold-to-talk)

**Decision**: Activate via the **GlobalShortcuts portal**. Create a session,
`BindShortcuts` one `dictate` shortcut carrying a `preferred_trigger`, and treat
the `Activated` signal as press and `Deactivated` as release — driving the
orchestrator's `Trigger` (`Press`/`Release`). Prefer the `ashpd` crate for
ergonomics; fall back to `zbus`-direct (portal signatures verified in
`org.freedesktop.portal.GlobalShortcuts.xml`: `Activated`/`Deactivated` carry
`session_handle`, `shortcut_id`, `timestamp`).

**Rationale**:
- **Source-verified on GNOME 50** (recorded in `docs/audio-adapter-meeting-prep.md`,
  plan T21): the portal backend grabs the accelerator through
  xdg-desktop-portal-gnome → gnome-shell → mutter, delivering both press
  (`Activated`) and release (`Deactivated`) — so hold-to-talk needs **no shell
  extension** and no privileged key grab (impossible for apps on Wayland).
- **Rebinding UI is the desktop's**: `BindShortcuts`/`ConfigureShortcuts` surface
  the compositor's own dialog — the feature ships no shortcut-config UI (spec Out
  of Scope), satisfying "activatable using a GlobalShortcut entry."
- Maps cleanly onto the existing `Trigger` trait (`trigger.rs`), so the FSM/
  controller are untouched by the activation mechanism.

**Alternatives considered**:
- **Direct compositor/X11 grab** — impossible under the Wayland security model
  (apps can't grab global keys); rejected.
- **A GNOME Shell extension** — heavier, GNOME-specific, needs packaging/review;
  unnecessary since the portal already delivers press+release. Rejected.

**Notes for implementation**:
- **Autorepeat dedup** (FR-008): mutter may re-emit `Activated` while the key is
  held; the portal Trigger collapses repeats to one `Press` until a `Deactivated`
  (first-Activated-wins), matching the meeting-prep guidance.
- **Default binding** (FR-009): a free 2-key `Super+<letter>` (not modifier-only —
  breaks apps); exact letter chosen by enumerating taken GNOME combos, confirmed
  by the user at bind time via `preferred_trigger`. The concrete default is a
  tasks-phase pick, not a blocker.
- **`ashpd` is a network build dep** (not currently vendored; its deps
  `async-channel`/`enumflags2` are). If offline builds must be preserved,
  `zbus`-direct is the vendored fallback — decided at task time.

## R3 — Upstream Wayland review: why not text-input-v3 / input-method-v2 now

**Decision**: Use IBus for iteration 1; keep a **Wayland-native input-method
backend as future/portability work** behind the `Injector` seam, not delivered
here.

**Findings** (from `~/probe/ubuntu` + `/usr/share/wayland-protocols`):
- **`zwp_text_input_v3`** (`text-input-unstable-v3.xml`) is the **application↔
  compositor** protocol — *apps* (the dictation *target*) speak it to receive
  `commit_string`/`preedit_string`. It is **not** an interface a dictation service
  implements; we are on the input-method side, not the app side.
- **The input-method side** is `zwp_input_method_v1` (`input-method-unstable-v1`,
  effectively deprecated) and **`zwp_input_method_v2`** (`xx-input-method-v2`,
  wlroots). **mutter/GNOME does not implement `input_method_v2`** for third-party
  input methods — on GNOME the *only* practical text-injection path is **IBus**
  (GNOME's own IME stack bridges to IBus internally). wlroots compositors (sway,
  etc.) do implement v2.
- Therefore a Wayland-native backend would (a) not work on the primary target
  (GNOME) today and (b) only pay off for wlroots portability — squarely future
  work. `text-input-v3` remains irrelevant to *our* side regardless.

**Consequence for design**: the `Injector` trait is backend-agnostic (FR-016) so
`input_method_v2` can be added later for wlroots without touching the controller,
but iteration 1 ships only the IBus backend.

## R4 — Focus-change & wrong-target safety

**Decision**: Capture the target at session start; **never retarget**. On a
`FocusOut` from the captured input context during an active session, **end the
session safely** — finalize already-committed text, discard the rest — rather
than commit into whatever now has focus.

**Rationale**: IBus routes `CommitText` from the active engine to the *currently
focused* input context. IBus therefore *follows focus*; it cannot guarantee
commit into the original surface after a focus change. UD129 permits either
"keep targeting the original surface" **or** "cancel/finalize safely, depending
on what the backend can guarantee" — since IBus can't guarantee the former, the
safe branch (end the session) is chosen (FR-014, SC-007). The engine's
`FocusOut` callback (present in the PoC as `do_focus_out`) is the detection hook.

**Alternatives considered**: buffering commits until the original surface
refocuses — rejected: unbounded latency, and the user has clearly moved on;
worse UX than a clean end.

## R5 — Secure-field detection

**Decision**: Refuse to start (or immediately end) a session when the focused
input context advertises a password/secure purpose, via the engine's
`SetContentType(purpose, hints)` callback — refuse when `purpose ==
IBUS_INPUT_PURPOSE_PASSWORD` (and treat PIN/date/etc. per policy). Where no
content-type is advertised, protection is best-effort and the residual risk is
documented (FR-021, UD129).

**Rationale**: `zwp_text_input_v3.set_content_type` (purpose enum incl.
`password`) flows through GNOME to the IBus engine as `SetContentType`; this is
the same signal GNOME uses to disable the on-screen keyboard suggestions in
password fields. It is the only portable secure-field hint available on the IBus
path. Lock screen / polkit prompts additionally don't expose an editable IBus
context to third-party engines, so they are inert by construction.

**Alternatives considered**: AT-SPI accessibility introspection of the focused
widget — rejected for the MVP: heavier, racy, and GNOME already funnels the
purpose through the content-type signal.

## R6 — Activity indicator surface on GNOME Wayland

**Decision**: A small **GTK4 overlay window** (borderless, always-on-top,
non-focusable) as the persistent indicator with distinct recording/transcribing/
finalizing/error visuals, plus **`notify-rust` notifications** for error/refusal
toasts. GTK gated behind a `ui-gtk` Cargo feature.

**Rationale**:
- GNOME has **no system tray** (StatusNotifierItem needs an extension) and mutter
  **does not implement `wlr-layer-shell`**, so `gtk4-layer-shell` overlays are
  out. A plain GTK4 top-level positioned near a screen edge is the portable,
  no-extension option, and GTK4 exposes accessibility (AT-SPI) so the indicator
  is screen-reader-perceivable (FR-019).
- `gtk4` + `notify-rust` are already vendored.

**Alternatives considered**:
- **Tray icon (`ksni`/StatusNotifierItem)** — needs a GNOME extension
  (AppIndicator) to be visible; rejected as a hard dependency, viable as a later
  add.
- **Notifications only** — transient; can't express "still listening" as a
  persistent state; poor a11y for the core signal. Kept for errors only.
- **layer-shell overlay** — unsupported by mutter. Rejected.

**Integration note**: GTK owns the process main thread + GLib loop; the tokio
runtime runs on a worker thread; the two are bridged with channels (indicator
state pushed to GTK, trigger edges pulled from the portal task). This is the
standard gtk4-rs + tokio arrangement and is isolated to the `myna-desktop` binary
and the `indicator::gtk` module. Hermetic tests use the headless `mock::Indicator`
with the `ui-gtk` feature off.

## R7 — Session controller shape (reuse vs. new)

**Decision**: A new `DesktopController` that is the **production analogue of
`runner::run_dictation`** — but a persistent, multi-session loop rather than the
one-shot demo. It consumes the orchestrator's `Trigger` (edges) and drives the
existing FSM/`run_dictation` machinery per utterance, routing `OrchestratorEvent`s
to both the `Injector` (commit-only) and the `Indicator`, and enforcing the
focus/secure-field policy.

**Rationale**: the orchestrator already proved trigger→FSM→sink end-to-end
(`tests/dictation_e2e.rs`, plan T41) with capture-gated-on-`ready`. Reusing it
keeps the wire/FSM untouched (invariant: "the Rust FSM is untouched") and
confines new logic to desktop policy (multi-session lifecycle, focus/secure
rules, indicator lifecycle). The `TextSink` seam is the join point: an adapter
turns `OrchestratorEvent::{Final,Done}` into `Injector::commit` and everything
else into `Indicator` updates.

**Alternatives considered**: extend `myna-cli` with a `--desktop` mode — rejected:
mixes the loopback/testbed demo with the shipped app and drags GTK/portal deps
into the demo binary. A dedicated crate keeps concerns and test surfaces clean.

## R8 — Legacy Python `myna.desktop` retirement

**Decision**: Delete `server/src/myna/desktop/{__init__,controller,textout}.py`
(the `DictationState` enum + `TextInjector` Protocol). The contract they sketched
now lives in Rust (`DesktopController` state model + `Injector` trait).

**Rationale**: they are **interface-only stubs** with no runtime importers — the
server, testbed, and tests do not import `myna.desktop` (verified: only the
package's own `__init__` references them). Removing them is safe (FR-025, SC-010)
and matches "this now belongs to the client." Their good ideas (commit-only,
target-fixed-at-start, secure-field refusal, indicator lifecycle) are carried
into the Rust `Injector` contract, so nothing is lost.

**Verification task**: `uv run pytest` + server import smoke stays green after
removal.

## R9 — Future extension: streaming preedit (partial-then-commit)

**Decision**: The MVP is commit-only (FR-012), but the `Injector` seam is shaped
**now** to accommodate a future streaming UX where in-flight hypotheses render as
provisional text in the target and are rewritten/replaced on finalization —
without reshaping the seam or the FSM later. Preedit is modeled as an **optional
backend capability**, not a required method.

**Why it already fits**:
- **Partial text already reaches the controller**: the internal vocab's
  `transcription.progress` unstable `snippet` is already surfaced as
  `OrchestratorEvent::Snippet(text)` (the `StdoutSink` even renders it). Streaming
  is "route `Snippet` to the injector's preedit channel," not a wire change.
- **The IBus backend natively supports it**: the reference PoC uses
  `UpdatePreeditText` for in-flight segments and `CommitText` on completion. Our
  shipped backend *is* an IBus engine — the MVP simply never calls
  `UpdatePreeditText`.
- **No change to the state model, focus capture, or secure-field rules**: preedit
  still targets only the surface captured at session start; secure fields still
  refuse.

**Crucial distinction — preedit is NOT the retraction we dropped**: the project
dropped `partial`/`replace`/**epoch retraction of committed text** ("`Final` is
never retracted") as confusing — that invariant stands. IME **preedit** is
different: volatile text the compositor/app owns in a dedicated preedit region
and clears on commit; it is **replacement-safe by the backend**, which is exactly
UD129's caveat ("provisional rendering where replacement safety is guaranteed by
the backend"). So streaming preedit does not reopen retraction.

**What to do now (cheap-future shaping)**:
- Add to the `Injector` trait a future, no-op-default `set_preedit(&str)` plus a
  `supports_preedit() -> bool` capability flag (see `contracts/injector.md`).
- IBus / (future) Wayland `input_method_v2` → `supports_preedit()==true`, real
  partial-then-commit.
- A uinput/`wtype` fallback backend (R1) → `supports_preedit()==false`, degrades
  cleanly to commit-only — it has no safe preedit region (rewriting would mean
  select-and-retype, which is unsafe). **This is why the capability gate matters**:
  streaming is only sound where a real preedit region exists.
- Keep FR-012 (commit-only) as an MVP *scope* choice; a later iteration flips the
  capability on rather than reshaping the seam.

**Semantic detail deferred to the streaming iteration** (not a blocker now):
whether `snippet` represents the current in-flight *segment* hypothesis vs. the
whole-utterance-so-far — preedit wants the former (the uncommitted tail), matching
the PoC's per-segment `completed` flag. The *shape* (a single replaceable string)
is already correct.

**Alternatives considered**: making `set_preedit` a required trait method now —
rejected: forces every backend (incl. the commit-only uinput fallback) to fake
preedit, and bakes a capability not all backends can honor. Optional-capability
is the clean seam.

## Summary of decisions

| # | Area | Decision |
|---|------|----------|
| R1 | Injection backend | IBus engine over `zbus` (pure Rust, no subprocess) |
| R2 | Activation | GlobalShortcuts portal, hold-to-talk, `ashpd` (zbus fallback) |
| R3 | Wayland-native | Deferred — mutter lacks `input_method_v2`; IBus only on GNOME |
| R4 | Focus safety | Target fixed at start; `FocusOut` → end session safely |
| R5 | Secure fields | Refuse on `SetContentType` password purpose; best-effort otherwise |
| R6 | Indicator | GTK4 overlay (`ui-gtk` feature) + `notify-rust` errors |
| R7 | Controller | New `DesktopController` reusing the orchestrator FSM/runner |
| R8 | Legacy cleanup | Delete `server/src/myna/desktop/*` (no runtime dependents) |
| R9 | Streaming preedit (future) | Seam shaped now: optional `set_preedit`/`supports_preedit`; MVP stays commit-only |

All Technical-Context unknowns are resolved; no NEEDS CLARIFICATION remains.
