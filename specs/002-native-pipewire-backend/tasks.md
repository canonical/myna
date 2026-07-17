# Tasks: Native PipeWire Capture Backend

**Input**: Design documents from `/specs/002-native-pipewire-backend/`
**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/, quickstart.md

**Tests**: REQUIRED — this is a shipped Rust system component, so constitution
Principle I (Red-Green TDD) applies in full. Every behavior-bearing task is
preceded by a failing test. Hermetic tests use `ScriptedBackend` and pure mapping
logic; real-PipeWire behavior is proven by an env-gated integration suite that
runs identically on the virtual-audio VM and on hardware (Principle II).

**Organization**: Tasks grouped by user story (US1–US4) for independent
implementation and testing. Priority order: US1 (P1) → US2 (P2) → US3 (P3) →
US4 (P3).

## Format: `[ID] [P?] [Story] Description`

- **[P]**: can run in parallel (different files, no dependency on incomplete tasks)
- **[Story]**: US1–US4 for story-phase tasks; Setup/Foundational/Polish carry none
- All paths are repo-relative; crate is `rust/myna-audio` unless noted

## Path Conventions

- Crate under test: `rust/myna-audio/` (`src/`, `tests/`)
- Consumer: `rust/myna-cli/src/main.rs`
- Env-gated integration suite: `rust/myna-audio/tests/pipewire_hw.rs`
  (gate: `MYNA_PIPEWIRE_TESTS=1`)

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: environment + dependency + module scaffolding so all stories build.

- [ ] T001 Add a minimal `workshop.yaml` at the repo root declaring the Rust toolchain (1.75+), `libpipewire-0.3` dev headers, and PipeWire audio tooling (`pw-cli`, `pw-loopback`) as SDKs/interfaces, satisfying constitution Principle IV for this crate's build+test (plan Complexity Tracking row 1)
- [ ] T002 Add the `pipewire` crate to `[workspace.dependencies]` in `rust/Cargo.toml` and reference it from `rust/myna-audio/Cargo.toml`; confirm `cargo build -p myna-audio` links against system libpipewire-0.3
- [ ] T003 [P] Scaffold empty modules `rust/myna-audio/src/pipewire.rs` (declare `pub struct PipeWireBackend`) and `rust/myna-audio/src/devices.rs` (declare `pub struct InputDevices`, `pub struct InputDevice`, `pub enum DeviceChange`), wire `mod`/`pub use` into `rust/myna-audio/src/lib.rs` alongside the existing exports
- [ ] T004 [P] Create the env-gated integration test file `rust/myna-audio/tests/pipewire_hw.rs` with the `MYNA_PIPEWIRE_TESTS` gate helper that skips cleanly when unset (no PipeWire required to compile/run the suite as a no-op)

**Checkpoint**: workspace builds with the pipewire dependency; new modules and the
gated test harness exist and compile; hermetic suite still green.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: the shared PipeWire connection/main-loop plumbing that BOTH capture
(US1–US3) and enumeration (US4) depend on. No user story can start until this is
done.

**⚠️ CRITICAL**: complete before ANY user story phase.

- [ ] T005 Implement a PipeWire main-loop thread helper in `rust/myna-audio/src/pipewire.rs` (or a shared `pw_loop` submodule): create context+core on a dedicated OS thread, expose a quit signal and a ≤250 ms timer tick for stop-polling (research R2, R6); return a `CaptureError::DeviceUnavailable` if PipeWire is unreachable
- [ ] T006 [P] Unit tests for the registry-props → `InputDevice` mapping in `rust/myna-audio/src/devices.rs` (`#[cfg(test)]`): valid source → `InputDevice`; missing name → skipped; monitor → excluded. **Write first, observe fail, then satisfy with T007**
- [ ] T007 [P] Implement the registry-props → `InputDevice` mapping in `rust/myna-audio/src/devices.rs` (pure function: extract stable `node.name` + `node.description`, keep `media.class = Audio/Source`, exclude sink monitors, skip nodes with no `node.name`) — data-model `InputDevice`, contract E-mapping (satisfies T006)

**Checkpoint**: a PipeWire loop can be started/stopped on a thread; device-prop
mapping is unit-tested and green offline. Stories can now begin.

**Reuse note (FR-009)**: capture-at-press + push-gated-on-`ready` is behavior of
the **unchanged** `CaptureSource`/ring (§6), not the backend — so no backend task
re-implements it. It is asserted for the native path in T009 (ring fills before
the consumer drains) so the reuse is verified, not merely assumed.

---

## Phase 3: User Story 1 — Dictate through the native backend, no subprocess (Priority: P1) 🎯 MVP

**Goal**: in-process live capture from the default device in exactly the
negotiated format, drop-in behind the `CaptureBackend` seam, transcribing a known
utterance with no subprocess and nothing on disk.

**Independent Test**: run `myna-dictate --mic` (native) against a `myna-server`
capturing a known utterance from a virtual source; assert exact transcript,
capture-at-press, `dropped == 0`, and no `pw-record`/child process.

### Tests for User Story 1 (write first, must fail) ⚠️

- [ ] T008 [P] [US1] Hermetic seam test: `PipeWireBackend` rejects `sample_width_bytes != 2` with one `Err(UnsupportedFormat)` and rejects nothing else at build time — in `rust/myna-audio/src/pipewire.rs` `#[cfg(test)]` (contract C12)
- [ ] T009 [P] [US1] Integration test (gated) in `rust/myna-audio/tests/pipewire_hw.rs`: default-source capture yields chunks in exactly the negotiated format; the ring fills from `capture()` (press) while the consumer defers draining, then drains buffered-then-live with nothing lost (FR-009 capture-at-press, reusing the unchanged `CaptureSource`); graceful `stop()` drains then ends with no `Err`; `AudioStats::dropped == 0` (contracts C1, C8, C13; FR-009, SC-006)
- [ ] T010 [P] [US1] Integration test (gated): device native format ≠ negotiated (feed a source at a different rate/channels) → consumer still receives exactly the negotiated format (contract C2; FR-003)
- [ ] T011 [P] [US1] Integration test (gated): mid-capture device removal → exactly one descriptive `Err`, then end (never empty-clean); and abort (drop the stream) stops capture + discards ring (contracts C9, C10; SC-007)
- [ ] T012 [P] [US1] Integration test (gated): stop/abort honored within 250 ms; and assert no external process is spawned and nothing is written to disk during a session (contracts C11, C14; SC-002, SC-009)

### Implementation for User Story 1

- [ ] T013 [US1] Implement `PipeWireBackend::new()` + `CaptureBackend::start` in `rust/myna-audio/src/pipewire.rs`: connect an input `Stream` in `spec.format` to the default source on the T005 loop; `process` callback copies buffer → `Producer::push`; format-width guard → `Err(UnsupportedFormat)` (satisfies T008; contracts C1, C12)
- [ ] T014 [US1] Implement graph-side format via the stream's SPA audio-raw format param so rate/channel conversion happens in the graph (research R3; satisfies T010, contract C2)
- [ ] T015 [US1] Implement the lifecycle: poll `spec.stop` on the loop timer, quit+drain+`finish(None)` on graceful stop; `finish(None)` on consumer-gone (`push` false / ring closed = abort); `finish(Some(..))` on stream error; device-open failure → `Err` from `start` (satisfies T009, T011, T012; contracts C8–C11)
- [ ] T016 [US1] Switch `myna-cli --mic` from `PwRecordBackend` to `PipeWireBackend` in `rust/myna-cli/src/main.rs` (import + the `.backend(...)` call site at line ~208); keep the captured/dropped readout and the near-silence hint
- [ ] T017 [US1] Run quickstart steps 1–3 (hermetic green, gated integration green on the virtual-audio graph, live dictation correct with no subprocess); **while the subprocess backend still exists**, record the reference transcript for a known utterance (the SC-001 baseline the native path must match after T033 removes `pw_record.rs`); record any deltas

**Checkpoint**: US1 fully functional and independently testable — native capture
replaces the subprocess on the `--mic` path end-to-end. **MVP reached.**

---

## Phase 4: User Story 2 — Choose a specific input device that stays chosen (Priority: P2)

**Goal**: select the capture node by stable `node.name`, surviving graph
renumbering; clear fault when the target is absent.

**Independent Test**: two named virtual nodes; target one by name → captured
audio comes from it; renumber the graph → same name still selects the same node;
absent target → clear "device unavailable" fault.

### Tests for User Story 2 (write first, must fail) ⚠️

- [ ] T018 [P] [US2] Integration test (gated) in `rust/myna-audio/tests/pipewire_hw.rs`: with two named sources, `target = Some(name)` captures from that node and not the default (contract C3; US2-1)
- [ ] T019 [P] [US2] Integration test (gated): after a graph change that reassigns volatile ids, the same `node.name` target still selects the same physical node (contract C5; SC-003)
- [ ] T020 [P] [US2] Integration test (gated): an absent target at connect → exactly one `Err(DeviceUnavailable(target))`, then end (contract C4; US2-3)

### Implementation for User Story 2

- [ ] T021 [US2] Resolve `spec.target` (stable `node.name`) to a target node via the registry and connect the stream to it in `rust/myna-audio/src/pipewire.rs`; `None` → default source (research R4; satisfies T018, T019; contract C3, C5)
- [ ] T022 [US2] Map an absent/unresolvable target to `CaptureError::DeviceUnavailable(target)` returned from `start` (satisfies T020; contract C4)

**Checkpoint**: US1 + US2 both independently testable; device selection is stable.

---

## Phase 5: User Story 3 — Capture the right channels on a multi-channel interface (Priority: P3)

**Goal**: honor `spec.channels` (pick/downmix specific indices) on multi-channel
devices; reject a selection the device can't satisfy.

**Independent Test**: multi-channel virtual device with a signal only on chosen
channels → captured audio contains it; out-of-range indices → clear fault.

### Tests for User Story 3 (write first, must fail) ⚠️

- [ ] T023 [P] [US3] Integration test (gated) in `rust/myna-audio/tests/pipewire_hw.rs`: `channels = Some(idx…)` on a multi-channel source captures only those channels, downmixed to the negotiated layout (contract C6; SC-004, US3-1)
- [ ] T024 [P] [US3] Integration test (gated): channel indices the device can't satisfy → exactly one `Err`, no mis-capture (contract C7; US3-2)

### Implementation for User Story 3

- [ ] T025 [US3] Implement channel pick/downmix for `spec.channels` via the stream's channel-map/position param (graph-side) in `rust/myna-audio/src/pipewire.rs` (research R3/R4; satisfies T023; contract C6)
- [ ] T026 [US3] Validate requested indices against the device's channel count; unsatisfiable → `CaptureError::Backend`/`UnsupportedFormat` before capture (satisfies T024; contract C7)

**Checkpoint**: US1–US3 testable; the subprocess backend's channel limitation is
gone.

---

## Phase 6: User Story 4 — See the available input devices, live (Priority: P3)

**Goal**: list current input devices and notify an observer as devices
appear/disappear, without re-requesting.

**Independent Test**: known virtual nodes present → `list()` returns them with
name+label; add/remove a node → observer notified of appearance/disappearance.

**Note**: independent of capture (US1–US3); depends only on Foundational (T005,
T007). Can proceed in parallel with US2/US3 once Phase 2 is done.

### Tests for User Story 4 (write first, must fail) ⚠️

- [ ] T027 [P] [US4] Integration test (gated) in `rust/myna-audio/tests/pipewire_hw.rs`: `list()` returns every present input device with stable `node_name` + `label`; empty set → empty `Vec`, not error (contract E1, E2; SC-005, US4-1/2)
- [ ] T028 [P] [US4] Integration test (gated): an active `watch()` observer sees a device appear (name+label) and disappear (by name) without re-requesting (contract E3, E4; FR-008a, US4-3)
- [ ] T029 [P] [US4] Integration test (gated): PipeWire unreachable → `InputDevices::new()` returns `Err(DeviceUnavailable)`; and a `node_name` from `list()` used as `CaptureSpec.target` selects that device (contract E5, E7)

### Implementation for User Story 4

- [ ] T030 [US4] Implement `InputDevices::new()` + `list()` in `rust/myna-audio/src/devices.rs`: registry listener on the T005 loop, maintain the current set via the T007 mapping, snapshot on `list()` (satisfies T027, T029; contract E1, E2, E5)
- [ ] T031 [US4] Implement live updates: `global`/`global_remove` → `watch::Receiver<Vec<InputDevice>>` from `watch()` (and `changes()` broadcast of `DeviceChange` deltas only if a consumer needs them — otherwise omit per contract note); dropping the handle stops the listener (satisfies T028; contract E3, E4)
- [ ] T032 [US4] Add a `--list-devices` flag to `rust/myna-cli/src/main.rs` that prints the live device list (name + label) and reflects add/remove while running (quickstart step 4)

**Checkpoint**: all four stories independently functional.

---

## Phase 7: Polish & Cross-Cutting Concerns

**Purpose**: retire the subprocess backend (done LAST so `main` never loses
`--mic`), record watermarks, finish docs.

- [ ] T033 Remove the subprocess backend: delete `rust/myna-audio/src/pw_record.rs`, drop `mod pw_record;` and `pub use pw_record::PwRecordBackend;` from `rust/myna-audio/src/lib.rs`, and update the module-doc/backend table comments in `src/lib.rs` and `src/backend.rs` (research R8; FR-016) — **only after US1 (T017) is green on hardware**
- [ ] T034 [P] Update the orchestrator's stale reference in `rust/myna-orchestrator/src/fsm.rs:637` (the `"pw-record exited mid-capture…"` test/message string) to the native-backend wording; adjust any affected test
- [ ] T035 [P] Add the capture-path performance watermark test (peak/steady memory, CPU, stop latency) with a checked-in baseline + declared per-metric tolerance, gated with the integration suite in `rust/myna-audio/tests/pipewire_hw.rs` (constitution Principle III; SC-008, quickstart step 5). Capture the SC-008 baseline on the native path; if a subprocess-vs-native comparison is wanted, record the subprocess figure in T017 before removal (T033)
- [ ] T036 [P] Update `docs/audio-adapter-api.md` (§5 backend table, §9) to mark T52 done: `PipeWireBackend` is the sole live backend, channel pick/downmix + live enumeration supported, subprocess retired
- [ ] T037 [P] Update `docs/project-plan.md` T52 row to done with a short outcome note, and `README.md` `--mic`/backend wording (remove `pw-record` references)
- [ ] T038 Run the full quickstart acceptance checklist (steps 1–5) end to end; `cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings` green

**Checkpoint**: subprocess gone, native backend is the sole live-capture path,
watermarks recorded, docs current, workspace green.

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: no dependencies — start immediately
- **Foundational (Phase 2)**: depends on Setup — BLOCKS all user stories
- **US1 (Phase 3)**: depends on Foundational — the MVP
- **US2 (Phase 4)**: depends on US1 (extends the capture path)
- **US3 (Phase 5)**: depends on US1 (extends the capture path); independent of US2
- **US4 (Phase 6)**: depends on Foundational only (T005, T007) — independent of
  US1–US3; may run in parallel with US2/US3
- **Polish (Phase 7)**: T033 depends on US1 green on hardware (T017); T034–T038
  after their subject stories

### Story Dependency Graph

```text
Setup → Foundational ┬→ US1 (P1) ┬→ US2 (P2)
                     │            └→ US3 (P3)
                     └→ US4 (P3)   (parallel with US2/US3)
                                   → Polish (US1 must be HW-green before T033)
```

### Within Each Story

- Tests (write first, observe fail) → implementation → story checkpoint
- Models/mapping before services; services before CLI wiring

---

## Parallel Opportunities

- **Setup**: T003, T004 in parallel (after T002 links the dependency)
- **Foundational**: T006 + T007 (mapping test + its impl) parallel to T005 (loop)
- **US1 tests**: T008–T012 all [P] (distinct test bodies) — write together, watch fail
- **US2 tests**: T018–T020 [P]; **US3 tests**: T023–T024 [P]; **US4 tests**: T027–T029 [P]
- **Cross-story**: once Foundational lands, US4 (Phase 6) proceeds alongside US2/US3
- **Polish**: T034–T037 [P] (different files); T033 gated on T017, T038 last

### Parallel Example: US1 tests

```bash
# Write these together, ensure all fail, then implement T013–T015:
Task: "T008 seam width-rejection test in src/pipewire.rs"
Task: "T009 default-capture format+stop+dropped test in tests/pipewire_hw.rs"
Task: "T010 graph-conversion test in tests/pipewire_hw.rs"
Task: "T011 removal/abort fault test in tests/pipewire_hw.rs"
Task: "T012 stop-promptness + no-subprocess/no-disk test in tests/pipewire_hw.rs"
```

---

## Implementation Strategy

### Branch Staging Plan (REQUIRED — constitution "Staged Delivery in Feature Branches")

Each branch is one independently testable increment (tests + implementation
together), builds only on merged prerequisites, and leaves `main` green. The
subprocess removal is deliberately its **own final branch** so `--mic` never
breaks on `main`.

| # | Branch | Scope (phases/tasks) | Prerequisite branches | Merge gates |
|---|--------|----------------------|-----------------------|-------------|
| 1 | `002a-pw-setup-foundation` | Phase 1–2 (T001–T007) | — | hermetic suite green; workspace builds with pipewire dep; Workshop def present (Principle IV gate closes here) |
| 2 | `002b-native-capture-us1` | Phase 3 (T008–T017) | #1 | hermetic + gated integration (virtual-audio) green; live-dictation smoke |
| 3 | `002c-device-selection-us2` | Phase 4 (T018–T022) | #2 | hermetic + gated integration green |
| 4 | `002d-channel-select-us3` | Phase 5 (T023–T026) | #2 | hermetic + gated integration green |
| 5 | `002e-live-enumeration-us4` | Phase 6 (T027–T032) | #1 | hermetic + gated integration green (may merge before/after #3–#4) |
| 6 | `002f-retire-subprocess` | Phase 7 (T033–T038) | #2 (US1 HW-green), plus #3–#5 for docs completeness | full workspace + clippy green; watermark baseline recorded; quickstart acceptance passes |

Notes: branch #5 depends only on #1 and can land in parallel with #3/#4. Branch
#6 must not merge until US1 is verified green on real hardware (T017), because it
deletes the only other live-capture path.

### MVP First

1. Setup + Foundational (branches #1)
2. US1 native capture (branch #2) → **STOP & VALIDATE**: quickstart steps 1–3
3. Ship/demo: native `--mic` works, subprocess still present as safety net

### Incremental Delivery

1. Foundation → US1 (MVP) → validate
2. Add US2 (stable selection) → validate
3. Add US3 (channels) and US4 (enumeration) in parallel → validate each
4. Retire subprocess + polish (branch #6) → final acceptance

---

## Notes

- [P] = different files, no dependency on incomplete tasks
- All real-PipeWire tests are gated on `MYNA_PIPEWIRE_TESTS=1` and skip cleanly
  offline; the identical code runs on the VM's virtual-audio graph and on
  hardware (Principle II)
- Never persist audio; the stats tap carries levels/counters only (Principle V)
- Verify each test fails before implementing; commit per task or logical group
- Do not delete `pw_record.rs` (T033) until US1 is green on hardware (T017)
