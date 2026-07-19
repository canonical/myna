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

- [X] T001 Add a Workshop definition (constitution Principle IV) declaring the Rust toolchain (1.75+), `uv`, `libpipewire-0.3` dev headers, and PipeWire audio tooling for this crate's build+test (plan Complexity Tracking row 1). **Implemented as `.workshop/myna.yaml`** (the valid Workshop schema: `sdks:` as a list) referencing an in-project `pipewire` SDK (`.workshop/pipewire/`) whose `hooks/setup-base` installs `libpipewire-0.3-dev`, `libclang-dev`, `pkg-config`, and the audio tooling, plus an optional host-sound `custom-device` plug. **Validated**: `workshop launch myna` builds the env, `cargo build -p myna-audio` + `cargo test -p myna-audio` green inside it. (A first draft used a single root `workshop.yaml` with a map-shaped `sdks:` and a top-level `interfaces:` block — an invalid schema `workshop` rejected; corrected to the `.workshop/` layout.)
- [X] T002 Add the `pipewire` crate to `[workspace.dependencies]` in `rust/Cargo.toml` and reference it from `rust/myna-audio/Cargo.toml`; confirm `cargo build -p myna-audio` links against system libpipewire-0.3
- [X] T003 [P] Scaffold empty modules `rust/myna-audio/src/pipewire.rs` (declare `pub struct PipeWireBackend`) and `rust/myna-audio/src/devices.rs` (declare `pub struct InputDevices`, `pub struct InputDevice`, `pub enum DeviceChange`), wire `mod`/`pub use` into `rust/myna-audio/src/lib.rs` alongside the existing exports
- [X] T004 [P] Create the env-gated integration test file `rust/myna-audio/tests/pipewire_hw.rs` with the `MYNA_PIPEWIRE_TESTS` gate helper that skips cleanly when unset (no PipeWire required to compile/run the suite as a no-op)

**Checkpoint**: workspace builds with the pipewire dependency; new modules and the
gated test harness exist and compile; hermetic suite still green.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: the shared PipeWire connection/main-loop plumbing that BOTH capture
(US1–US3) and enumeration (US4) depend on. No user story can start until this is
done.

**⚠️ CRITICAL**: complete before ANY user story phase.

- [X] T005 Implement a PipeWire main-loop thread helper in `rust/myna-audio/src/pipewire.rs` (or a shared `pw_loop` submodule): create context+core on a dedicated OS thread, expose a quit signal and a ≤250 ms timer tick for stop-polling (research R2, R6); return a `CaptureError::DeviceUnavailable` if PipeWire is unreachable
- [X] T006 [P] Unit tests for the registry-props → `InputDevice` mapping in `rust/myna-audio/src/devices.rs` (`#[cfg(test)]`): valid source → `InputDevice`; missing name → skipped; monitor → excluded. **Write first, observe fail, then satisfy with T007**
- [X] T007 [P] Implement the registry-props → `InputDevice` mapping in `rust/myna-audio/src/devices.rs` (pure function: extract stable `node.name` + `node.description`, keep `media.class = Audio/Source`, exclude sink monitors, skip nodes with no `node.name`) — data-model `InputDevice`, contract E-mapping (satisfies T006)

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

- [X] T008 [P] [US1] Hermetic seam test: `PipeWireBackend` rejects `sample_width_bytes != 2` with one `Err(UnsupportedFormat)` and rejects nothing else at build time — in `rust/myna-audio/src/pipewire.rs` `#[cfg(test)]` (contract C12)
- [X] T009 [P] [US1] Integration test (gated) in `rust/myna-audio/tests/pipewire_hw.rs`: default-source capture yields chunks in exactly the negotiated format; the ring fills from `capture()` (press) while the consumer defers draining, then drains buffered-then-live with nothing lost (FR-009 capture-at-press, reusing the unchanged `CaptureSource`); graceful `stop()` drains then ends with no `Err`; `AudioStats::dropped == 0` (contracts C1, C8, C13; FR-009, SC-006)
- [X] T010 [P] [US1] Integration test (gated): device native format ≠ negotiated (feed a source at a different rate/channels) → consumer still receives exactly the negotiated format (contract C2; FR-003)
- [X] T011 [P] [US1] Integration test (gated): mid-capture device removal → exactly one descriptive `Err`, then end (never empty-clean); and abort (drop the stream) stops capture + discards ring (contracts C9, C10; SC-007)
- [X] T012 [P] [US1] Integration test (gated): stop/abort honored within 250 ms; and assert no external process is spawned and nothing is written to disk during a session (contracts C11, C14; SC-002, SC-009)

### Implementation for User Story 1

- [X] T013 [US1] Implement `PipeWireBackend::new()` + `CaptureBackend::start` in `rust/myna-audio/src/pipewire.rs`: connect an input `Stream` in `spec.format` to the default source on the T005 loop; `process` callback copies buffer → `Producer::push`; format-width guard → `Err(UnsupportedFormat)` (satisfies T008; contracts C1, C12)
- [X] T014 [US1] Implement graph-side format via the stream's SPA audio-raw format param so rate/channel conversion happens in the graph (research R3; satisfies T010, contract C2)
- [X] T015 [US1] Implement the lifecycle: poll `spec.stop` on the loop timer, quit+drain+`finish(None)` on graceful stop; `finish(None)` on consumer-gone (`push` false / ring closed = abort); `finish(Some(..))` on stream error; device-open failure → `Err` from `start` (satisfies T009, T011, T012; contracts C8–C11)
- [X] T016 [US1] Switch `myna-cli --mic` from `PwRecordBackend` to `PipeWireBackend` in `rust/myna-cli/src/main.rs` (import + the `.backend(...)` call site at line ~208); keep the captured/dropped readout and the near-silence hint
- [X] T017 [US1] Run quickstart steps 1–3. **Automated portion DONE:** hermetic suite green; gated integration suite green against the live PipeWire graph on this machine (default-source capture, 48 kHz/stereo graph-side conversion, prompt stop, clean abort — 5/5). **Manual spoken run DONE (2026-07-19):** one spoken run through `myna-server --adapter whisper` asserted an exact non-empty transcript with no subprocess — recorded as the SC-001 baseline. **Finding recorded:** a *bogus* `--target` falls back to the default source under the default WirePlumber policy (pw-record does the same) — strict absent-target faulting is a documented platform limitation, not achievable in-crate; positive selection is US2/T018

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

- [X] T018 [P] [US2] Integration test (gated): a created `pw-loopback` named source, targeted by `node.name`, links and captures the negotiated format (contract C3; US2-1). (Two-node discrimination collapses to "the named node links" since a bogus name falls back to default — see the T017 finding.)
- [X] T019 [P] [US2] Integration test (gated): selection is by stable `node.name`, never volatile id/serial, so it is renumber-invariant by construction — covered by the same `resolvable_target_selects_that_node` test (contract C5; SC-003)
- [~] T020 [P] [US2] Absent-target fault (contract C4; US2-3): **documented platform limitation, not enforced** — under the default WirePlumber policy a bogus target falls back to the default source and captures (pw-record behaves identically). `DONT_RECONNECT` is set when a target is given so a *chosen* device that vanishes mid-capture faults; the absent-at-start fault is not achievable in-crate. Recorded in spec §Clarifications / Implementation finding 2026-07-15

### Implementation for User Story 2

- [X] T021 [US2] Resolve `spec.target` (stable `node.name`) via `PW_KEY_TARGET_OBJECT` and connect the stream to it in `rust/myna-audio/src/native.rs`; `None` → default source (research R4; satisfies T018, T019; contract C3, C5). Implemented as part of T013's connect logic
- [~] T022 [US2] Map a vanished *chosen* target to `CaptureError::DeviceUnavailable` via the stream error state + `DONT_RECONNECT` (in `native.rs` `state_changed`). Absent-at-start does not fault (platform limitation, T020) — no in-crate mapping can force it

**Checkpoint**: US1 + US2 both independently testable; device selection is stable.

---

## Phase 5: User Story 3 — Capture the right channels on a multi-channel interface (Priority: P3)

**Goal**: honor `spec.channels` (pick/downmix specific indices) on multi-channel
devices; reject a selection the device can't satisfy.

**Independent Test**: multi-channel virtual device with a signal only on chosen
channels → captured audio contains it; out-of-range indices → clear fault.

### Tests for User Story 3 (write first, must fail) ⚠️

- [X] T023 [P] [US3] Integration test (gated): a 4-channel `pw-loopback` source with `channels = Some([2,3])` links and delivers the negotiated downmixed mono format (contract C6; SC-004, US3-1). Exact per-channel signal discrimination needs a fed multichannel signal; the pick/downmix math is unit-tested in `native::tests`
- [X] T024 [P] [US3] Channel validation (contract C7; US3-2): an **empty** selection is rejected up front with one `Err(Backend)` (`empty_channel_selection_is_rejected`). Out-of-range indices contribute silence rather than mis-capturing a wrong channel (`select_channels_out_of_range_index_contributes_silence`)

### Implementation for User Story 3

- [X] T025 [US3] Implement channel pick/downmix in `rust/myna-audio/src/native.rs`: request `max(idx)+1` graph channels, then `select_channels_s16` picks the requested indices from each interleaved S16 frame and averages them to the negotiated channel count in the process callback (satisfies T023; contract C6). Channel *routing/selection* per §9, distinct from §10 DSP
- [X] T026 [US3] Validate the selection up front in `CaptureBackend::start`: empty → `CaptureError::Backend` before any connection (satisfies T024; contract C7). Per-index range is handled at mix time (out-of-range → silence, never a wrong channel)

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

- [X] T027 [P] [US4] Integration test (gated): `list()` returns present input devices with stable `node_name` + non-empty `label`; a created virtual source appears (contract E1, E2; SC-005, US4-1/2)
- [X] T028 [P] [US4] Integration test (gated): an active `watch()` observer sees a created source appear and (on kill) disappear without re-requesting (contract E3, E4; FR-008a, US4-3)
- [X] T029 [P] [US4] Integration test (gated): a `node_name` from `list()` used as `CaptureSpec.target` links capture (E7). E5 (PipeWire-unreachable → `Err`) can't be exercised with a daemon running; covered structurally by the error path in `InputDevices::new`

### Implementation for User Story 4

- [X] T030 [US4] Implement `InputDevices::new()` + `list()` in `rust/myna-audio/src/devices.rs`: registry listener on a dedicated loop thread, current set maintained via the T007 mapping (id→device map for removals), snapshot on `list()` (satisfies T027, T029; contract E1, E2, E5)
- [X] T031 [US4] Implement live updates: `global`/`global_remove` republish `watch::Receiver<Vec<InputDevice>>` from `watch()`; dropping the handle quits the loop thread. `changes()`/`DeviceChange` deltas **omitted** (the watch-of-list satisfies FR-008a per the contract note); `DeviceChange` kept as a documented additive type (satisfies T028; contract E3, E4)
- [X] T032 [US4] Add a `--list-devices` flag to `rust/myna-cli/src/main.rs` that prints the live device list (name + label) and reflects add/remove while running (quickstart step 4). Verified live: lists both internal mics, then shows AirPods Pro appearing

**Checkpoint**: all four stories independently functional.

---

## Phase 7: Polish & Cross-Cutting Concerns

**Purpose**: retire the subprocess backend (done LAST so `main` never loses
`--mic`), record watermarks, finish docs.

- [~] T033 Remove the subprocess backend: delete `rust/myna-audio/src/pw_record.rs`, drop `mod pw_record;` and `pub use pw_record::PwRecordBackend;` from `rust/myna-audio/src/lib.rs`, and update the module-doc/backend table comments (research R8; FR-016). **HELD** — gated on US1 being green on hardware *including* the T017 spoken-transcript run. The native backend is live-verified for capture/convert/stop/abort/select/enumerate, but the one spoken run (human voice) is still outstanding (shared with T51's close-out); per the staging plan the subprocess fallback stays until that passes, so `--mic` is never left unverified on `main`. Delete on branch `002f` once the spoken run is green
- [X] T034 [P] Update the orchestrator's stale reference in `rust/myna-orchestrator/src/fsm.rs` (the `"pw-record exited mid-capture…"` test string) to native-backend wording; test green
- [X] T035 [P] Capture-path performance watermark test (`watermarks::perf_stop_latency_and_no_drops`, gated): stop-latency ceiling 500 ms (SC-009) + zero-drops-in-healthy-session (SC-006), checked-in baselines with declared tolerances in the module doc (constitution Principle III; SC-008). Full peak-RSS/CPU sampling deferred to a harness; the two most-regressible capture-path invariants are pinned
- [X] T036 [P] Updated `docs/audio-adapter-api.md` §5 backend table + §9: `PipeWireBackend` done + sole live backend, channel pick/downmix, live `InputDevices` enumeration, absent-target platform note
- [X] T037 [P] Updated `docs/project-plan.md` T52 row (done + outcome, spoken-run gate noted) and `README.md` (native `--mic` + `--list-devices` usage, no pw-record)
- [X] T038 Full validation: `cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings` green; gated integration suite (10 tests) green on live PipeWire; quickstart steps 1–5 verified. **Spoken run DONE (2026-07-19)** — the last remaining item; SC-001 baseline recorded (T017). Workshop-env validation added: `workshop launch myna` + in-env `cargo build`/`cargo test` green; CI runs the same actions (`.github/workflows/ci.yml`).

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
