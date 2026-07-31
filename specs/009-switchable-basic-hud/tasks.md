# Tasks: Switchable Basic Dictation HUD

**Input**: Design documents from `/specs/009-switchable-basic-hud/`

**Prerequisites**: `plan.md`, `spec.md`, `research.md`, `data-model.md`, `contracts/`, `quickstart.md`

**Tests**: Test tasks are REQUIRED and precede their corresponding implementation tasks. Pure GJS behavior follows red-green TDD; Shell actor construction and focus behavior use the feature-004 harness-tier exception and the manual acceptance in `quickstart.md`.

**Organization**: Tasks are grouped by user story. Shared presentation-state ownership is foundational because both HUD styles must preserve one notice/error lifecycle and one timestamped level stream.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel because it changes different files and has no dependency on incomplete work.
- **[Story]**: Maps a task to US1, US2, or US3 from `spec.md`; setup, foundational, and polish tasks have no story label.
- Every task names its exact repository path.

## Path Conventions

- Extension source: `extensions/myna-shell/`
- Headless GJS tests: `extensions/myna-shell/test/`
- Extension schema: `extensions/myna-shell/schemas/`
- Feature contracts and validation: `specs/009-switchable-basic-hud/`
- Canonical automation: `.workshop/myna.yaml` and `.github/workflows/ci.yml`

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Make the existing extension test/package boundary runnable before behavior changes begin.

- [X] T001 Add a `gjs-package` Workshop action that packages the current extension into `/tmp` without committing generated artifacts in `.workshop/myna.yaml
- [X] T002 Add the existing `workshop run myna gjs-test` action and the new `gjs-package` action to the Workshop CI job in `.github/workflows/ci.yml` after T001 defines the package action

**Checkpoint**: Existing feature-004 GJS tests and the current extension package smoke pass in Workshop before feature-009 behavior lands.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Move presentation-independent state above the view seam so two renderers cannot duplicate or lose notice, dismissal, and level-freshness semantics.

**CRITICAL**: Complete this phase before implementing either user-facing HUD path.

### Tests (red first)

- [X] T003 Write failing controller contract tests for source/display descriptors, one held-notice slot, absolute recoverable deadline, critical dismissal, timestamped levels, ordinary-state replacement, and idempotent destroy in `extensions/myna-shell/test/indicator-controller.test.js`

### Implementation (green)

- [X] T004 Implement the injected-clock/scheduler/view-factory `IndicatorController` state model and lifecycle required by T003 in `extensions/myna-shell/indicator-controller.js`
- [X] T005 Remove held-notice policy, hold timers, and now-dead policy-helper usage from `HudView`, accept an `onDismiss` callback, make `hide()` unconditional, and preserve supplied `receivedAt` level timestamps in `extensions/myna-shell/hud.js` and `extensions/myna-shell/hud-logic.js`
- [X] T006 Revise the rendering-only `IndicatorView` contract to `show`, timestamped `setLevel`, unconditional `hide`, and idempotent `destroy`, while retaining the wave constructor path in `extensions/myna-shell/view.js`
- [ ] T007 Run the existing `hud`, `ribbon`, and `lifecycle` GJS suites plus a wave-HUD loading/recording/notice/error/idle manual smoke after T004–T006, and record the regression result in `specs/009-switchable-basic-hud/quickstart.md`

**Checkpoint**: The wave HUD retains feature-004 observable behavior through one controller-owned lifecycle; no view owns semantic notice timing.

---

## Phase 3: User Story 1 - Use a simple native-style dictation indicator (Priority: P1) MVP

**Goal**: Provide the default compact basic HUD with microphone icon, content-free state label, and calibrated horizontal input-energy bar.

**Independent Test**: With no explicit style choice, start dictation and verify only the basic HUD appears on the primary monitor, the bar responds monotonically to normal speech, reaches empty on silence/stale/non-recording state, and keyboard focus remains in the target application.

### Tests for User Story 1 (red first)

- [X] T008 [US1] Write failing pure tests for calibrated floor-to-zero mapping, malformed/clamped inputs, monotonic fill, recording-only nonzero target, repeated fresh timestamps, stale decay within 600 ms, attack/release smoothing, and reduced-motion behavior in `extensions/myna-shell/test/basic.test.js`, plus injected-constructor tests for absent/unknown-to-basic fallback, explicit wave selection, and unchanged `onDismiss` forwarding in Shell-independent `extensions/myna-shell/test/view-selection.test.js`

### Implementation for User Story 1 (green)

- [X] T009 [US1] Implement the pure basic-bar target and smoothing functions by reusing `vumeter.js` calibration and normalizing its floor to zero in `extensions/myna-shell/basic-logic.js`
- [X] T010 [US1] Implement `BasicHudView` with primary-monitor positioning, non-focusable Shell chrome, mic/status/bar actors, timestamped level handling, state-driven zero targets, monitor cleanup, and pointer-only critical dismiss callback in `extensions/myna-shell/basic.js`
- [X] T011 [P] [US1] Add native OSD-style basic pill, progress track/fill, severity, loading, reduced-motion, and high-contrast theme rules without changing wave rules in `extensions/myna-shell/stylesheet.css`
- [X] T012 [US1] Implement pure style normalization/injected-constructor selection in `extensions/myna-shell/view-selection.js`, then make `extensions/myna-shell/view.js` supply the real basic/wave constructors and forward `onDismiss` without exposing Shell actors to headless tests
- [ ] T013 [US1] Execute the staged basic-only state, level, primary-monitor, high-contrast, reduced-motion, and focus checks from section 3a and record any platform findings in `specs/009-switchable-basic-hud/quickstart.md`

**Checkpoint**: US1 works as an independently demonstrable basic default HUD; the wave renderer remains constructible through the same seam.

---

## Phase 4: User Story 2 - Choose a preferred HUD style (Priority: P1)

**Goal**: Persist a Basic/Wave ribbon preference and switch the active presentation immediately without interrupting dictation or losing current state/level.

**Independent Test**: Select each style in preferences during idle and active dictation, verify immediate single-view replacement with preserved state and original level timestamp, then re-enable/relogin and confirm the last valid choice persists.

### Tests for User Story 2 (red first)

- [X] T014 [P] [US2] Write failing settings contract tests for schema ID/path/key, stable enum values, schema basic default, metadata declaration, and preference index mapping in `extensions/myna-shell/test/settings.test.js`
- [X] T015 [US2] Extend controller tests with failing cases for hidden switching, destroy-before-create ordering, descriptor/timestamp replay, unchanged-style no-op, invalid-to-basic fallback, and 100 rapid switches leaving one live view in `extensions/myna-shell/test/indicator-controller.test.js`
- [X] T016 [US2] Extend stub lifecycle tests with failing cases for live settings changes before service appearance, during recording, after disable, and across re-enable in `extensions/myna-shell/test/lifecycle.test.js`

### Implementation for User Story 2 (green)

- [X] T017 [P] [US2] Define the local `hud-style` enum schema with stable `basic=0`, `wave=1`, and default `basic` in `extensions/myna-shell/schemas/org.gnome.shell.extensions.myna.gschema.xml`
- [X] T018 [US2] Declare `org.gnome.shell.extensions.myna` as the extension settings schema in `extensions/myna-shell/metadata.json`
- [X] T019 [P] [US2] Implement the one-row `ExtensionPreferences.fillPreferencesWindow()` Basic/Wave selector with explicit enum-to-index mapping in `extensions/myna-shell/prefs.js`
- [X] T020 [US2] Implement atomic style replacement, visible descriptor replay, original timestamp replay, unchanged-style no-op, and retired-generation callback protection in `extensions/myna-shell/indicator-controller.js`
- [X] T021 [US2] Wire `getSettings()`, `changed::hud-style`, monotonic level-arrival timestamps, `DictationService`, and controller enable/disable cleanup in `extensions/myna-shell/extension.js`
- [ ] T022 [US2] Validate the source schema and packaged ZIP contents, exercise preference default/persistence and active/idle switching from sections 2–4, then verify both Basic and Wave survive full GNOME logout/login cycles from section 10 and record results in `specs/009-switchable-basic-hud/quickstart.md`

**Checkpoint**: US2 provides persistent live choice between exactly two HUDs, with one active presentation and no Shell or dictation-session restart.

---

## Phase 5: User Story 3 - Receive equivalent lifecycle and error feedback (Priority: P2)

**Goal**: Prove and preserve identical state, recoverable-notice, critical-error, focus, privacy, and teardown semantics across both HUD styles.

**Independent Test**: Drive both styles through every known/unknown lifecycle state, switch halfway through recoverable and critical holds, dismiss a critical error, switch again, and verify timing, persistence, non-resurrection, focus safety, and cleanup are equivalent.

### Tests for User Story 3 (red first)

- [X] T023 [US3] Add failing cross-style controller tests for all descriptor states, recoverable remaining-deadline preservation, genuine repeat deadline restart, critical persistence/dismissal/non-resurrection, ordinary-state override, and explicit service-disappearance behavior (ordinary clears; held recoverable/critical lifetimes persist without restart) in `extensions/myna-shell/test/indicator-controller.test.js`
- [X] T024 [US3] Extend the stub D-Bus lifecycle test through `IndicatorController` to assert repeated equal levels stay fresh, unknown states remain neutral, availability changes remain dormant/clean, and both fake view styles receive identical semantic descriptors in `extensions/myna-shell/test/lifecycle.test.js`

### Implementation for User Story 3 (green)

- [X] T025 [US3] Complete controller occurrence, held-notice deadline, critical-dismissal, service-disappearance, and teardown behavior needed by T023–T024 in `extensions/myna-shell/indicator-controller.js` and `extensions/myna-shell/extension.js`
- [X] T026 [US3] Align basic and wave state/severity rendering with shared icon, label, colour, dismiss, primary-monitor, and content-free behavior without changing wave geometry in `extensions/myna-shell/basic.js` and `extensions/myna-shell/hud.js`
- [ ] T027 [US3] Execute recoverable-switch, critical-switch/dismiss, unknown-state, service-disappearance, 100-switch cleanup, privacy, and focus checks from sections 5–7 and record results in `specs/009-switchable-basic-hud/quickstart.md`

**Checkpoint**: Both styles pass the same lifecycle/severity contract and no retired view or dismissed occurrence can react after replacement.

---

## Phase 6: Polish & Cross-Cutting Concerns

**Purpose**: Package, document, and validate the complete two-HUD feature.

- [X] T028 Update the Workshop package action to run strict schema validation and assert the ZIP includes `prefs.js`/schema but excludes `dev-lab/` and source `gschemas.compiled` in `.workshop/myna.yaml`
- [X] T029 [P] Update install instructions to compile local schemas, document `gnome-extensions prefs`, both HUD styles, default behavior, and the current module layout in `extensions/myna-shell/README.md`
- [X] T030 [P] Update the repository overview to describe the switchable basic/wave HUD and link feature 009 validation in `README.md`
- [X] T031 [P] Record feature 009 and its relationship to feature 004/T56 in the global tracker without rewriting feature-004 history in `docs/project-plan.md`
- [X] T032 Add supersession notes for the former single-view/no-settings assumptions while preserving historical decisions in `specs/004-gnome-shell-indicator/contracts/extension.md` and `specs/004-gnome-shell-indicator/data-model.md`
- [ ] T033 Run `workshop run myna gjs-test` and `workshop run myna gjs-package`, fix all regressions in `extensions/myna-shell/`, and record the final automated result in `specs/009-switchable-basic-hud/quickstart.md`
- [ ] T034 Run the complete installed GNOME acceptance on a named reference environment using the section-4 screen-recording latency method and compositor frame profiler, then record environment provenance, both-direction switch latency, 600 ms decay, approximately 60 fps rendering, primary-monitor placement, focus safety, service-loss behavior, 100-switch cleanup, and wave parity in `specs/009-switchable-basic-hud/quickstart.md`
- [ ] T035 Run the three-observer randomized state-identification trial from section 8, verify at least 33/36 correct responses (≥90%), and record aggregate results and confused state pairs in `specs/009-switchable-basic-hud/quickstart.md`
- [ ] T036 Disable all network interfaces while retaining the local D-Bus/PipeWire/IBus/backend stack, complete and switch one session in each HUD, inspect for remote fallback/content artifacts, and record the offline acceptance from section 9 in `specs/009-switchable-basic-hud/quickstart.md`

---

## Dependencies & Execution Order

### Phase Dependencies

- **Phase 1 Setup**: Starts immediately and establishes green automation.
- **Phase 2 Foundational**: Depends on Phase 1 and blocks all user stories.
- **Phase 3 US1**: Depends on Phase 2; delivers the basic default HUD.
- **Phase 4 US2**: Depends on Phase 2 and the US1 basic view because the preference must select two real renderers.
- **Phase 5 US3**: Depends on US1 and US2 to exercise parity and switching across both completed styles.
- **Phase 6 Polish**: Depends on every story selected for release.

### User Story Dependency Graph

```text
Setup -> Foundation -> US1 (basic HUD) -> US2 (persistent switching) -> US3 (parity/errors) -> Polish
```

- **US1 (P1)**: Independently demonstrates the requested simple basic indicator after Foundation.
- **US2 (P1)**: Adds durable user choice; requires US1 only because Basic must be a real selection target.
- **US3 (P2)**: Verifies equivalent established behavior after both styles and switching exist.

### Within Each User Story

- Write each pure/contract test and observe failure before implementation.
- Implement pure state/meter logic before Shell actor wiring.
- Keep semantic lifecycle in the controller and pixels in views.
- Run the story's headless tests before its manual Shell acceptance.
- Do not proceed past a checkpoint with a red default branch.

### Parallel Opportunities

- T011 can run after the Basic actor shape is agreed while T009–T010 proceed in separate files.
- T014 can run in parallel with the controller-switching test T015.
- T017 and T019 can run in parallel after T014 defines the expected schema/UI contract.
- T029, T030, and T031 can run in parallel after all story behavior is stable.

---

## Parallel Example: User Story 2

```text
Task T014: Write settings/schema contract tests in extensions/myna-shell/test/settings.test.js
Task T015: Write style replacement tests in extensions/myna-shell/test/indicator-controller.test.js

After T014 is red:
Task T017: Add the enum schema in extensions/myna-shell/schemas/org.gnome.shell.extensions.myna.gschema.xml
Task T019: Add the selector UI in extensions/myna-shell/prefs.js
```

## Parallel Example: Polish

```text
Task T029: Update extensions/myna-shell/README.md
Task T030: Update README.md
Task T031: Update docs/project-plan.md
```

---

## Implementation Strategy

### Branch Staging Plan

| # | Branch | Scope | Prerequisite | Merge gates |
|---|---|---|---|---|
| 1 | `009a-hud-controller` | Phase 1–2, T001–T007 | none | Existing + controller GJS contracts; current wave manual smoke; Workshop package smoke |
| 2 | `009b-basic-hud` | Phase 3, T008–T013 | #1 merged | Basic pure tests; full GJS suite; manual basic state/level/focus/monitor check |
| 3 | `009c-hud-preference` | Phase 4, T014–T022 | #2 merged | Settings/controller/lifecycle tests; strict schema/package smoke; persistence/live-switch manual check |
| 4 | `009d-hud-parity` | Phase 5, T023–T027 | #3 merged | Full GJS suite; recoverable/critical/service-loss/focus/100-switch manual checks |
| 5 | `009e-hud-docs-release` | Phase 6, T028–T036 | #4 merged | Workshop GJS + package gates; complete quickstart acceptance including state identification, relogin persistence, and offline operation; docs/spec consistency |

Each branch includes its tests and implementation together, is based only on merged prerequisites, and must leave the default branch green.

### MVP First

1. Complete Phase 1 Setup.
2. Complete Phase 2 Foundation.
3. Complete Phase 3 US1.
4. Stop and validate the basic default HUD independently.

US1 is the smallest visual MVP. The complete requested coexistence MVP is US1 + US2 because user-selectable switching is an explicit P1 requirement.

### Incremental Delivery

1. Foundation centralizes semantic lifetime without changing wave behavior.
2. US1 introduces the simple default presentation.
3. US2 exposes persistent live choice and restores wave accessibility.
4. US3 hardens parity and error/cleanup edges.
5. Polish makes schema packaging, CI, documentation, and hardware evidence release-ready.

---

## Notes

- `[P]` means different files and no incomplete dependency; do not parallelize tasks that edit the same controller/view file.
- Test task IDs precede implementation task IDs for every behavior-bearing pure module.
- Shell actor rendering is manually accepted because it cannot be instantiated safely in headless GJS; this is the recorded feature-004 harness-tier exception.
- No task changes the Rust publisher, D-Bus interface, inference backend, injection path, or snap packaging.
- Keep raw audio and transcript content out of settings, controller state, views, tests, logs, and artifacts.
