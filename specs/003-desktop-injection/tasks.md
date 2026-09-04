# Tasks: Desktop Session Controller + Text Injection

**Input**: Design documents from `/specs/003-desktop-injection/`
**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/, quickstart.md

**Tests**: REQUIRED — this is a shipped Rust system component, so constitution
Principle I (Red-Green TDD) applies in full. Every behavior-bearing task is
preceded by a failing test. Hermetic tests drive the controller and boundary
logic through mocks (`MockInjector`/`MockIndicator`, the orchestrator's
`ScriptedTrigger`) and pure mapping functions — no D-Bus, IBus, portal, or
display. Real IBus/portal/GTK behavior is proven by env-gated integration suites
(`MYNA_IBUS_TESTS` / `MYNA_PORTAL_TESTS` / display gate) that run identically on
the desktop VM and on hardware (Principle II).

**Organization**: Tasks grouped by user story (US1–US4) for independent
implementation and testing. Priority order: US1 (P1 🎯 MVP) → US2 (P2) → US3 (P2)
→ US4 (P3).

## Format: `[ID] [P?] [Story] Description`

- **[P]**: can run in parallel (different files, no dependency on incomplete tasks)
- **[Story]**: US1–US4 for story-phase tasks; Setup/Foundational/Polish carry none
- All paths are repo-relative; new crate is `client/myna-desktop` unless noted

## Path Conventions

- New crate: `client/myna-desktop/` (`src/`, `src/bin/`, `tests/`)
- Reused seams: `client/myna-orchestrator/src/{trigger,sink,fsm,runner}.rs` (unchanged)
- Reused capture: `client/myna-audio/` (feature 002, unchanged)
- Env-gated suites: `client/myna-desktop/tests/{ibus_hw.rs,portal_hw.rs,indicator_hw.rs}`
  (gates: `MYNA_IBUS_TESTS=1`, `MYNA_PORTAL_TESTS=1`, display-present)
- Legacy removal: `server/src/myna/desktop/`

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: environment + crate + dependency + module scaffolding so all stories build.

- [X] T001 Extend the Workshop definition (constitution Principle IV) in `.workshop/myna.yaml` / `.workshop/pipewire/hooks/setup-base` to add this feature's system deps: `libgtk-4-dev` (gtk4 build), and the gated-suite runtime services — a running IBus daemon and an `xdg-desktop-portal` with a GlobalShortcuts backend + D-Bus session bus. Validate `workshop launch myna` builds and `cargo build -p myna-desktop` links (plan Complexity Tracking Workshop row)
- [X] T002 Create the `client/myna-desktop` crate: add it to `[workspace] members` in `client/Cargo.toml`; write `client/myna-desktop/Cargo.toml` depending on `myna-orchestrator`, `myna-audio`, `myna-core`, `tokio`, `async-trait`, `thiserror`, `futures-util`, `zbus`, `notify-rust`, and `gtk4`+`glib` **behind a default `ui-gtk` feature**; add `ashpd` (portal) — note the network-build caveat in a comment, `zbus`-direct fallback allowed. Confirm `cargo build -p myna-desktop --no-default-features` and `--features ui-gtk` both link
- [X] T003 [P] Scaffold empty modules in `client/myna-desktop/src/`: `lib.rs` (mod wiring + `pub use`), `controller.rs` (declare `pub struct DesktopController`, `pub enum DictationState`), `inject/mod.rs` (`pub trait Injector`, `InjectionTarget`, `FocusEvent`, `InjectError`), `inject/{ibus,mock}.rs`, `shortcut/{mod,portal}.rs`, `indicator/mod.rs` (`pub trait Indicator`, `IndicatorState`), `indicator/{gtk,notify,mock}.rs`, `bin/myna-desktop.rs`. GTK modules `#[cfg(feature = "ui-gtk")]`. Everything compiles as stubs under both feature settings
- [X] T004 [P] Create the env-gated integration test files `client/myna-desktop/tests/{ibus_hw.rs,portal_hw.rs,indicator_hw.rs}` with gate helpers (`MYNA_IBUS_TESTS`, `MYNA_PORTAL_TESTS`, display-present) that skip cleanly when unset, so the suites compile/run as no-ops offline

**Checkpoint**: workspace builds with the new crate under both feature settings; modules and gated harnesses exist and compile; existing suites still green.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: the controller skeleton + boundary traits + mocks + event-routing
adapter that ALL stories depend on. No user story can start until this is done.

**⚠️ CRITICAL**: complete before ANY user story phase.

- [X] T005 Unit tests for the `DictationState` machine in `client/myna-desktop/src/controller.rs` (`#[cfg(test)]`): every legal transition from data-model.md accepted; every illegal transition rejected (is a bug). **Write first, observe fail, then satisfy with T006**
- [X] T006 Implement `DictationState` + the legal-transition table in `client/myna-desktop/src/controller.rs` (data-model.md state model: Idle/Starting/Recording/Transcribing/Finalizing/Completed/Cancelled/Error) (satisfies T005; FR-005). Carries the vocabulary migrated from the retired Python `DictationState`
- [X] T007 [P] Implement `MockInjector` in `client/myna-desktop/src/inject/mock.rs`: scripts `acquire` outcomes (ok / `SecureField` / `NoTarget` / `Unavailable`) + a scripted `focus_events` stream; records `commit`/`set_activity`/`cancel`/`end` calls; `supports_preedit()==false`, `set_preedit` no-op (contract injector.md; used by all hermetic controller tests)
- [X] T008 [P] Implement `MockIndicator` in `client/myna-desktop/src/indicator/mock.rs`: records the `IndicatorState` sequence + `hide()`; no GTK (built with `ui-gtk` off) (contract indicator.md N-mapping tests)
- [X] T009 [P] Unit tests for the `OrchestratorEvent → IndicatorState` mapping helper in `client/myna-desktop/src/controller.rs`: `Loading`→preparing/`Recording`, `Ready`→`Recording`, `Transcribing`→`Transcribing`, `Finalizing`→`Finalizing`, `Done`→`Hidden`, `Error{msg}`→`Error(msg)`. **Write first, observe fail, then satisfy with T010** (contract indicator.md)
- [X] T010 Implement the `DesktopController` skeleton + the event-routing adapter in `client/myna-desktop/src/controller.rs`: a builder taking `Box<dyn Trigger>` + `Box<dyn Injector>` + `Box<dyn Indicator>` + a session factory; the per-utterance loop (`Press`→acquire→start `run_dictation`→route events→`Release`/terminal→finalize→Idle) driving the T006 state machine; routes `OrchestratorEvent::{Final,Done}` to `Injector::commit` and all states to `Indicator::set_state` via the T009 mapping (satisfies T009; FR-001, FR-002, FR-003). Reuses `myna-orchestrator` `run_dictation`/FSM unchanged

**Checkpoint**: the controller composes three mocked boundaries and runs a full mocked session hermetically; state machine + event mapping green offline. Stories can now begin.

**Reuse note**: capture-at-press + push-gated-on-`ready` is behavior of the
**unchanged** orchestrator FSM/`run_dictation` (plan T41), not re-implemented here;
the controller only composes it. Asserted in T012 (buffered cold-load speech is
injected once, not lost) so the reuse is verified.

---

## Phase 3: User Story 1 — Speak into the focused application (Priority: P1) 🎯 MVP

**Goal**: committed transcripts inserted via IBus into the app focused at session
start, driven by a stand-in trigger (the orchestrator's `StdinTrigger`), with a
headless indicator — the dictation last-mile, shippable before the hotkey/UI land.

**Independent Test**: with a focused text field and a running `myna-server`, run
`myna-desktop` driven by stdin, speak a known utterance, end; assert the committed
transcript appears in that field, in order, once, and nothing is typed elsewhere.

### Tests for User Story 1 (write first, must fail) ⚠️

- [X] T011 [P] [US1] Hermetic controller test (`MockInjector`): a scripted session with two `Final` segments + `Done` calls `commit` twice, in order, each once, and never re-commits — `client/myna-desktop/tests/controller.rs` (contract I2)
- [X] T011a [P] [US1] Hermetic controller test (**FR-004 / SC-004 — push-to-talk, no background listening**): a capture-recording mock audio source asserts **no capture is started while `Idle`**, and capture exists **only** between `Press` and `Release`/terminal, across multiple session cycles — `client/myna-desktop/tests/controller.rs`. (Closes the privacy-coverage gap: the mic-only-while-active guarantee is asserted, not merely implied by the T010 loop)
- [X] T012 [P] [US1] Hermetic controller test: `Loading`→`Ready` cold-load window with buffered audio yields exactly one eventual `commit` of the transcript (nothing lost; reuse of capture-at-press verified) (contract I1 shape; FR-002, US1-3)
- [X] T013 [P] [US1] Hermetic controller test: an `OrchestratorEvent::Snippet` is **never** routed to `Injector::commit` (commit-only) (contract I3; FR-012, SC-006, US1-4)
- [X] T014 [P] [US1] Hermetic controller test: a no-speech session (`Done` with empty text, no `Final`) performs no `commit` and ends clean (contract I4; US1-5)
- [X] T015 [P] [US1] Hermetic controller test: `acquire` → `Err(NoTarget)` and `Err(Unavailable)` each surface a clear `Error` state and abort the session without capturing (contracts I6, I7; FR-023)
- [X] T016 [P] [US1] Hermetic test: `MockInjector` asserts only literal text is passed to `commit` (no key-combo tokens synthesized) and that `cancel`/`end` are idempotent + restore-once on the error path (contracts I10, I11; FR-015, FR-013)
- [X] T017 [P] [US1] Integration test (gated `MYNA_IBUS_TESTS`) in `tests/ibus_hw.rs`: `IbusInjector` acquires a focused test entry, commits "hello", ends, and the entry contains exactly "hello"; global-engine restored afterward (contracts I1, I11; SC-001)

### Implementation for User Story 1

- [X] T018 [US1] Implement `IbusInjector` in `client/myna-desktop/src/inject/ibus.rs`: register an IBus component + engine over `zbus` (hand-written `org.freedesktop.IBus.*` interfaces — research R1); `acquire` makes it the active engine + reads the focused context; `commit` → `CommitText`; `set_activity` maps to the engine activity channel; `end`/`cancel` restore the prior engine (idempotent). `supports_preedit()==true`, `set_preedit` a no-op for now (R9 seam, not wired) (satisfies T017; contracts I1, I11)
- [X] T019 [US1] Implement `NotifyIndicator` in `client/myna-desktop/src/indicator/notify.rs` (`notify-rust`): headless error/state toasts so the controller runs without GTK (the MVP indicator; the GTK overlay is US3) (FR-020 fallback)
- [X] T020 [US1] Wire `client/myna-desktop/src/bin/myna-desktop.rs`: compose the orchestrator's `StdinTrigger` (stand-in for the portal hotkey) + `IbusInjector` + `NotifyIndicator` + the capabilities-negotiated audio source (feature 002 `PipeWireBackend`) into `DesktopController`; flags `--socket`/`--language` (FR-001; capabilities-negotiate the `input_format` per plan T21 acceptance)
- [X] T021 [US1] Run quickstart steps 1–3 for the MVP path: hermetic suite green (`--no-default-features`); gated `ibus_hw` green against the live IBus daemon; one **manual spoken run** through `myna-server --adapter whisper` asserting the committed transcript lands in a focused GNOME Text Editor with nothing typed elsewhere (SC-001, SC-002). **Privacy (FR-024 / SC-009)**: no audio is persisted and the in-memory buffer is released at session end — this is **inherited from feature 002's capture path** (the bounded ring + `AudioStats`, already tested there: nothing written to disk); 003 relies on that baseline and adds no new persistence, so no 003-level disk-write assertion is duplicated. Confirm the inheritance holds during the spoken run

**Checkpoint**: US1 fully functional and independently testable — spoken words land as committed text in the focused app via IBus, driven by stdin. **MVP reached.**

---

## Phase 4: User Story 2 — Activate hands-free with a global shortcut (Priority: P2)

**Goal**: replace the stand-in trigger with a real `GlobalShortcuts` portal
binding — hold-to-talk, rebindable through the desktop's own shortcut UI.

**Independent Test**: bind a test shortcut; press-and-hold over a focused field,
speak, release; assert a session starts on press, ends on release, transcript
injected — no terminal involved.

### Tests for User Story 2 (write first, must fail) ⚠️

- [X] T022 [P] [US2] Hermetic unit test for the autorepeat-dedup state machine in `client/myna-desktop/src/shortcut/portal.rs` (`#[cfg(test)]`, fed a fake portal-signal stream): first `Activated` → one `Press`; repeat `Activated` before `Deactivated` → ignored; `Deactivated` → `Release`; unbind/end → `None`. **Write first, observe fail, then satisfy with T024** (contract trigger.md T1–T4)
- [X] T023 [P] [US2] Integration test (gated `MYNA_PORTAL_TESTS`) in `tests/portal_hw.rs`: bind a test shortcut against the live portal; assert activate→`Press`, deactivate→`Release`; portal-unavailable → `Err(PortalUnavailable)` (contracts T1, T2, T5)

### Implementation for User Story 2

- [X] T024 [US2] Implement `GlobalShortcutTrigger` in `client/myna-desktop/src/shortcut/portal.rs`: `bind(shortcut_id, preferred_trigger)` (portal CreateSession + BindShortcuts via `ashpd`/`zbus`); map `Activated`→`Press` (deduped, first-wins-until-`Deactivated`), `Deactivated`→`Release`, session-end→`None`; implement the orchestrator's `Trigger` trait (satisfies T022, T023; contracts T1–T6; FR-006/007/008/010)
- [X] T025 [US2] Add a `--hotkey`/portal mode to `client/myna-desktop/src/bin/myna-desktop.rs` selecting `GlobalShortcutTrigger` in place of `StdinTrigger`, with a default free `Super+<letter>` `preferred_trigger` confirmable via the desktop dialog (FR-009); document the first-run bind flow
- [X] T026 [US2] Run quickstart step 3 (hands-free): bind on first run, press-and-hold, speak, release → transcript injected with no terminal; autorepeat starts exactly one session (SC-003)

**Checkpoint**: US1 + US2 both independently testable; dictation is hands-free.

---

## Phase 5: User Story 3 — See that dictation is active (Priority: P2)

**Goal**: a persistent, screen-reader-perceivable GTK4 overlay with distinct
recording/transcribing/finalizing/error states. Independent of US2; may run in
parallel once Foundational lands.

**Independent Test**: drive a session lifecycle and assert the indicator shows a
distinct state per phase, appears within the latency target, and clears at end.

### Tests for User Story 3 (write first, must fail) ⚠️

- [X] T027 [P] [US3] Hermetic test (`MockIndicator`): a full lifecycle yields the state sequence Recording→Transcribing→Finalizing→Hidden, and an error yields `Error(msg)`; no transcript text ever passed to the indicator — `client/myna-desktop/tests/controller.rs` (contracts N1–N4, N8)
- [X] T028 [P] [US3] Integration test (gated, display-present) in `tests/indicator_hw.rs`: `GtkIndicator` becomes visible within the activation-latency target after `Recording` (N5, **FR-018**), exposes state to AT-SPI (N6, **FR-019**), and the `notify` fallback fires when the overlay surface is unavailable (N7, **FR-020**)

### Implementation for User Story 3

- [X] T029 [US3] Implement `GtkIndicator` in `client/myna-desktop/src/indicator/gtk.rs` (`#[cfg(feature = "ui-gtk")]`): a borderless always-on-top non-focusable GTK4 overlay with distinct visuals per `IndicatorState`; state pushed via a channel from the tokio side; AT-SPI labels for a11y; error state also raises a `notify-rust` toast (satisfies T028; contracts N1–N7; FR-017/018/019/020)
- [X] T030 [US3] Wire the GTK main-thread/tokio-worker bridge in `client/myna-desktop/src/bin/myna-desktop.rs` (`ui-gtk`): GTK owns the main thread + GLib loop, the controller/tokio runtime runs on a worker thread, indicator states flow over a channel (plan Complexity Tracking main-thread row); select `GtkIndicator` when `ui-gtk` is on, else `NotifyIndicator`
- [X] T031 [US3] Run quickstart step 3 with the overlay: indicator appears on press, transitions through states, clears on completion; screen-reader announces state changes

**Checkpoint**: US1–US3 testable; the dictation experience is visible + hands-free.

---

## Phase 6: User Story 4 — Safe targeting and protected fields (Priority: P3)

**Goal**: target fixed at start (no retarget on focus change → end safely),
secure/password fields refused, target-gone cancels safely. Depends on US1
(extends `IbusInjector` + controller policy).

**Independent Test**: start in field A, switch focus to B mid-session → zero chars
in B, session ends safely; focus a password field → refused with feedback; close
the target window mid-session → cancelled safely.

### Tests for User Story 4 (write first, must fail) ⚠️

- [X] T032 [P] [US4] Hermetic controller test (`MockInjector` scripted `focus_events`): a `FocusEvent::FocusOut` mid-session finalizes already-committed text and ends the session with **zero** further `commit` calls — `client/myna-desktop/tests/controller.rs` (contract I8; FR-014, SC-007, US4-1)
- [X] T033 [P] [US4] Hermetic controller test: a `FocusEvent::TargetGone` cancels safely (discards uncommitted, `cancel` called, notification) (contract I9; FR-022, US4-2)
- [X] T034 [P] [US4] Hermetic controller test: `acquire` → `Err(SecureField)` refuses to start, shows an error/notification, and never captures audio (contract I5; FR-021, SC-008, US4-3)
- [X] T035 [P] [US4] Integration test (gated `MYNA_IBUS_TESTS`) in `tests/ibus_hw.rs`: real IBus `FocusOut` from a focused entry emits a `FocusEvent::FocusOut`; a password-purpose entry (`SetContentType` PASSWORD) makes `acquire` return `Err(SecureField)` (contracts I5, I8)

### Implementation for User Story 4

- [X] T036 [US4] Implement focus/secure detection in `client/myna-desktop/src/inject/ibus.rs`: `FocusOut`/context-gone → the `focus_events` stream (`FocusOut`/`TargetGone`); `SetContentType` password purpose → `acquire` returns `Err(SecureField)`; best-effort where no content-type is advertised, documented (satisfies T035; contracts I5, I8, I9; FR-021)
- [X] T037 [US4] Implement the controller safety policy in `client/myna-desktop/src/controller.rs`: on `FocusOut` → finalize-and-end (Finalizing→Completed, no new commits); on `TargetGone` → Cancelled + notify; on `SecureField` at acquire → Error + notify, no capture (satisfies T032, T033, T034; FR-014/021/022)
- [X] T038 [US4] Run quickstart step 4: focus-change mid-session inserts zero chars into the new surface (SC-007); password field refused (SC-008); target-window close cancels safely

**Checkpoint**: all four stories independently functional; UD129 safety/privacy acceptance met.

---

## Phase 7: Polish & Cross-Cutting Concerns

**Purpose**: retire the legacy Python stubs (done LAST, after the Rust contract is
proven), record watermarks, finish docs.

- [X] T039 Retire the legacy Python desktop stubs (FR-025): delete `server/src/myna/desktop/{__init__,controller,textout}.py`; update the docstring in `server/src/myna/__init__.py` and the note in `server/src/myna/core/__init__.py` that reference `myna.desktop`. Verify `cd server && uv run pytest -q` green and `import myna; import myna.core; import myna.server` succeed (research R8; SC-010)
- [X] T040 [P] Performance watermark test (`perf_*`, gated) in `client/myna-desktop/tests/indicator_hw.rs` (or a `watermarks` module): activation→indicator-visible (≤200 ms, SC-005), press→capture-start (<100 ms), per-segment commit (<50 ms), session-teardown latency — checked-in baselines with declared tolerances in the module doc (constitution Principle III); reuse feature-002 capture-path baselines
- [X] T041 [P] Create `docs/desktop-injection.md` (or extend `docs/architecture/`): the settled T21/T22 contract — controller state model, the three seams (`Trigger`/`Injector`/`Indicator`) with mocks, IBus-over-zbus backend, GlobalShortcuts activation, GTK indicator, the R9 streaming-preedit extension path, and the Wayland-native (`input_method_v2`) future
- [X] T042 [P] Update `docs/project-plan.md` (T21 + T22 rows → done + outcome, spoken-run gate noted) and `README.md` (the `myna-desktop` push-to-talk app: `--hotkey`/portal + IBus injection usage)
- [X] T043 Full validation: `cargo test --workspace` + `cargo test -p myna-desktop --no-default-features` + `cargo clippy --workspace --all-targets -- -D warnings` green; gated suites (`ibus_hw`/`portal_hw`/`indicator_hw`) green on the live desktop; quickstart steps 1–6 verified incl. the manual spoken run; Workshop-env validation (`workshop launch myna` + in-env build/test) and CI action parity

**Checkpoint**: legacy stubs gone, the Rust desktop last-mile is the shipped path, watermarks recorded, docs current, workspace green.

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: no dependencies — start immediately
- **Foundational (Phase 2)**: depends on Setup — BLOCKS all user stories
- **US1 (Phase 3)**: depends on Foundational — the MVP
- **US2 (Phase 4)**: depends on Foundational (portal Trigger reuses the orchestrator `Trigger`); hands-free end-to-end also needs US1's binary
- **US3 (Phase 5)**: depends on Foundational only (Indicator seam) — independent of US1/US2; may run in parallel with US2
- **US4 (Phase 6)**: depends on US1 (extends `IbusInjector` + controller policy)
- **Polish (Phase 7)**: T039 (Python removal) after US1 proves the Rust contract; T040–T043 after their subject stories

### Story Dependency Graph

```text
Setup → Foundational ┬→ US1 (P1, MVP) ┬→ US4 (P3)
                     │                 └→ US2 (P2)  (E2E needs US1's binary)
                     └→ US3 (P2)        (parallel with US2)
                                        → Polish (T039 after US1 green)
```

### Within Each Story

- Tests (write first, observe fail) → implementation → story checkpoint
- Traits/mocks before backends; backends before binary wiring

---

## Parallel Opportunities

- **Setup**: T003, T004 in parallel (after T002 creates the crate)
- **Foundational**: T007, T008, T009 [P] (distinct files) alongside T005/T006
- **US1 tests**: T011–T017 all [P] (distinct test bodies) — write together, watch fail
- **US2 tests**: T022, T023 [P]; **US3 tests**: T027, T028 [P]; **US4 tests**: T032–T035 [P]
- **Cross-story**: once Foundational lands, US3 (Phase 5) proceeds alongside US2
- **Polish**: T040–T042 [P] (different files); T039 gated on US1, T043 last

### Parallel Example: US1 tests

```bash
# Write these together, ensure all fail, then implement T018–T020:
Task: "T011 commit-order/once test in tests/controller.rs"
Task: "T011a no-capture-between-sessions test (FR-004/SC-004) in tests/controller.rs"
Task: "T012 cold-load buffered-then-injected test in tests/controller.rs"
Task: "T013 snippet-never-committed test in tests/controller.rs"
Task: "T014 no-speech no-commit test in tests/controller.rs"
Task: "T015 NoTarget/Unavailable error-state test in tests/controller.rs"
Task: "T016 literal-only + idempotent-restore test (MockInjector)"
Task: "T017 gated IBus commit+restore test in tests/ibus_hw.rs"
```

---

## Implementation Strategy

### Branch Staging Plan (REQUIRED — constitution "Staged Delivery in Feature Branches")

Each branch is one independently testable increment (tests + implementation
together), builds only on merged prerequisites, and leaves `main` green. The
Python-stub removal is deliberately its **own final branch** so nothing depends
on unproven Rust.

| # | Branch | Scope (phases/tasks) | Prerequisite branches | Merge gates |
|---|--------|----------------------|-----------------------|-------------|
| 1 | `003a-desktop-setup-foundation` | Phase 1–2 (T001–T010) | — | hermetic suite green (`--no-default-features`); workspace builds both feature settings; Workshop def extended (Principle IV gate closes here) |
| 2 | `003b-ibus-injection-us1` | Phase 3 (T011–T021) | #1 | hermetic + gated `ibus_hw` green; manual spoken run (SC-001) |
| 3 | `003c-global-shortcut-us2` | Phase 4 (T022–T026) | #2 | hermetic dedup + gated `portal_hw` green |
| 4 | `003d-activity-indicator-us3` | Phase 5 (T027–T031) | #1 | hermetic + gated `indicator_hw` (display) green (may land before/after #3) |
| 5 | `003e-safety-us4` | Phase 6 (T032–T038) | #2 | hermetic + gated `ibus_hw` (focus/secure) green |
| 6 | `003f-polish-cleanup` | Phase 7 (T039–T043) | #2 (US1 green), plus #3–#5 for docs completeness | full workspace + clippy green; watermarks recorded; Python suite green after stub removal; quickstart 1–6 pass |

Notes: branch #4 depends only on #1 and can land in parallel with #3. Branch #6
must not remove the Python stubs until US1 proves the Rust contract on hardware.

### MVP First

1. Setup + Foundational (branch #1)
2. US1 IBus injection (branch #2) → **STOP & VALIDATE**: quickstart steps 1–3, spoken run
3. Ship/demo: spoken words land in the focused app (stdin-triggered) — the last mile works

### Incremental Delivery

1. Foundation → US1 (MVP) → validate
2. Add US2 (hands-free hotkey) → validate
3. Add US3 (indicator) and US4 (safety) → validate each
4. Retire Python stubs + polish (branch #6) → final acceptance

---

## Notes

- [P] = different files, no dependency on incomplete tasks
- All real IBus/portal/GTK tests are gated and skip cleanly offline; identical code
  runs on the desktop VM and on hardware (Principle II)
- Never persist audio; the injector handles text only; no transcript content logged
  by default (Principle V)
- Verify each test fails before implementing; commit per task or logical group
- Do not remove the Python `myna.desktop` stubs (T039) until US1 is green (T021)
- The R9 streaming-preedit seam (`set_preedit`/`supports_preedit`) is scaffolded but
  **not wired** in this feature — commit-only MVP (FR-012)
