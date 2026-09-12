# Tasks: Progressive Streaming Emission

**Input**: Design documents from `/specs/008-progressive-emission/`

**Prerequisites**: plan.md (required), spec.md (required for user stories), research.md, data-model.md, contracts/

**Tests**: All implementation is Python evaluation-harness tier (constitution: TDD-exempt) — test tasks are included where they carry the load (strategy commit rules, emission invariants I1–I7) but are not red-green mandatory. The Rust client is untouched by this feature; any client change discovered during implementation becomes a new TDD task here.

**Organization**: Tasks are grouped by user story. Spike S1 (T005) gates the US1 default strategy; spike S2 (T018) gates US2. Both spikes have pre-decided fallbacks (research.md D3/D6) — a no-go does not block the story, it selects the fallback.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (US1–US4)
- Include exact file paths in descriptions

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Config surface, dependency extras, and measurement scaffolding

- [X] T001 Verify feature documentation structure is complete in `specs/008-progressive-emission/` (spec, plan, research, data-model, contracts/×2, quickstart)
- [X] T002 [P] Add `--strategy {local-agreement,tail-mutation,fixed-head}`, `--stream-cadence-s`, `--stream-window-cap-s` argument parsing and whisper-only validation to `server/src/myna/server/cli.py` per `contracts/strategy-config.md`
- [X] T003 [P] Add `parakeet` extra (onnxruntime) and `sherpa` extra (sherpa-onnx) to `server/pyproject.toml`
- [X] T004 [P] Add emission watermark fields (time_to_first_unstable, time_to_first_committed, finalize_latency_s) to the `dev/matrix.py` output schema alongside the 007 streaming columns

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Spike S1 gate, shared streaming machinery, and the invariant harness every story is validated against

**⚠️ CRITICAL**: No user story work can begin until this phase is complete

- [X] T005 **Spike S1** (CPU, ≤1 day): implement `dev/spikes/word_ts_stability.py` — decode growing prefixes (2 s steps) of ≥ 10 `corpus/english/` clips with faster-whisper `word_timestamps=True`; measure adjacent-pass word-sequence agreement rate and per-word timestamp drift; write findings + go/no-go to `results/spike-s1-word-timestamps.md` per `research.md` Decision 3 (≥ ~90 % agreement, drift ≲ 0.3 s ⇒ local-agreement default; else tail-mutation default, local-agreement falls back to segment-text-prefix agreement)
- [X] T006 Create bounded rolling-window machinery in `server/src/myna/testbed/streaming/window.py`: uncommitted PCM buffer, committed-frontier advancement (drops audio before frontier — constitution V), `window_cap_seconds` force-commit trigger, `overlap_seconds` tail carry per `data-model.md` StreamingSessionState
- [X] T007 Create the strategy seam in `server/src/myna/testbed/streaming/strategies.py`: `StreamingStrategy` protocol (`commit_rule(last_hypothesis, current_hypothesis) -> committed_prefix | None`), `Hypothesis`/`Word` types, strategy registry keyed by the `--strategy` names
- [X] T008 Create the emission-invariant harness in `server/tests/testbed/test_emission_invariants.py`: assertions I1–I7 from `contracts/emission-semantics.md` over recorded event streams (append-only commit, final == concatenation, unstable supersedes unstable, commit clears unstable, no unstable limbo, bounded window, batch degenerate)

**Checkpoint**: S1 default decided; window + strategy seam + invariant harness ready — story implementation can begin

---

## Phase 3: User Story 1 — Whisper emits hypotheses and committed text mid-utterance (Priority: P1) 🎯 MVP

**Goal**: The whisper adapter streams: unstable hypotheses while audio arrives, committed segments before end-of-audio, three selectable strategies, wire-invisible to clients

**Independent Test**: `myna-dictate --clip <≥8 s real clip> --mode streaming --show-unstable` against `myna-server --adapter whisper --streaming --strategy <each>` shows `~` lines during playback, ≥ 1 `»` before clip end, `✓` == concatenation of `»` (quickstart S1; SC-006)

### Tests for User Story 1 (harness tier — load-bearing, not red-green mandatory)

- [X] T009 [P] [US1] Strategy commit-rule unit tests over synthetic hypothesis sequences (agreement prefix, trailing-segment holdback, stuck-partial escape, VAD cut points) in `server/tests/testbed/test_streaming_strategies.py`
- [X] T010 [P] [US1] Batch-degenerate regression test (I7): streaming off ⇒ single committed segment in `server/tests/testbed/test_whisper_streaming.py`

### Implementation for User Story 1

- [X] T011 [US1] Implement tail-mutation strategy (commit all complete segments except trailing; trailing = unstable; N≈10 stuck-partial escape) in `server/src/myna/testbed/streaming/strategies.py` per `contracts/emission-semantics.md`
- [X] T012 [P] [US1] Implement fixed-head strategy (energy/VAD segmentation; starting constants arm 15 s / 500 ms cut / 60 s force-cut / 1 s overlap / 6-word dedupe) in `server/src/myna/testbed/streaming/strategies.py`
- [X] T013 [US1] Implement local-agreement strategy per S1 outcome (word-prefix agreement, drift ≤ 0.3 s, no commits within 0.5 s of window tail; or segment-text-prefix variant if S1 no-go) in `server/src/myna/testbed/streaming/strategies.py`
- [X] T014 [US1] Rework the whisper adapter streaming path in `server/src/myna/testbed/whisper.py` `run_session`: while the audio iterator produces chunks, tick the re-decode loop at `cadence_seconds` (decode uncommitted window in a worker thread) → `commit_rule` → emit committed (`segment_index`) / unstable events; end-of-audio resolves the tail per I5. Remove the `Disposition.COMMITTED if self._streaming else Disposition.COMMITTED` no-op (line ~211)
- [X] T015 [US1] Wire `--strategy`/cadence/window-cap from `server/src/myna/server/cli.py` through `server/src/myna/server/lifecycle.py` into the whisper adapter constructor
- [X] T016 [US1] Record whisper × 3 strategies emission watermarks via `dev/bench.py` into `results/streaming-watermarks.json`; evaluate SC-001/SC-002/SC-003 gates on the real corpus (long-stream corpus `corpus/english/manifest-streams.json`; beam-size isolation sweep included)
- [X] T017 [US1] Live-validate quickstart S1 for all three strategies (realtime clip, `~`/`»` during playback, batch-mode regression without `--streaming`)

**Checkpoint**: US1 fully functional — the 2026-07-27 failing manual validation passes for whisper (SC-006, whisper half)

---

## Phase 4: User Story 2 — Nemotron native frame-once streaming (Priority: P1)

**Goal**: The nemotron adapter streams via NeMo's cache-aware incremental path: each frame encoded once, latency independent of utterance length

**Independent Test**: 30 s realtime clip → continuous `~` partials, `»` commits at natural boundaries, terminal `✓` ≤ 1 s after clip end; TTFC within 1.5× of a 5 s clip (quickstart S2; SC-004). Requires NVIDIA PC.

- [x] T018 [US2] **Spike S2** (GPU, ≤1 day): implement `dev/spikes/nemo_streaming_feed.py` on the NVIDIA PC — pin the NeMo 2.7.3 live-push pattern (`CacheAwareStreamingAudioBuffer` + `conformer_stream_step`; `FrameBatchASR` confirmed offline-simulation-only per `research.md` Decision 6), per-step partial stability, finalize latency at three `att_context_size` settings on a 30 s real clip; findings + GO to `results/spike-s2-nemo-streaming.md`
- [x] T019 [US2] Implement the incremental streaming branch in `server/src/myna/testbed/nemotron.py` `run_session` per the S2 pattern: push PCM chunks as they arrive (`_StreamDecoder`, full-chunk schedule mid-stream + partial-tail flush), per-step partials → unstable, two-tick-stable word prefixes → committed (`_StreamEmitter`, 2-word right-context guard — this checkpoint emits no sentence punctuation on our corpus, so Decision 5's sentence boundaries don't fire), `att_context_size` dial effective; batch branch untouched (FR-008)
- [x] T020 [US2] End-of-audio finalize: resolve outstanding unstable per I5 (`_StreamEmitter.finish` commits the remainder; divergent-tail guard keeps the concatenation canonical per I2), terminal done 0.059 s after end-of-audio @ 30 s
- [x] T021 [US2] Validate nemotron emission against the invariant harness (`server/tests/testbed/test_nemotron_streaming.py` — offline policy tests + hardware session running the shared I1–I5 checkers) and record SC-004 watermarks in `results/streaming-watermarks.json` (finalize 0.059 s ≤ 1 s; TTFC ratio 1.0 ≤ 1.5×; streaming WER == batch, SC-003 ✓)
- [x] T022 [US2] Live-validate quickstart S2 on the NVIDIA PC: `myna-dictate --clip <30 s stream> --mode streaming --show-unstable` shows continuous `~` partials and `»` commits during playback, terminal `✓` at clip end (SC-006 nemotron half); 5 s clip likewise

**Checkpoint**: US2 functional — frame-once streaming demonstrated and measured (SC-006, nemotron half)

---

## Phase 5: User Story 3 — Parakeet-class small snap for CPU tiers (Priority: P3)

**Goal**: A strictly confined, int8-ONNX Parakeet snap serves progressive committed dictation on CPU-only hardware at ≤ 25 % of the full NeMo snap's size

**Independent Test**: `snap install --dangerous myna-parakeet_*.snap`, dictate a multi-sentence realtime clip through the confined socket — `»` segments arrive mid-utterance; installed size meets SC-005 (quickstart S3)

- [X] T023 [US3] Stage Parakeet TDT 0.6B v3 int8 ONNX weights via `dev/parakeet/fetch_parakeet_onnx.py`
- [X] T024 [US3] Port the greedy TDT decode loop to numpy/onnxruntime in `server/src/myna/testbed/parakeet.py`
- [X] T025 [US3] Implement Parakeet `run_session` with SilenceCut chunked commit, capabilities, ready gating, and off-format rejection
- [X] T026 [US3] Validate Parakeet against the invariant harness and real-corpus WER; record watermarks
- [X] T027 [US3] Create the CPU-only, strictly confined `parakeet-snap` (model component, `ws+unix` socket, idle-unload, no `hardware-observe`)
- [X] T028 [US3] Validate confined end-to-end streaming through `/var/snap/myna-parakeet/common/run/ubustt.sock`; make SilenceCut cadence configurable via `stream-arm-seconds`, `stream-silence-cut-seconds`, and `stream-force-cut-seconds`

**Checkpoint**: US3 functional — small confined transducer snap streaming on CPU

---

## Phase 6: User Story 4 — sherpa-onnx turnkey streaming snap + conclusion (Priority: P3)

**Goal**: A second small snap on the sherpa-onnx runtime provides native chunked streaming; its measurements complete the build-vs-adopt comparison

**Independent Test**: Confined sherpa snap: continuous `~` partials + endpoint-driven `»` commits on a realtime clip; size meets SC-005 (quickstart S3)

- [X] T029 [US4] ~~Export~~ **Fetch** a NeMo-family streaming transducer in sherpa-onnx format via `dev/fetch_sherpa_model.py` — k2-fsa's pre-exported `sherpa-onnx-nemo-streaming-fast-conformer-transducer-en-480ms-int8` made the export scripts unnecessary (80/1040 ms latency variants + Zipformer fallback fetch the same way)
- [X] T030 [US4] Implement the sherpa adapter in `server/src/myna/testbed/sherpa.py`: `OnlineRecognizer` push loop — partial results → unstable, endpoint-detected segments → committed; capabilities + ready gating + off-format rejection (FR-012). Note: sherpa-onnx's native lib needs onnxruntime 1.27.x's version node (pyproject pins `onnxruntime>=1.27,<1.28`; `dev/fetch_sherpa_model.py --fix-libs` symlinks the venv lib)
- [X] T031 [US4] Create the CPU-only, strictly confined `sherpa-snap` (model component, `ws+unix` socket, idle-unload, no `hardware-observe`)
- [~] T032 [US4] Validate sherpa emission (stub-recognizer tests and live bench complete; watermarks and installed size recorded); confined end-to-end validation remains

**Checkpoint**: US4 functional — both small snaps measured side by side

---

## Phase 7: Polish & Cross-Cutting Concerns

**Purpose**: Conclude the streaming investigation

- [ ] T033 [P] Write the concluding report `docs/interop/streaming-conclusion.md` (SC-007): accuracy / latency profile / footprint / tier coverage for whisper×3 strategies, nemotron native, parakeet, sherpa; recommended backend per hardware tier; tail-mutation commit-guarantee caveat (research.md Decision 4)
- [ ] T034 [P] Update `CLAUDE.md` current-state (FR-008 gap closed, two new snaps) and `docs/project-plan.md` (streaming workstream status)
- [ ] T035 Ratify emission watermark baselines + per-metric tolerances in `results/streaming-watermarks.json` (constitution Principle III)
- [ ] T036 Run the full `quickstart.md` validation sweep (S0–S5) and record outcomes in the report appendix

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: no dependencies — T002/T003/T004 parallel
- **Foundational (Phase 2)**: depends on Setup; **T005 (Spike S1) blocks US1's default-strategy choice (T013)**; T006/T007 block T011–T014; T008 blocks all validation tasks
- **US1 (Phase 3)**: depends on Foundational — MVP
- **US2 (Phase 4)**: depends on Foundational only (T008 harness); T018 (Spike S2, GPU) gates T019 — independent of US1, can run in parallel given NVIDIA PC access
- **US3 (Phase 5)**: depends on US1's fixed-head strategy (T012) and window machinery — shared code, not a story dependency for testability
- **US4 (Phase 6)**: depends on Foundational (T008); independent of US1–US3
- **Polish (Phase 7)**: depends on all four stories

### Within Each Story

- Strategies before adapter loop (T011–T013 → T014); CLI plumbing after loop (T015)
- Spikes before their gated implementation (T005 → T013; T018 → T019)
- Packaging after adapter validation (T026 → T027; T030 → T031)
- Watermarks after implementation, live validation last per story

### Parallel Opportunities

- T002, T003, T004 (Setup) — different files
- T009, T010 (US1 tests) — different files
- T011, T012 (US1 strategies) — same file as T013, so T011+T012 parallel only if T013 waits
- US1 (CPU machine) ∥ US2 (NVIDIA PC) — different machines, different files
- T023 (model staging) ∥ T024 (decode port) — download while porting
- T033, T034 (Polish docs) — different files

---

## Parallel Example: User Story 1

```bash
# After Foundational, launch US1 tests together:
Task: "Strategy commit-rule unit tests in server/tests/testbed/test_streaming_strategies.py"
Task: "Batch-degenerate regression test in server/tests/testbed/test_whisper_streaming.py"

# Then strategies (T011, T012 parallel; T013 after S1 outcome confirmed):
Task: "tail-mutation strategy in server/src/myna/testbed/streaming/strategies.py"
Task: "fixed-head strategy in server/src/myna/testbed/streaming/strategies.py"
```

---

## Implementation Strategy

### Branch Staging Plan (REQUIRED - constitution "Staged Delivery in Feature Branches")

| # | Branch | Scope (phases/stories) | Prerequisite branches | Merge gates |
|---|--------|------------------------|-----------------------|-------------|
| 1 | `008-streaming-foundation` | Phase 1–2 (incl. Spike S1) | — | strategy/invariant suites green; S1 findings recorded |
| 2 | `008-whisper-strategies` | Phase 3 (US1) | #1 | invariant suite + SC-001/002/003 watermarks; quickstart S1 passes |
| 3 | `008-nemotron-native` | Phase 4 (US2) | #1 | S2 findings recorded; SC-004 watermarks; quickstart S2 passes (GPU) |
| 4 | `008-parakeet-snap` | Phase 5 (US3) | #2 | confined e2e; SC-005 size gate; SC-003 WER |
| 5 | `008-sherpa-snap-report` | Phase 6–7 (US4 + Polish) | #2 | confined e2e; SC-005; concluding report landed |

Branches #3–#5 do not build on unmerged siblings beyond #1/#2 as tabled; every merge leaves the default branch green.

### MVP First (User Story 1 Only)

1. Complete Phase 1: Setup
2. Complete Phase 2: Foundational (Spike S1 decides the default strategy)
3. Complete Phase 3: User Story 1
4. **STOP and VALIDATE**: quickstart S1 — the 2026-07-27 failing manual test passes for whisper
5. This alone closes the FR-008 gap for the default backend; demo-ready

### Incremental Delivery

1. Setup + Foundational → machinery + S1 decision
2. US1 → whisper streams (MVP) → validate independently
3. US2 → nemotron native streaming → validate independently (GPU)
4. US3 → Parakeet small snap → validate confined
5. US4 → sherpa snap → concluding report closes the investigation

---

## Notes

- [P] tasks = different files, no dependencies
- [Story] label maps task to user story for traceability (US1–US4 per the post-clarify spec)
- Spikes are timeboxed gates with pre-decided fallbacks — record findings even on no-go
- Python harness tier: tests included where load-bearing (T008–T010), not red-green mandatory; Rust untouched
- Privacy check per task: no audio persistence; unstable text display-only, never logged by default
- Commit after each task or logical group; stop at any checkpoint to validate the story independently
