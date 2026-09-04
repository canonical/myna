# Tasks: Dual-Mode Streaming Transcription

**Input**: Design documents from `/specs/007-streaming-mode/`

**Prerequisites**: plan.md (required), spec.md (required for user stories), research.md, data-model.md, contracts/

**Tests**: Test tasks are REQUIRED for Rust code and MUST precede their corresponding implementation tasks (red-green TDD per constitution Principle I). Python adapter code is evaluation-harness tier — exempt from TDD but should have tests where practical.

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3)
- Include exact file paths in descriptions

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Project initialization and documentation structure

- [X] T001 Verify feature documentation structure is complete in `specs/007-streaming-mode/`
- [X] T002 Add streaming metrics definitions to `dev/matrix.py` (time_to_first_committed, commit_stability columns)
- [X] T003 [P] Add streaming strategy axis to `dev/bench.py` (streaming/batch selector in benchmarks)

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Core infrastructure that MUST be complete before ANY user story can be implemented

**⚠️ CRITICAL**: No user story work can begin until this phase is complete

- [X] T004 Add Disposition enum to Python wire contract in `server/src/myna/core/events.py`
- [X] T005 Add StreamingMode enum to Rust core in `client/myna-core/src/events.rs`
- [X] T006 [P] Create streaming session state module in `server/src/myna/core/streaming.py` (SegmentAccumulator)
- [X] T007 Update `docs/architecture/ie115-wire.md` with reference to streaming amendment in `specs/007-streaming-mode/contracts/streaming-wire.md`

**Checkpoint**: Foundation ready - user story implementation can now begin in parallel

---

## Phase 3: User Story 4 - Wire protocol distinguishes committed from unstable text (Priority: P1)

**Goal**: The IE115 wire carries an explicit committed/unstable discriminant so clients can safely inject streaming text

**Independent Test**: Send test events through the wire codec; verify disposition field encodes/decodes correctly; verify backward-compat (absent field = committed)

### Tests for User Story 4 (REQUIRED - constitution Principle I) ⚠️

> **NOTE: Write these tests FIRST, ensure they FAIL before implementation**

- [X] T008 [P] [US4] Golden-frame test for disposition encoding in `server/tests/core/test_wire_ie115_disposition.py`
- [X] T009 [P] [US4] Golden-frame test for disposition decoding in `client/myna-core/src/wire/ie115_tests.rs`
- [X] T010 [P] [US4] Backward-compat test (absent field → committed default) in both Python and Rust

### Implementation for User Story 4

- [X] T011 [P] [US4] Extend delta event with disposition field in `server/src/myna/core/wire_ie115.py` (encode)
- [X] T012 [P] [US4] Extend delta event with disposition field in `client/myna-orchestrator/src/backend/ws_unix_ie115.rs` (decode)
- [X] T013 [P] [US4] Add segment_index field to committed deltas in `server/src/myna/core/events.py`
- [X] T014 [US4] Add session.streaming field to session.created greeting in `server/src/myna/core/wire_ie115.py`
- [X] T015 [US4] Decode session.streaming field in `client/myna-orchestrator/src/backend/ws_unix_ie115.rs`
- [X] T016 [US4] Update existing wire contract tests to pass with additive fields

**Checkpoint**: At this point, the wire can carry committed/unstable discriminants; both client and server speak the extended protocol

---

## Phase 4: User Story 6 - Streaming adapters emit progressive committed segments (Priority: P1)

**Goal**: Server-side adapters emit committed text progressively during inference (Nemotron native, Whisper LocalAgreement)

**Independent Test**: Send 10+ second audio to each adapter in streaming mode; verify committed deltas arrive before end-of-audio; verify final transcript = concatenation of committed segments

### Tests for User Story 6 (Python harness tier — tests optional but recommended)

- [X] T017 [P] [US6] Integration test for Nemotron streaming emission in `server/tests/testbed/test_nemotron_streaming.py`
- [X] T018 [P] [US6] Integration test for Whisper streaming emission in `server/tests/testbed/test_whisper_streaming.py`
- [X] T019 [P] [US6] Committed-text invariant test (append-only, no retraction) in `server/tests/testbed/test_streaming_invariants.py`

### Implementation for User Story 6

- [X] T020 [P] [US6] Add streaming flag to FasterWhisperAdapter constructor in `server/src/myna/testbed/whisper.py`
- [X] T021 [US6] Integrate whisper_streaming (LocalAgreement) into FasterWhisperAdapter in `server/src/myna/testbed/whisper.py`
- [X] T022 [US6] Emit committed segments from Whisper stable-chunk callback in `server/src/myna/testbed/whisper.py`
- [X] T023 [P] [US6] Add streaming mode to NemotronAdapter constructor in `server/src/myna/testbed/nemotron.py`
- [X] T024 [US6] Implement native transducer streaming (frame-by-frame commit) in `server/src/myna/testbed/nemotron.py`
- [X] T025 [US6] Emit committed segments from Nemotron decode loop in `server/src/myna/testbed/nemotron.py`
- [X] T026 [US6] Add --streaming CLI flag to myna-server in `server/src/myna/server.py`
- [X] T027 [US6] Wire session.streaming field based on adapter mode in `server/src/myna/server.py`

**Checkpoint**: Adapters emit progressive committed deltas; client receives them but doesn't yet display them progressively

---

## Phase 5: User Story 1 - User sees text appear while still speaking (Priority: P1) 🎯 MVP

**Goal**: On streaming-capable hardware, committed text appears in the text field progressively as the user speaks

**Independent Test**: Activate dictation on GPU-tier hardware, speak 8+ seconds; verify at least 2 committed segments appear before hotkey release; verify no retraction

### Tests for User Story 1 (REQUIRED - constitution Principle I) ⚠️

- [X] T028 [P] [US1] FSM test for streaming committed-segment accumulation in `client/myna-orchestrator/tests/fsm_streaming_tests.rs`
- [X] T029 [P] [US1] FSM test for streaming terminal (completed after deltas) in `client/myna-orchestrator/tests/fsm_streaming_tests.rs`
- [X] T030 [P] [US1] Integration test for streaming round-trip (myna-dictate → streaming server) in `client/myna-cli/tests/integration_streaming_tests.rs`

### Implementation for User Story 1

- [X] T031 [P] [US1] Extend FSM state to track streaming mode in `client/myna-orchestrator/src/fsm.rs`
- [X] T032 [US1] Handle committed deltas in FSM (accumulate + emit to sink progressively) in `client/myna-orchestrator/src/fsm.rs`
- [X] T033 [US1] Handle unstable deltas in FSM (discard by default) in `client/myna-orchestrator/src/fsm.rs`
- [X] T034 [US1] Add streaming display mode to myna-dictate (» committed, ~ unstable with flag) in `client/myna-cli/src/main.rs`
- [X] T035 [US1] Update TextSink trait to support progressive emission in `client/myna-orchestrator/src/runner.rs`
- [X] T036 [US1] Add --show-unstable CLI flag to myna-dictate in `client/myna-cli/src/main.rs`

**Checkpoint**: Streaming text appears progressively in myna-dictate; committed segments are injected; unstable text is optionally shown

---

## Phase 6: User Story 2 - Batch mode remains the default on lower hardware tiers (Priority: P1)

**Goal**: On CPU-only or weak hardware, batch mode is automatically selected; streaming is not attempted

**Independent Test**: On a CPU-only tier where RTF ≥ 1.0, activate dictation; verify text appears only after release (no progressive deltas)

### Tests for User Story 2 (REQUIRED - constitution Principle I) ⚠️

- [X] T037 [P] [US2] Test RTF gate logic (threshold check) in `client/myna-orchestrator/tests/tier_assessment_tests.rs`
- [X] T038 [P] [US2] Integration test for batch mode on low-tier hardware in `client/myna-cli/tests/integration_batch_tests.rs`

### Implementation for User Story 2

- [X] T039 [P] [US2] Create TierAssessment module in `client/myna-core/src/tier.rs` (RTF baseline loader)
- [X] T040 [US2] Implement RTF gate (threshold check) in `client/myna-core/src/tier.rs`
- [X] T041 [US2] Generate streaming-tiers.json baseline data via dev/matrix.py in `results/streaming-tiers.json`
- [X] T042 [US2] Load tier baselines at session start in `client/myna-orchestrator/src/runner.rs`
- [X] T043 [US2] Apply RTF gate before session creation in `client/myna-orchestrator/src/runner.rs`
- [X] T044 [US2] Default to batch when no baseline exists in `client/myna-core/src/tier.rs`

**Checkpoint**: RTF gate protects low-tier hardware from degraded streaming; batch mode unchanged from today

---

## Phase 7: User Story 3 - User can choose between streaming and batch in settings (Priority: P2)

**Goal**: Users on capable hardware can force batch mode; power users can force streaming

**Independent Test**: Change mode setting, activate dictation, verify selected mode is honored

### Tests for User Story 3 (REQUIRED - constitution Principle I) ⚠️

- [X] T045 [P] [US3] Test mode override (user setting beats tier gate) in `client/myna-orchestrator/tests/mode_override_tests.rs`
- [X] T046 [P] [US3] Test mode persistence (setting survives restart) in `client/myna-cli/tests/settings_persistence_tests.rs`

### Implementation for User Story 3

- [X] T047 [P] [US3] Add StreamingMode enum to client settings in `client/myna-core/src/settings.rs`
- [X] T048 [US3] Persist mode setting via snap config (or dconf for unconfined) in `client/myna-core/src/settings.rs`
- [X] T049 [US3] Add --mode CLI flag to myna-dictate in `client/myna-cli/src/main.rs`
- [X] T050 [US3] Apply user override in mode resolution (auto/streaming/batch) in `client/myna-orchestrator/src/runner.rs`
- [X] T051 [US3] Add mode setting UI to myna-desktop (or document CLI for snap config) in `client/myna-desktop/src/main.rs` or `docs/usage.md`

**Checkpoint**: User can override automatic tier-based mode selection; setting persists

---

## Phase 8: User Story 5 - Interop findings fed back to canonical/whisper-snap team (Priority: P2)

**Goal**: Document and deliver the 6 protocol gaps discovered during interop experiments

**Independent Test**: A written interop report exists and has been shared with the team

### Implementation for User Story 5

- [X] T052 [P] [US5] Create interop report document in `docs/interop/canonical-whisper-snap-report.md`
- [X] T053 [US5] Document gap #1 (endpoint path standardization) in `docs/interop/canonical-whisper-snap-report.md`
- [X] T054 [US5] Document gap #2 (binary frame support) in `docs/interop/canonical-whisper-snap-report.md`
- [X] T055 [US5] Document gap #3 (empty-completed-as-reset anti-pattern) in `docs/interop/canonical-whisper-snap-report.md`
- [X] T056 [US5] Document gap #4 (model.loaded/unloaded → STATUS alignment) in `docs/interop/canonical-whisper-snap-report.md`
- [X] T057 [US5] Document gap #5 (session.update unconditional reload) in `docs/interop/canonical-whisper-snap-report.md`
- [X] T058 [US5] Document gap #6 (session.created timing / liveness signal) in `docs/interop/canonical-whisper-snap-report.md`
- [X] T059 [US5] Add proposed resolutions and recommendations to `docs/interop/canonical-whisper-snap-report.md`
- [X] T060 [US5] Deliver report to canonical/whisper-snap team (GitHub issue + comms channel)
- [X] T061 [P] [US5] Integration test against canonical/whisper-snap adapter in `client/myna-cli/tests/interop_canonical_tests.rs`

**Checkpoint**: Interop report delivered; protocol alignment discussion initiated; interop test verifies protocol compatibility

---

## Phase 9: Polish & Cross-Cutting Concerns

**Purpose**: Improvements that affect multiple user stories

- [X] T062 [P] Update architecture docs with streaming design in `docs/architecture/streaming.md` (include Qwen-C streaming deferral rationale)
- [X] T063 [P] Update streaming validation scenarios in quickstart.md
- [X] T064 [P] Measure streaming WER vs batch WER on real corpus (SC-002: within 2pp)
- [X] T065 [P] Measure time-to-first-committed on GPU tier (SC-001: ≤3s Nemotron, ≤5s Whisper)
- [X] T066 [P] Record performance watermark baselines for streaming metrics
- [X] T067 Refactor streaming FSM paths in `client/myna-orchestrator/src/fsm.rs` and `client/myna-orchestrator/src/runner.rs`
- [X] T068 Run quickstart.md validation scenarios end-to-end
- [X] T069 Update CLAUDE.md with streaming status

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies - can start immediately
- **Foundational (Phase 2)**: Depends on Setup completion - BLOCKS all user stories
- **User Story 4 (Phase 3)**: Depends on Foundational - wire protocol is foundational for all streaming work
- **User Story 6 (Phase 4)**: Depends on US4 - adapters need wire discriminant to emit committed segments
- **User Story 1 (Phase 5)**: Depends on US4 + US6 - client needs wire discriminant + server emission
- **User Story 2 (Phase 6)**: Depends on US1 - RTF gate needs streaming path to exist before gating it
- **User Story 3 (Phase 7)**: Depends on US1 + US2 - override needs both modes working
- **User Story 5 (Phase 8)**: Can start after US4 (wire alignment) - independent deliverable
- **Polish (Phase 9)**: Depends on all desired user stories being complete

### User Story Dependencies

- **US4 (P1)**: Foundational for all streaming work - no dependencies on other stories
- **US6 (P1)**: Depends on US4 (wire discriminant) - server-side emission
- **US1 (P1)**: Depends on US4 + US6 (wire + emission) - client display
- **US2 (P1)**: Depends on US1 (streaming path must exist to gate it)
- **US3 (P2)**: Depends on US1 + US2 (both modes must work)
- **US5 (P2)**: Can proceed in parallel with implementation once wire design (US4) is ratified

### Within Each User Story

- Tests MUST be written and FAIL before implementation (red-green TDD, constitution Principle I)
- Wire codec changes before adapter emission
- Adapter emission before client display
- Core implementation before integration
- Story complete before moving to next priority

### Parallel Opportunities

- All Setup tasks marked [P] can run in parallel
- All Foundational tasks marked [P] can run in parallel (within Phase 2)
- Within US4: golden-frame tests and backward-compat tests can run in parallel
- Within US6: Nemotron and Whisper adapter work can run in parallel (different files)
- Within US1: FSM tests can run in parallel with each other
- US5 (interop report) can be drafted in parallel with implementation phases 3-7
- Polish phase: documentation, metrics, and cleanup can run in parallel

---

## Parallel Example: User Story 4 (Wire Protocol)

```bash
# Launch all tests for User Story 4 together:
Task T008: "Golden-frame test for disposition encoding in server/tests/core/test_wire_ie115_disposition.py"
Task T009: "Golden-frame test for disposition decoding in client/myna-core/src/wire/ie115_tests.rs"
Task T010: "Backward-compat test (absent field → committed default) in both Python and Rust"

# Launch codec implementations in parallel:
Task T011: "Extend delta event with disposition field in server/src/myna/core/wire_ie115.py"
Task T012: "Extend delta event with disposition field in client/myna-orchestrator/src/backend/ws_unix_ie115.rs"
Task T013: "Add segment_index field to committed deltas in server/src/myna/core/events.py"
```

---

## Implementation Strategy

### Branch Staging Plan (REQUIRED - constitution "Staged Delivery in Feature Branches")

Map phases/stories to sensibly scoped feature branches, in merge order, with the test gates
that must pass at each merge. Each branch is one independently testable increment containing
its tests and implementation together; a branch must not build on unmerged sibling work, and
every merge must leave the default branch green.

| # | Branch | Scope (phases/stories) | Prerequisite branches | Merge gates |
|---|--------|------------------------|-----------------------|-------------|
| 1 | `007a-wire-discriminant` | Phase 1–3 (Setup, Foundational, US4) | — | hermetic suite (golden-frame tests, backward-compat tests) |
| 2 | `007b-streaming-adapters` | Phase 4 (US6) | #1 | hermetic + integration (streaming emission tests, committed-text invariant tests) |
| 3 | `007c-client-streaming` | Phase 5 (US1) | #2 | hermetic + integration (FSM streaming tests, round-trip tests) |
| 4 | `007d-tier-gate-interop` | Phase 6–8 (US2, US3, US5) | #3 | hermetic + integration (tier gate tests, mode override tests) + interop report delivered |

### MVP First (User Story 1 + 4 + 6 Only)

1. Complete Phase 1: Setup
2. Complete Phase 2: Foundational (CRITICAL - blocks all stories)
3. Complete Phase 3: User Story 4 (wire protocol) → merge branch 007a
4. Complete Phase 4: User Story 6 (streaming adapters) → merge branch 007b
5. Complete Phase 5: User Story 1 (client display) → merge branch 007c
6. **STOP and VALIDATE**: Test streaming end-to-end on GPU hardware
7. Deploy/demo if ready

### Incremental Delivery

1. Complete Setup + Foundational → Foundation ready
2. Add US4 (wire protocol) → Test wire codec independently → Merge 007a
3. Add US6 (streaming adapters) → Test server emission independently → Merge 007b
4. Add US1 (client display) → Test end-to-end streaming → Merge 007c (MVP!)
5. Add US2 + US3 (tier gate + user override) + US5 (interop report) → Merge 007d
6. Each branch adds value without breaking previous functionality

### Parallel Team Strategy

With multiple developers:

1. Team completes Setup + Foundational together (Phase 1–2)
2. Branch 007a (US4 wire protocol):
   - Developer A: Python wire codec + tests
   - Developer B: Rust wire codec + tests
3. Branch 007b (US6 streaming adapters):
   - Developer C: Nemotron streaming
   - Developer D: Whisper LocalAgreement streaming
4. Branch 007c (US1 client display):
   - Developer A: FSM + runner changes
   - Developer B: myna-dictate display
5. Branch 007d (US2 + US3 + US5):
   - Developer C: RTF gate + tier assessment
   - Developer D: Mode override + settings
   - Developer E: Interop report (can start after 007a)

---

## Notes

- [P] tasks = different files, no dependencies
- [Story] label maps task to specific user story for traceability
- Each user story should be independently completable and testable
- Verify tests fail before implementing (red-green TDD)
- Commit after each task or logical group
- Stop at any checkpoint to validate story independently
- Python adapter code is evaluation-harness tier — exempt from strict TDD but tests are recommended
- Rust client code MUST follow TDD (constitution Principle I)
- Avoid: vague tasks, same file conflicts, cross-story dependencies that break independence
