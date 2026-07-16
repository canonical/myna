# Tasks: Audio Adapter Library

**Input**: Design documents from `specs/001-rust-audio-adapter/`

**Prerequisites**: `spec.md`, `plan.md`, `research.md`, `data-model.md`, `contracts/audio-adapter-api.md`, `quickstart.md`, `diagrams.md`

**Tests**: Test tasks are REQUIRED and precede their implementation tasks (red-green TDD, constitution Principle I). Contract guarantees G1–G15 are defined in `contracts/audio-adapter-api.md`.

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story. Cross-cutting phases (setup, foundational, sandbox regime, polish) carry no story label.

## Format: `[ID] [P?] [Story?] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: User story label (US1, US2, US3) — user-story phases only
- Include exact file paths in descriptions

## Phase 1: Workspace, Sandbox, and Crate Setup

**Purpose**: Cargo workspace, Workshop sandbox definition, crate scaffolding.

- [x] T001 Create root `Cargo.toml` workspace manifest including `crates/audio-adapter` member
- [x] T002 Create `crates/audio-adapter/Cargo.toml` with crate name `myna-audio-adapter`, default features `pipewire` + `pulse`, optional features `vad`, `denoise`, `async`, `test-util`
- [x] T003 [P] Create `workshop.yaml` at repo root — Canonical Workshop sandbox definition (constitution Principle IV, FR-021): Rust toolchain, `libpipewire-0.3-dev`/`libpulse-dev`/`clang`/`pkg-config`, `pipewire-utils`/`pulseaudio-utils`, virtual-audio provisioning, and a launch-time backend selection knob (`pipewire` default | `pulse`)
- [x] T004 [P] Create crate directory layout per plan.md: `crates/audio-adapter/src/` (stub modules), `tests/`, `tests/fixtures/`, `examples/`
- [x] T005 [P] Write `crates/audio-adapter/README.md` with system prerequisites for host builds and the Workshop sandbox flow (`workshop launch` / `workshop exec`) as the recommended path

**Checkpoint**: `workshop launch` brings up the sandbox; empty crate builds inside it and on the host.

---

## Phase 2: Foundational Types and Contracts

**Purpose**: Core data model and error/contract types every user story depends on (data-model.md).

**⚠️ CRITICAL**: No user story implementation can begin until this phase is complete.

- [x] T006 [P] Implement `Error` enum in `crates/audio-adapter/src/error.rs` (`NoDevice`, `PermissionDenied`, `UnsupportedFormat`, `DeviceLost`, `Backend`) per FR-012
- [x] T007 [P] Implement `SampleFormat` and `AudioFormat` in `crates/audio-adapter/src/format.rs` with default target 16 kHz / mono / S16LE
- [x] T008 [P] Implement `NodeId` and `InputNode` in `crates/audio-adapter/src/node.rs` (`id`, `name`, `description`, `is_default`, `supported_formats`)
- [x] T009 [P] Implement `StreamConfig`, `PreprocessConfig`, `NodeSelector`, `BackendSelector` in `crates/audio-adapter/src/config.rs` with data-model.md validation rules (rate bounds, `max_buffer_duration` > 0, default 10 s)
- [x] T010 [P] Implement `AudioFrame`, `StreamEvent`, `StreamItem` in `crates/audio-adapter/src/frame.rs` with timing metadata and `seq`
- [x] T011 [P] Mark public enums (`Error`, `StreamEvent`, `StreamItem`) `#[non_exhaustive]` per contract §Stability

**Checkpoint**: Foundational types compile; crate builds with `--no-default-features`.

---

## Phase 3: User Story 1 - Open an Audio Stream and Deliver Frames (Priority: P1) 🎯 MVP

**Goal**: Stateless `enumerate_nodes()` / `open_stream()` facade delivering contiguous, artifact-free target-format frames, with overrun/underrun/device-loss semantics.

**Independent Test**: With `MockBackend`, open a stream, read frames, force overrun/underrun/device-loss, close — asserting G2, G3, G4, G5, G7, G8, G9, G13, G15.

### Tests for User Story 1 (write FIRST, observe them FAIL)

- [x] T012 [P] [US1] Create `MockBackend` in `crates/audio-adapter/src/backend/mock.rs` (feature `test-util`): feeds deterministic WAV fixtures, injects underrun gaps, format changes, and device-loss on command
- [x] T013 [P] [US1] Contract tests in `crates/audio-adapter/tests/contract.rs` for G7 (idempotent open), G8 (close releases ≤ 200 ms, buffers cleared), G9 (first frame ≤ 100 ms); G13 (no disk persistence) is verified by the strace privacy check (T057), not by an in-process test
- [x] T014 [P] [US1] Contract tests in `crates/audio-adapter/tests/contract.rs` for G3 (buffer full → oldest dropped, exactly one `Overrun{dropped}` per loss span, smoothed splice — assert no discontinuity above fade threshold) and G4 (server gap → silence fill keeps `seq`/timestamps continuous, one `Underrun{filled}` event, smoothed fill boundaries) per FR-014/FR-015/FR-018
- [x] T015 [P] [US1] Consumer-scenario test in `crates/audio-adapter/tests/consumer_scenario.rs` (G15, FR-020): replay the Speech Controller call pattern from contracts §Known consumer — enumerate → open → timed read loop matching `Frame`/`DeviceLost`/`Overrun` items → close — against `MockBackend` (extended with `VoiceActivity` handling in US3)
- [ ] T016 [US1] Integration tests in `crates/audio-adapter/tests/integration.rs` (gated: `MYNA_AUDIO_IT=1` + `#[ignore]`): node enumeration with metadata, idempotent open on a real server, device-lost via virtual-node removal (G5), `NoDevice` on nonexistent node

### Implementation for User Story 1

- [x] T017 [US1] Define `AudioBackend` trait and auto-probe (PipeWire → Pulse) in `crates/audio-adapter/src/backend/mod.rs`
- [ ] T018 [US1] Implement PipeWire backend in `crates/audio-adapter/src/backend/pipewire.rs`: registry-based node enumeration, server-side format negotiation (FR-009), RT capture callback, device-lost detection
- [ ] T019 [US1] Implement PulseAudio fallback backend in `crates/audio-adapter/src/backend/pulse.rs`: source enumeration, capture stream, device-lost detection
- [x] T020 [US1] Implement bounded SPSC ring buffer in `crates/audio-adapter/src/ring.rs`: drop-oldest on overflow, loss-span accounting, raised-cosine splice smoothing (FR-014/FR-015)
- [ ] T021 [US1] Implement underrun detection and silence fill with smoothed boundaries + `Underrun` events in the capture path (`ring.rs`/backend modules) per FR-018
- [x] T022 [US1] Implement `AudioStream` in `crates/audio-adapter/src/stream.rs` (`read`, `read_timeout`, `close`, `node`, `target_format`) draining frames and interleaved events in timeline order
- [x] T023 [US1] Implement public facade in `crates/audio-adapter/src/lib.rs` (`enumerate_nodes`, `open_stream` with idempotent per-node handle lookup per FR-003)

**Checkpoint**: US1 contract, consumer-scenario, and integration tests pass; MVP demonstrable.

---

## Phase 4: User Story 2 - Resample and Format-Convert to STT-Compatible Audio (Priority: P2)

**Goal**: Any supported source format converts to the configured target; mid-stream renegotiation is transparent.

**Independent Test**: Feed a 48 kHz stereo fixture through the pipeline; every output frame is 16 kHz mono S16LE (G1), timeline contiguous (G2), renegotiation transparent (G6).

### Tests for User Story 2 (write FIRST, observe them FAIL)

- [ ] T024 [P] [US2] Add WAV fixtures to `crates/audio-adapter/tests/fixtures/` (`speech_48k_stereo.wav`, `speech_16k_mono.wav`, format-change sequence)
- [ ] T025 [P] [US2] Contract tests in `crates/audio-adapter/tests/contract.rs` for G1 (exact target-format match), G2 (contiguity across conversion), G6 (mid-stream renegotiation continues; `UnsupportedFormat` only when unconvertible)
- [ ] T026 [P] [US2] Resampling-quality test in `crates/audio-adapter/src/convert/resample.rs` (unit): `rubato` output vs high-quality reference resample of a fixture, total sample error < 1% over the clip (SC-002)
- [ ] T027 [US2] Integration test in `crates/audio-adapter/tests/integration.rs`: capture from a 48 kHz stereo virtual node, assert 16 kHz mono delivery

### Implementation for User Story 2

- [x] T028 [P] [US2] Sample-format and channel-mixdown conversion in `crates/audio-adapter/src/convert/channels.rs`
- [x] T029 [US2] `rubato`-based fallback resampler in `crates/audio-adapter/src/convert/resample.rs` (allocation-free in steady state)
- [x] T030 [US2] Conversion pipeline in `crates/audio-adapter/src/convert/mod.rs`: bypass when the server delivers target format, in-process convert otherwise
- [x] T031 [US2] Wire conversion into the `AudioStream` production path so every read yields target-format frames
- [x] T032 [US2] Transparent mid-stream renegotiation in backends + convert pipeline (FR-017)

**Checkpoint**: US2 tests pass; US1 suite still green over converted output.

---

## Phase 5: User Story 3 - Optional Preprocessing for Better Transcription Quality (Priority: P3)

**Goal**: Feature-gated denoise and VAD stages chained before delivery; pass-through untouched when disabled.

**Independent Test**: Run stages on noisy fixtures — non-speech attenuated, `VoiceActivity` transitions fire (G11); disabled preprocessing adds no latency stage (G12).

### Tests for User Story 3 (write FIRST, observe them FAIL)

- [ ] T033 [P] [US3] Add noisy/reverberant fixtures (`noisy_speech.wav`, silence-bounded utterances) to `crates/audio-adapter/tests/fixtures/`
- [ ] T034 [P] [US3] Unit tests for the denoise stage in `crates/audio-adapter/src/preprocess/denoise.rs` (non-speech RMS reduced, speech intact)
- [ ] T035 [P] [US3] Unit tests for the VAD stage in `crates/audio-adapter/src/preprocess/vad.rs` (transition events at fixture speech boundaries)
- [ ] T036 [US3] Contract tests in `crates/audio-adapter/tests/contract.rs` for G11 (VAD events on speech stop) and G12 (pass-through when disabled); extend `tests/consumer_scenario.rs` with `VoiceActivity` utterance-chunking handling (G15)

### Implementation for User Story 3

- [ ] T037 [US3] Define `PreprocessStage` trait and stage chain in `crates/audio-adapter/src/preprocess/mod.rs`
- [ ] T038 [P] [US3] Implement RNNoise denoise stage (`nnnoiseless`, feature `denoise`) in `crates/audio-adapter/src/preprocess/denoise.rs`
- [ ] T039 [P] [US3] Implement Silero VAD stage (feature `vad`) in `crates/audio-adapter/src/preprocess/vad.rs`, emitting `StreamEvent::VoiceActivity`
- [ ] T040 [US3] Wire the chain into `AudioStream` per `StreamConfig.preprocess`
- [x] T041 [US3] Document `DeverbStage` deferral in `crates/audio-adapter/src/preprocess/mod.rs` (FR-011 MAY; non-breaking future stage)
- [ ] T042 [US3] SC-005 accuracy benchmark: define reference noisy/reverberant corpus + STT harness (or reference the myna testbed corpus) in `crates/audio-adapter/benches/accuracy.md` procedure doc; run baseline vs preprocessed, record results

**Checkpoint**: US3 green with `--features vad,denoise`; US1/US2 green with and without preprocessing.

---

## Phase 6: Async Adapter and Examples

**Purpose**: Optional async interface and the quickstart's runnable conformance examples.

- [ ] T043 [P] Implement `futures::Stream` adapter (feature `async`) in `crates/audio-adapter/src/async_stream.rs` with a smoke test
- [x] T044 Create `crates/audio-adapter/examples/capture_check.rs`: measure first-frame latency, steady-state lag, close/release time; non-zero exit outside SC-001/SC-003/SC-004 targets
- [ ] T045 Create `crates/audio-adapter/examples/preprocess_check.rs`: VAD/denoise validation on a fixture file per quickstart
- [ ] T046 [P] Add fixture-generation script + docs in `crates/audio-adapter/tests/fixtures/README.md`

---

## Phase 7: Sandboxed Test Regime (FR-021/FR-022)

**Purpose**: Make the Workshop sandbox the working, verified test environment across both backends.

- [ ] T047 Declare per-backend test subsets (FR-021 clarification): tag PipeWire-only tests (native node enumeration, session-manager routing) vs common tests via test-name convention or feature flags; check in the subset declaration in `crates/audio-adapter/tests/README.md`; ensure results record the backend exercised
- [ ] T048 Backend-matrix sandbox runs: `workshop launch` with PipeWire and with PulseAudio; each backend passes 100% of its declared subset (SC-007), including the `speech_controller_session_flow` integration variant of the consumer-scenario test against the virtual node
- [ ] T049 Host-isolation and teardown verification (SC-008, FR-022): snapshot host audio devices/daemons before/during/after a sandboxed run — zero changes; after teardown no processes, devices, or files remain on the host
- [ ] T050 Offline-after-provisioning check (FR-022): after initial provisioning cache, relaunch sandbox and run the suites with network disabled — all pass
- [ ] T051 Environment-vs-test failure distinction (FR-022): sandbox entry points exit with distinct, documented codes/messages for provisioning failures (Workshop missing, virtualization unavailable, backend failed to start) vs test failures

---

## Phase 8: Polish & Cross-Cutting Concerns

**Purpose**: Documentation, quality gates, performance watermarks, release readiness.

- [ ] T052 [P] Write crate-level rustdoc in `crates/audio-adapter/src/lib.rs`, including the Speech Controller consumer-surface section mirroring contracts §Known consumer (FR-020)
- [ ] T053 Verify and update `specs/001-rust-audio-adapter/diagrams.md` against as-built behavior (FR-019) — this is the single canonical diagram location; do not create a second copy under `crates/`
- [ ] T054 Performance watermark baselines (constitution Principle III): measure peak/steady-state memory, CPU, end-to-end latency, and buffer occupancy in the Workshop container and sandbox VM profile; check in baselines with declared per-metric tolerances under `crates/audio-adapter/benches/watermarks/`; add a tolerance-checked regression test wired into the suite
- [ ] T055 [P] Run `cargo clippy --all-features -- -D warnings` and fix all issues
- [ ] T056 Run the full matrix: `--no-default-features`, `--all-features`, and feature combos; full suite incl. `MYNA_AUDIO_IT=1` integration
- [ ] T057 Privacy checks: `strace` no-audio-write sweep and `unshare -n` network-free run per quickstart (FR-007, SC-006, G13/G14)
- [ ] T058 Add `CHANGELOG.md` entry for the new crate
- [ ] T059 Verify `quickstart.md` executes exactly as documented — sandbox flow first, host flows second

---

## Dependencies & Execution Order

### Phase Dependencies

- **Phase 1 (Setup)**: none — start immediately; T003 (`workshop.yaml`) unblocks all sandboxed gates.
- **Phase 2 (Foundational)**: after Phase 1; BLOCKS all user stories.
- **Phase 3 (US1)**: after Phase 2 — MVP.
- **Phase 4 (US2)**: after Phase 2; end-to-end paths consume US1's stream/ring.
- **Phase 5 (US3)**: after Phase 2; stage unit tests independent, end-to-end needs US2.
- **Phase 6 (Async/Examples)**: after US1 (capture_check), US3 (preprocess_check).
- **Phase 7 (Sandbox regime)**: T047 after US1 tests exist; T048–T051 after the suites they run (full value after Phase 6).
- **Phase 8 (Polish)**: after all prior phases; T054 needs the sandbox (Phase 7).

### Within Each User Story

- Tests written and observed failing before implementation (constitution Principle I).
- US1: mock/trait → backends → ring/stream → facade.
- US2: channels → resampler → pipeline → stream wiring → renegotiation.
- US3: trait → stages → chain wiring.

### Parallel Opportunities

- Phase 1: T003, T004, T005 in parallel after T001/T002.
- Phase 2: T006–T011 all parallel.
- US1: T012–T015 (test files) parallel; T018 and T019 (two backends) parallel after T017.
- US2: T024–T026 parallel; T028 parallel with T029.
- US3: T033–T035 parallel; T038 and T039 parallel after T037.
- Phase 8: T052, T055 parallel with each other.

---

## Implementation Strategy

### Branch Staging Plan (constitution "Staged Delivery in Feature Branches")

| # | Branch | Scope (phases/stories) | Prerequisite branches | Merge gates |
|---|--------|------------------------|-----------------------|-------------|
| 1 | `001-audio-foundation` | Phase 1–2 (workspace, `workshop.yaml`, foundational types) | — | crate builds (host + sandbox smoke: `workshop launch` succeeds); hermetic type tests |
| 2 | `001-audio-us1-capture` | Phase 3 (US1 tests + implementation) | #1 | hermetic contract + consumer-scenario suites; sandbox integration, PipeWire subset |
| 3 | `001-audio-us2-convert` | Phase 4 (US2) | #2 | hermetic suites; sandbox integration incl. 48 kHz virtual-node conversion |
| 4 | `001-audio-us3-preprocess` | Phase 5 (US3) | #3 | hermetic suites with `--features vad,denoise`; SC-005 benchmark recorded |
| 5 | `001-audio-async-examples` | Phase 6 | #4 | hermetic suites; `capture_check`/`preprocess_check` conformance in sandbox |
| 6 | `001-audio-sandbox-matrix` | Phase 7 (subsets, backend matrix, isolation/offline checks) | #5 | both backend subsets 100% green (SC-007); isolation (SC-008) + offline checks pass |
| 7 | `001-audio-polish-watermarks` | Phase 8 | #6 | full feature matrix; watermark baselines checked in, tolerance test green; privacy checks |

Each branch carries its increment's tests and implementation together and leaves `main` green at merge; no branch builds on unmerged sibling work.

### MVP First (User Story 1 Only)

1. Branches 1–2 (Setup + Foundational + US1).
2. **STOP and VALIDATE**: contract + consumer-scenario suites, `capture_check` in the sandbox.
3. Demo capture/streaming.

### Incremental Delivery

Branch-by-branch per the staging plan: each merge is an independently tested increment (capture → conversion → preprocessing → examples → sandbox matrix → watermarks/polish).

### Parallel Team Strategy

After branch 1 merges: Developer A takes US1 backends (T018/T019 parallel), Developer B prepares US2 conversion tests/impl on a branch stacked on #2, Developer C prepares US3 stages (unit-testable against the trait). Branches still merge in staging-plan order.

---

## Notes

- **Feature flags**: `pipewire` + `pulse` default; `vad`, `denoise`, `async`, `test-util` optional. CI tests `--no-default-features`, `--all-features`, and per-feature combos (T056).
- **Integration tests**: gated behind `MYNA_AUDIO_IT=1` + `#[ignore]`; hermetic `cargo test` needs no audio server. Per-backend subsets are explicitly declared (T047) — PipeWire-only tests do not run under the PulseAudio selection.
- **Test-first**: contract tests for G1–G15 exist and fail before the implementations they assert; `MockBackend` keeps user stories progressing without a live audio server.
- **Sandbox-first**: the Workshop sandbox (T003) is the reference environment — reviewers should be able to reproduce every gate with `workshop launch` + `workshop exec`.
- **No disk persistence / offline**: verified by T057 (host) and T050 (sandbox).
- **Deferred deverb**: FR-011 (MAY) documented as a future `PreprocessStage` (T041).
