# Implementation Plan: Desktop Session Controller + Text Injection

**Branch**: `003-desktop-injection` | **Date**: 2026-07-19 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `/specs/003-desktop-injection/spec.md`

## Summary

Deliver the dictation last-mile (plan T21 + T22): a **desktop session controller**
that a global shortcut activates for push-to-talk, driving the *existing* client
capture→FSM→transcript path (feature 002 native capture + `myna-orchestrator`),
and a **text-injection backend** that inserts committed transcripts into the
application focused when the session started. Activation is the
`org.freedesktop.portal.GlobalShortcuts` portal (hold-to-talk: `Activated`=press,
`Deactivated`=release); injection is **IBus** behind a backend-agnostic
`Injector` seam; a small **activity indicator** shows recording/transcribing/
finalizing/error. The controller reuses the orchestrator's `Trigger`/`TextSink`
seams unchanged — the real portal hotkey and the IBus injector simply become the
production implementations of those traits. The legacy Python
`server/src/myna/desktop/` stubs are retired (their contract now lives in Rust).

## Technical Context

**Language/Version**: Rust (stable, workspace edition 2021, `rust-version = 1.75`)

**Primary Dependencies**:
- `zbus` (5.x, **already vendored**) — D-Bus; implements the IBus **engine**
  object (`org.freedesktop.IBus.Engine`) over the IBus private bus, and can serve
  as the portal client fallback.
- `ashpd` (0.x) — ergonomic XDG-portal client for `GlobalShortcuts`
  (CreateSession / BindShortcuts / Activated+Deactivated streams). *Network build
  dep*; `zbus`-direct is the vendored fallback (portal signatures verified in
  `org.freedesktop.portal.GlobalShortcuts.xml`).
- `gtk4` (0.11, **already vendored**) + `glib` — the activity-indicator overlay
  window; gated behind a `ui-gtk` Cargo feature so hermetic tests build without it.
- `notify-rust` (4.x, **already vendored**) — desktop notifications for error
  states / secure-field refusals.
- Existing workspace crates reused unchanged: `myna-core`, `myna-orchestrator`
  (FSM + `Trigger`/`TextSink` seams), `myna-audio` (native capture), `tokio`,
  `async-trait`, `thiserror`, `serde`.

**Storage**: N/A for audio (bounded in-memory ring reused; never persisted,
constitution V). The chosen shortcut binding is stored by the **portal**, not by
us; no app-owned settings store in scope (no Settings panel — spec Out of Scope).

**Testing**: `cargo test` hermetic suite driven by **mocks** at each boundary
(scripted `Trigger`, in-memory `Injector`, headless `Indicator`) — no D-Bus,
IBus, portal, or GTK required; plus an **env-gated integration suite**
(`MYNA_IBUS_TESTS=1`, `MYNA_PORTAL_TESTS=1`) exercised against a real IBus daemon
and a portal backend on the virtual-audio/desktop VM **and** on hardware without
code change (constitution II).

**Target Platform**: Ubuntu Desktop (current LTS+), Wayland session, **GNOME**
primary validated DE; IBus present (verified `ibus-1.0` 1.5.34, daemon running);
`xdg-desktop-portal` with a `GlobalShortcuts` backend (verified GNOME 50).

**Project Type**: Desktop application — Rust workspace; one new library+binary
crate `myna-desktop` composing three boundary modules behind traits, plus a
binary that wires them to the orchestrator.

**Performance Goals** (UD129 targets, pinned as watermarks — constitution III):
press→capture-start < 100 ms; activation→indicator-visible 100–200 ms; final
commit within 1–2 s after release (mostly inference-bound); no capture-path
regression versus feature 002 baselines; injection commit adds < 50 ms per
segment on reference hardware.

**Constraints**: offline-first, **no network** on the dictation path; **never
persist audio**; **commit-only** (no preedit in the target app); inject **literal
text only** — never synthesize unsafe key combos (Tab/Alt+Tab/Super/F-keys);
**target captured at session start**, no mid-session retarget; **push-to-talk
only** (mic captured only while a session is active); GTK must run on the process
main thread (tokio runtime on a worker thread, bridged by channels).

**Scale/Scope**: one new crate (~4 modules + bin), three trait boundaries each
with a mock, one env-gated integration suite, retirement of one Python package
(`myna.desktop`). Injection validated against an application matrix (GNOME Text
Editor, a GTK field, a browser field, a chat/Electron field, a terminal).

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

Constitution v1.3.0. This is a **shipped Rust system component**, so all
principles apply in full (the Python-harness carve-out does not apply; the Python
`myna.desktop` stubs being *removed* here are interface-only and were never
shipped runtime).

| Principle | Gate | Status |
|---|---|---|
| I. Red-Green TDD (post-ratification) | Every behavior-bearing change lands test-first. Each boundary is a trait with a mock; the controller's session lifecycle, autorepeat dedup, focus-change→end, secure-field refusal, and commit-only routing get failing tests before code. Contract guarantees (injection lifecycle, trigger edges, indicator states) encoded as executable tests first. | PASS (planned) |
| II. Integration-Test Readiness on Real Audio/Desktop Stacks | Boundaries behind traits (`Trigger`/`Injector`/`Indicator`); hermetic suite uses mocks (no D-Bus/IBus/portal/GTK). Real IBus + portal behavior in one env-gated suite that runs identically on the desktop VM and on hardware. | PASS (by design) |
| III. Performance Watermarks & Regression Sensitivity | Activation→indicator, press→capture, per-segment commit, and session-teardown latencies recorded as checked-in baselines with declared tolerances on the reference environments; SC-005 is the measurable target. Reuses feature-002 capture-path baselines. | PASS (planned) |
| IV. Workshop-Based Development Environment | New build/test deps must be in the Workshop definition: `libgtk-4-dev` (gtk4 build), a running **IBus daemon** + **xdg-desktop-portal** with a GlobalShortcuts backend for the gated integration suite, and D-Bus. `zbus`/`ashpd` need no system `-dev` headers. Extends `.workshop/myna.yaml` (added in feature 002). | GATED — tracked |
| V. Privacy-First, Offline-First Audio | Bounded in-memory ring reused; discarded at session end; no network on the dictation path; injector handles **text only**; diagnostics never log transcript content or raw audio by default. | PASS (by design) |

**Post-Phase-1 re-check**: see the end of this file — re-evaluated after the
design artifacts; no new violations introduced.

## Project Structure

### Documentation (this feature)

```text
specs/003-desktop-injection/
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── contracts/           # Phase 1 output (Rust API + external-boundary contracts)
│   ├── injector.md      # Injector seam + IBus backend guarantees
│   ├── trigger.md       # GlobalShortcuts portal Trigger guarantees
│   └── indicator.md     # Activity-indicator seam guarantees
├── checklists/
│   └── requirements.md  # from /speckit-specify
└── tasks.md             # /speckit-tasks output (NOT created here)
```

### Source Code (repository root)

```text
client/
├── myna-core/                 # UNCHANGED (wire contract + consumer traits)
├── myna-audio/                # UNCHANGED (native PipeWire capture, feature 002)
├── myna-orchestrator/         # UNCHANGED seams reused:
│   └── src/
│       ├── trigger.rs         #   Trigger (Press/Release edges) — portal impls it
│       ├── sink.rs            #   TextSink (OrchestratorEvent) — inject adapter feeds it
│       └── fsm.rs / runner.rs #   session/residency FSM + run_dictation
├── myna-desktop/              # NEW crate (lib + bin) — the T21/T22 deliverable
│   ├── Cargo.toml             #   deps: myna-orchestrator, myna-audio, zbus, ashpd,
│   │                          #   gtk4 (feature `ui-gtk`), notify-rust, tokio
│   ├── src/
│   │   ├── lib.rs             #   re-exports; wires the boundaries to the controller
│   │   ├── controller.rs      #   DesktopController: session lifecycle + state model
│   │   ├── inject/            #   Injector trait + backends
│   │   │   ├── mod.rs         #     Injector seam (acquire/indicate/commit/cancel/end)
│   │   │   ├── ibus.rs        #     IBus engine over zbus (the shipped backend)
│   │   │   └── mock.rs        #     in-memory Injector for hermetic tests
│   │   ├── shortcut/          #   Trigger backends
│   │   │   ├── mod.rs
│   │   │   ├── portal.rs      #     GlobalShortcuts portal Trigger (ashpd/zbus)
│   │   │   └── (ScriptedTrigger reused from orchestrator for tests)
│   │   ├── indicator/         #   Activity indicator
│   │   │   ├── mod.rs         #     Indicator trait (state updates)
│   │   │   ├── gtk.rs         #     GTK4 overlay window (`ui-gtk` feature)
│   │   │   ├── notify.rs      #     notify-rust error notifications
│   │   │   └── mock.rs        #     headless Indicator for hermetic tests
│   │   └── bin/
│   │       └── myna-desktop.rs #    the shipped push-to-talk app
│   └── tests/
│       ├── controller.rs      #   hermetic: lifecycle, dedup, focus-end, commit-only
│       ├── ibus_hw.rs         #   env-gated (MYNA_IBUS_TESTS): real IBus commit/focus/secure
│       └── portal_hw.rs       #   env-gated (MYNA_PORTAL_TESTS): real bind/activate/deactivate
└── Cargo.toml                 # + myna-desktop member

server/src/myna/desktop/       # REMOVED (FR-025): controller.py, textout.py, __init__.py
```

**Structure Decision**: One new workspace crate `myna-desktop` (mirroring feature
002's "add within the existing workspace" precedent), composing three trait
boundaries — `Injector`, `Trigger` (orchestrator's), `Indicator` — each with a
mock, so the controller's logic is fully hermetic-testable and the real
IBus/portal/GTK integrations are isolated behind env-gated suites and a Cargo
feature. The controller is the production analogue of `runner::run_dictation`,
specialized for the desktop (persistent multi-session loop, focus/secure-field
policy, indicator lifecycle) rather than the one-shot demo. `myna-cli` stays the
loopback/testbed demo; `myna-desktop`'s binary is the shipped dictation app.

## Complexity Tracking

> Only rows that need constitutional justification.

| Violation / Risk | Why Needed | Simpler Alternative Rejected Because |
|---|---|---|
| **IBus engine implemented over raw D-Bus (`zbus`)** — hand-written `org.freedesktop.IBus.*` interface definitions (IBus ships no introspection XML) | The shipped component must be Rust (constitution) and IBus is the UD129-mandated backend; `zbus` is pure-Rust, already vendored, needs no FFI/GObject-introspection and spawns no subprocess (consistent with feature 002 retiring the `pw-record` subprocess). | (a) `libibus` FFI + a GLib main loop — no maintained Rust binding, GObject-introspection gap, and a GLib loop competing with tokio; (b) a Python IBus helper subprocess (like the PoC) — reintroduces Python + a subprocess into a shipped component, the opposite of feature 002's direction; (c) virtual-keyboard/uinput (`wtype`/`ydotool`, as Handy) — loses IBus's focus-in/out + content-type signals that FR-014/FR-021 depend on, and synthesizes keystrokes (unsafe-combo risk). Kept as a possible *future* backend behind the same `Injector` seam. |
| **GTK4 dependency for the activity indicator** | UD129 requires a persistent, screen-reader-perceivable indicator with distinct recording/transcribing/finalizing/error states; transient notifications can't express a live state. gtk4-rs is vendored and mature. | Notifications-only (`notify-rust`) — transient, not a persistent state surface, poor a11y for "still listening"; kept for *error* toasts. Gated behind a `ui-gtk` feature so hermetic tests and the injector/trigger crates never pull GTK. |
| **GTK main-thread vs tokio runtime** | GTK must own the process main thread + GLib loop; the orchestrator is tokio-async. | Running GTK off-main-thread is fragile/unsupported; instead the binary runs GTK on main and the tokio runtime on a worker thread, bridged by channels — a standard gtk4-rs+tokio pattern, isolated to the binary. |
| **Real IBus/portal/GTK can't be hermetically unit-tested** | Injection, global-shortcut binding, and overlay rendering only exist against a live IBus daemon / portal / display. | Mocking them tests the mock, not the integration where the bugs live (constitution II rationale). Real behavior goes in one env-gated suite that runs on the VM and on hardware; hermetic coverage stays on the trait seams via mocks. |
| **New system deps not yet in Workshop** (`libgtk-4-dev`, IBus daemon, xdg-desktop-portal + GlobalShortcuts backend, D-Bus for the gated suite) | Constitution IV mandates the Workshop definition gain deps in the PR that introduces them; feature 002 established `.workshop/myna.yaml`. | Deferring violates IV; scoped as a foundational task extending the existing Workshop definition. |

## Constitution re-check (post-design)

Re-evaluated after Phase 1 (research + data-model + contracts + quickstart):

- **I. TDD** — the three contracts (`injector.md`, `trigger.md`, `indicator.md`)
  are row-per-guarantee test tables; controller logic (dedup, focus-end,
  secure-refusal, commit-only) is hermetic with mocks. Tests precede code. PASS.
- **II. Integration readiness** — hermetic coverage on the trait seams via mocks;
  real IBus/portal behavior in env-gated suites identical on VM and hardware. PASS.
- **III. Watermarks** — quickstart step 5 + SC-005 pin activation/commit/teardown
  latencies with tolerances; a perf test is a planned task; capture-path baselines
  inherited from feature 002. PASS.
- **IV. Workshop** — the one open gate: `libgtk-4-dev` + IBus/portal/D-Bus test
  services must be declared in `.workshop/myna.yaml`. Scheduled as a foundational
  task in the same increment. GATED until it lands.
- **V. Privacy/offline** — no new persistence or network; injector is text-only;
  audio ring reused and discarded at session end; diagnostics content-free by
  default. PASS.

No principle is violated by the design; the sole tracked item (IV) is a
known-missing artifact this feature is the correct occasion to extend.
