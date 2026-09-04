# Tasks: Audio8-ASR Backend (Adapter + Benchmark Comparison + Inference Snap)

**Input**: Design documents from `specs/010-audio8-asr-backend/`

**Prerequisites**: plan.md (required), spec.md (required for user stories), research.md, data-model.md, quickstart.md

**Tests**: Per constitution §I, the Python adapter (`server/src/myna/testbed/audio8.py`), fetch script, and snap packaging are evaluation-harness tier — exempt from TDD *ordering*. However, FR-017/SC-007 explicitly require a unit suite and coverage parity, so test tasks ARE included (T013) — written alongside the adapter, not strictly before. Acceptance is through quickstart validation scenarios (T025–T026) plus the unit/coverage gates.

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3)
- Include exact file paths in descriptions

## Path Conventions

Paths follow the project structure from `plan.md`:
- Adapter: `server/src/myna/testbed/audio8.py`
- Server config: `server/src/myna/server/cli.py`, `server/pyproject.toml`
- Tests: `server/tests/test_audio8_unit.py`
- Snap: `audio8-snap/`
- Evaluation: `dev/bench.py`, `dev/aggregate.py`, `dev/adapter_coverage.py`, `dev/fetch_audio8_model.py`

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Dependency scaffolding and the license-acknowledging fetch tooling that every story needs before implementation begins.

- [x] T001 Add `audio8` extra to `server/pyproject.toml`: `onnxruntime>=1.27,<1.28` (project pin, research.md Decision 12 — NOT upstream's soft 1.22 pin), `tokenizers>=0.22`, `transformers>=4.57` (WhisperFeatureExtractor only — no torch), `numpy`, `psutil`. Verify `uv sync --extra audio8` installs cleanly and does NOT pull torch into the resolve.
- [x] T002 [P] Create `dev/fetch_audio8_model.py` — stages the model bundle AND engine source from HF `Audio8/Audio8-ASR-0.1B-onnx-runtime` into a gitignored directory (HF cache layout, mirroring `dev/fetch_funasr_model.py`; research.md Decision 2). Refuses to download without `--accept-license "CC-BY-NC-4.0"` (prints license summary + integrator-responsibility notice; FR-014). `--profile` flag: `dev` (int8 decoder + int8 audio tower + shared weights + `asr_onnx_runtime.py` + `hotword/`, default), `snap` (dev + int4 decoder graphs), `full` (everything incl. fp32 reference graphs). Resumable via `hf download`; `AUDIO8_MODEL_DIR` env override documented in `--help`. Verify the stage dir is gitignored if staged inside the repo.

**Checkpoint**: `audio8` extra installs without torch; bundle + engine fetchable with explicit license acknowledgment.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: De-risk the runtime and resolve the two spike gates BEFORE adapter capabilities are written. Mirroring 009's throwaway-smoke pattern.

- [x] T003 Verify the staged runtime loads and decodes: one-shot throwaway script that importlib-loads `asr_onnx_runtime.py` from the staged dir, constructs `OnnxCacheAsrEngine(model_dir, cache_precision="int8", audio_precision="int8")`, and transcribes 1 s of zero audio wrapped as WAV bytes. Confirms (a) `onnxruntime` 1.27.x loads the publisher graphs (Decision 12), (b) no torch/`trust_remote_code` anywhere in the import chain, (c) the engine's `transcribe()` returns the documented result dict (`text`, `raw`, `elapsed_seconds`). Throwaway — replaced by T007; exists only to de-risk before adapter investment.
- [x] T004 [P] Spike S1 — language pinning seam (research.md Decision 4): subclass `OnnxCacheAsrEngine`, override `_build_prompt` to emit `Please transcribe this audio in <Language>.` when pinned (upstream prompt verbatim for `auto`). Run 5 Chinese (`corpus/chinese`) + 5 English (`corpus/english`) clips under `auto` and pinned-correct and pinned-WRONG languages; compare CER/WER. **Gate**: pinning reliable (pinned-correct ≈ auto, pinned-wrong measurably worse) → advertise full pin set; else → advertise `auto`-only and flag FR-006 for spec amendment. Record outcome in `results/spike-audio8-language.md` (precedent: `results/spike-s1-word-timestamps.md`).
- [x] T005 [P] Spike S2 — output posture + silence threshold (research.md Decisions 7, 14): transcribe 10 `corpus/english` clips; record punctuation presence, capitalization, ITN behavior, residual `<|...|>` tokens, and `language X` prefix frequency (upstream `normalize_prediction_text` exists because this happens). Determine the capabilities `punctuation` value empirically (FR-007). Tune the silence RMS gate: verify all `quiet`-category clips pass the gate while synthetic digital silence and near-silence are caught; check what the model emits on ungated silence (hallucination signature). Record in `results/spike-audio8-posture.md`.

**Checkpoint**: Runtime confirmed on the project's onnxruntime pin; language-pinning and punctuation posture resolved with data; silence threshold tuned. Adapter implementation begins.

---

## Phase 3: User Story 1 - Audio8 dictation through the standard testbed (Priority: P1) 🎯 MVP

**Goal**: An Audio8 adapter in `myna-server` that serves batch dictation over both wire dialects, advertises truthful capabilities, runs warm-up, sanitizes generative output, gates silence, handles unbounded audio via chunk-and-stitch (FR-009 amended), and carries a weights-free unit suite at coverage parity.

**Independent Test**: Start `myna-server --adapter audio8`, run `myna-dictate` against both wire dialects with English and Chinese clips, verify committed text is returned (artifact-free), and run `pytest tests/test_audio8_unit.py` without model weights.

### Implementation for User Story 1

- [x] T006 [US1] Implement `Audio8Adapter` class in `server/src/myna/testbed/audio8.py`: constructor accepts `model_dir` (default: staged snapshot), `language` (default `"auto"`), `cache_precision` (default `"int8"`), `audio_precision` (default `"int8"`), `max_new_tokens` (default `256`, FR-008), `num_threads` (default 4); properties `streaming` (always `False`), `candidate`, `capabilities`; `model_lock`. Capabilities advertises `models=("audio8-asr-0.1b",)`, `languages` per T004 spike outcome, `punctuation` per T005 spike outcome (FR-007), `input_formats` (16k s16le mono). The staged engine import MUST be lazy (inside `_load_model`) so the unit suite runs weights-free (SC-007). Follows the `Adapter` protocol from `server/src/myna/testbed/adapter.py`.
- [x] T007 [US1] Implement `_load_model()` in `server/src/myna/testbed/audio8.py`: importlib-loads `asr_onnx_runtime.py` from `model_dir` (env `AUDIO8_MODEL_DIR` override, qwen adapter's `QWEN_ASR_LIB` pattern; research.md Decision 2); defines the `_PinnedPromptEngine(OnnxCacheAsrEngine)` subclass overriding `_build_prompt` exactly as spike-verified in T004 (`auto` = upstream prompt verbatim); instantiates with the configured precisions (Decision 10). Guarded by `model_lock`; idempotent; missing/corrupt bundle → fail fast with clear error, never download (FR-013).
- [x] T008 [US1] Implement warm-up in `server/src/myna/testbed/audio8.py`: at load time, one inference with 6 s synthetic low-amplitude Gaussian noise (`rng.standard_normal(int(16000*6.0))*50.0`, seed=0 — funasr-identical parameters, research.md Decision 9) before reporting `PHASE_READY`; warm-up output discarded through the same sanitization path. Emits `TranscriptionProgress(phase=PHASE_PREPARING)` heartbeats (2 s) during load, `PHASE_READY` after warm-up; loading via `asyncio.to_thread`.
- [x] T009 [US1] Implement `run_session()` in `server/src/myna/testbed/audio8.py`: validates `config.audio_format` against `(16000 Hz, 1 ch, s16le)` — `TranscriptionError(code="unsupported_audio_format")` on mismatch (FR-002); accumulates PCM into `bytearray`; `TranscriptionProgress()` every 1 s. Audio is unbounded: `_decode` splits > `max_audio_seconds` (30) into per-chunk decodes and stitches (FR-009 amended — upstream `_extract_features` silently truncates, so the adapter chunks BEFORE the engine sees over-cap audio; never reject, never truncate).
- [x] T010 [US1] Implement decode + emit in `server/src/myna/testbed/audio8.py`: wraps buffered PCM as in-memory WAV bytes; silence RMS gate (threshold from T005, Decision 7) → empty transcript without decode; else `engine.transcribe(wav_bytes, language=None, max_new_tokens=self._max_new_tokens, hotwords=None)` via `asyncio.to_thread`. Sanitization (Decision 5, FR-005): upstream `normalize_prediction_text`, then `<\|.*?\|>` sweep, then leading `language X` prefix strip. Emits `TranscriptionFinal(text, disposition=Disposition.COMMITTED)` then `TranscriptionDone` (FR-004). Empty/near-silent → empty string, not an error. Exceptions → `TranscriptionError(code="inference_failed")`. Unknown model request → `model_not_available` (FR-012/013).
- [x] T011 [US1] Register adapter in `server/src/myna/server/cli.py`: add `--adapter audio8`; model dir rides `--model` (sherpa/funasr pattern, default staged snapshot); flags `--audio8-language` (default `auto`), `--audio8-precision` (default `int8`, choices `int8`/`int4`), `--audio8-max-new-tokens` (default 256). Lazy import dispatch matching existing adapters.
- [x] T012 [P] [US1] Implement `unload()` in `server/src/myna/testbed/audio8.py`: releases ORT sessions (`self._engine = None`), `gc.collect()`; guarded by `model_lock`; idempotent; sherpa/whisper idle-unload pattern.
- [x] T013 [P] [US1] Create `server/tests/test_audio8_unit.py` (weights-free, FR-017/SC-007 — mock/monkeypatch the staged engine import): sanitization sweep (special tokens, `<|text|>` splits, `language X` prefixes, whitespace collapse); prompt seam (`_build_prompt` override only when pinned, upstream prompt for `auto`); audio-format validation rejects 8 kHz/stereo; long audio chunk-and-stitch (FR-009 amended); silence gate passes/blocks at the T005 threshold; `max_new_tokens` passthrough + clamp; unknown model → `model_not_available`; `unload()` idempotent. Then wire `audio8` into `dev/adapter_coverage.py` (`ADAPTERS_DEFAULT`) and confirm merged coverage of `audio8.py` ≥ the funasr adapter floor (SC-007).

**Checkpoint**: `myna-server --adapter audio8` serves English and Chinese dictation over both wire dialects; unit suite green weights-free; coverage parity achieved. Quickstart S1–S5 pass.

---

## Phase 4: User Story 2 - Benchmark comparison against existing backends (Priority: P2)

**Goal**: Audio8 benchmarked through the existing pipeline on both corpora, aggregated against every recorded backend baseline into a checked-in comparison report — the adopt/drop decision artifact.

**Independent Test**: Run `dev/bench.py` against the Audio8 backend with the `audio8/cpu` label on both corpora, aggregate with existing baselines, and verify the report ranks all backends on identical clips with 100% of clips accounted for.

### Implementation for User Story 2

- [x] T014 [US2] English benchmark run: `dev/bench.py --socket <audio8 socket> --label audio8/cpu` on `corpus/english`, output `results/bench-audio8-real.jsonl` (mirrors `results/bench-funasr-real.jsonl`). Verify record shape matches existing runs (`label`, `served_models=["audio8-asr-0.1b"]`, `streaming_strategy="batch"`) and 100% of clips scored or explicitly accounted (SC-002).
- [x] T015 [P] [US2] Chinese benchmark run: `dev/bench.py --socket <audio8 socket> --label audio8/cpu` on `corpus/chinese`, output `results/bench-audio8-chinese.jsonl` (mirrors `results/bench-funasr-chinese.jsonl`). CER through the existing metrics pipeline.
- [x] T016 [US2] Produce the comparison report (FR-016, SC-002): `dev/aggregate.py --by-category` over all `results/bench-*.jsonl` baselines (whisper, sherpa, parakeet, funasr, nemotron, qwen where present) plus the new Audio8 runs; per-backend WER (en) / CER (zh), commit latency, RTF table; check in as `results/audio8-comparison.md` with a verdict-ready summary (accuracy vs whisper-tiny baseline, vs funasr on Chinese, latency class). Include the automated scans: zero residual special tokens/tags across all Audio8 transcripts (SC-006) and zero hallucinated multi-word outputs on non-speech clips (SC-005).
- [x] T017 [US2] Record performance watermarks: capture commit latency (SC-003: ≤ 2 s for ≤ 15 s utterance after `ready`; first utterance not measurably slower), RTF, and peak session memory (SC-004: within small-model watermark tolerance; publisher documents ≈ 1.1 GB int8) into `results/` alongside the whisper-tiny baselines. Re-run on GPU hardware with `--label audio8/nvidia-gpu` if available (FR-018) — results under the distinct label, never conflated.

**Checkpoint**: `results/audio8-comparison.md` exists and ranks Audio8 against all baselines on identical clips. Quickstart S6 passes.

---

## Phase 5: User Story 3 - Strictly-confined Audio8 inference snap (Priority: P3)

**Goal**: An `audio8` snap under strict confinement shipping the ONNX bundle as a component, with cpu and nvidia-gpu engines, accessible to the confined `myna` client snap over the content-shared socket.

**Independent Test**: Install snap + model component, connect `myna` snap's socket plug, dictate with network disabled — transcript returned; on GPU hardware, `use-engine nvidia-gpu` serves via CUDA, and selecting it without a GPU fails fast.

### Implementation for User Story 3

- [x] T018 [US3] Create `audio8-snap/` directory structure: `snap/snapcraft.yaml`, `components/model-audio8-onnx/`, `engines/cpu/`, `engines/nvidia-gpu/`, `runtimes/audio8-onnx-cpu/`, `runtimes/audio8-onnx-cuda/`, `models/`, `scripts/server.sh`, `wheels/`, `dev/prepare.sh`. Mirror `whisper-snap/` layout exactly.
- [x] T019 [US3] Create `audio8-snap/dev/prepare.sh`: builds the myna wheel from `server/` (`uv build`), copies the wheel + `audio8`-extra wheels (onnxruntime CPU build, tokenizers, transformers, numpy, psutil) into `audio8-snap/wheels/`. Follows `whisper-snap/dev/prepare.sh` pattern.
- [x] T020 [US3] Create `audio8-snap/snap/snapcraft.yaml`: base `core24`, `confinement: strict`, `compression: lzo`. `apps.server` daemon running `bin/server.sh`; plugs `hardware-observe`, `opengl`, `network-bind` (NO `network` — offline invariant); `slots.ubustt-socket` content share (writable, source `$SNAP_COMMON/run`) matching whisper-snap. Parts per the whisper-snap python pattern: `server-app` (venv with `myna[audio8]` wheel), `cli` (modelctl), `scripts/`, `model-components` (`prime: [-*]`). Components: `model-audio8-onnx` (type: standard).
- [x] T021 [P] [US3] Create `audio8-snap/scripts/server.sh`: launches `myna-server --adapter audio8 --socket $SNAP_COMMON/run/ubustt.sock --model <model-component-path>`; resolves the active component via modelctl (`$SNAP_COMPONENTS` pattern from `whisper-snap/scripts/server.sh`).
- [x] T022 [P] [US3] Create `audio8-snap/engines/cpu/engine.yaml` + `audio8-snap/runtimes/audio8-onnx-cpu/runtime.yaml`: modelctl engine/runtime manifests declaring the CPU engine on the Python venv runtime. Follow `whisper-snap/engines/cpu/` and `whisper-snap/runtimes/faster-whisper-cpu/` exactly.
- [x] T023 [P] [US3] Create `audio8-snap/engines/nvidia-gpu/engine.yaml` + `audio8-snap/runtimes/audio8-onnx-cuda/runtime.yaml`: GPU engine installing `onnxruntime-gpu` wheels and selecting the CUDA execution provider (research.md Decision 11); engine startup MUST fail fast with a clear error when the CUDA provider is unavailable (FR-020) — verify by running the engine selection on non-GPU hardware and asserting the error, never silent CPU fallback.
- [ ] T024 [US3] Populate `audio8-snap/components/model-audio8-onnx/`: run `dev/fetch_audio8_model.py --profile snap --accept-license "CC-BY-NC-4.0"` into the component directory (int8+int4 decoder graphs, int8 audio tower, shared weights, engine source; fp32 graphs EXCLUDED — Decision 10, ≈ 886 MB). Include the staged `asr_onnx_runtime.py` + `hotword/` so the snap's adapter loads the engine from the component (Decision 2). Component ships the LICENSE file from the upstream repo for integrator visibility.

**Checkpoint**: `sudo snap install --dangerous myna-audio8_*.snap myna-audio8+model-audio8-onnx.comp` → confined dictation works offline; engine switching works. Quickstart S7 passes.

---

## Phase 6: Polish & Cross-Cutting Concerns

**Purpose**: End-to-end validation and success-criteria verification.

- [ ] T025 Run quickstart.md validation end-to-end: execute all 7 scenarios (S1–S7) and confirm they pass; document any deviations in the spec's checklist notes.
- [ ] T026 Confined end-to-end smoke + SC verification: install the `audio8` snap + model component, connect the `myna` snap socket plug, dictate with networking disabled (SC-008); verify idle-unload/reload cycle and peak memory within the small-model watermark tolerance; verify GPU-engine fail-fast on non-GPU hardware (FR-020) and, where GPU hardware exists, a full GPU-engine session under the `audio8/nvidia-gpu` label (SC-002).

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies — start immediately. T002 is required by everything downstream (bundle + engine source).
- **Foundational (Phase 2)**: Depends on T001 (deps) and T002 (staged bundle). T003 must precede T007; spikes T004/T005 must precede T006 (capabilities values) and T010 (silence threshold).
- **US1 (Phase 3)**: Depends on Foundational. T006–T010 sequential core; T011/T012/T013 parallelizable against it.
- **US2 (Phase 4)**: Depends on US1 (the adapter is the instrument). T014/T015 parallel; T016/T017 depend on the runs.
- **US3 (Phase 5)**: Depends on US1 (snap packages the adapter) and T002 (snap-profile fetch in T024). Independent of US2 — snap can proceed before benchmarks finish.
- **Polish (Phase 6)**: T025 after US1+US2; T026 after US3.

### User Story Dependencies

- **US1 (P1)**: After Foundational. No story dependencies.
- **US2 (P2)**: Requires US1.
- **US3 (P3)**: Requires US1 (+ T002 snap profile). NOT blocked by US2 — but per spec intent, the comparison (US2) informs whether the snap earns its place; sequence US2 first in practice.

### Within User Story 1

- T007 depends on T003 (runtime verified) and T006 (skeleton)
- T008 depends on T007 (warm-up needs the engine)
- T009 depends on T006 (session handler needs skeleton)
- T010 depends on T005 (threshold), T007, T009
- T011 depends on T006; T012 independent; T013 depends on T006–T010 (tests behavior)

### Parallel Opportunities

- T001 ∥ T002 (deps vs fetch script — different files)
- T004 ∥ T005 (both spikes, different corpora/questions)
- T011 ∥ T012 ∥ T013 once the adapter core exists
- T014 ∥ T015 (en vs zh runs — sequential per socket in practice, but independent)
- T021 ∥ T022 ∥ T023 (snap scripts/engines — different files)
- US3 scaffolding (T018–T023) ∥ US2 runs (T014–T017)

---

## Parallel Example: User Story 3

```bash
# Snap scaffolding files in parallel (different files):
Task: T019 "Create audio8-snap/dev/prepare.sh"
Task: T020 "Create audio8-snap/snap/snapcraft.yaml"
Task: T021 "Create audio8-snap/scripts/server.sh"
Task: T022 "Create cpu engine + runtime manifests"
Task: T023 "Create nvidia-gpu engine + runtime manifests"

# Then populate the component (depends on T002 + T020):
Task: T024 "Populate audio8-snap/components/model-audio8-onnx/"
```

---

## Implementation Strategy

### Branch Staging Plan (constitution "Staged Delivery in Feature Branches")

| # | Branch | Scope (phases/stories) | Prerequisite branches | Merge gates |
|---|--------|------------------------|-----------------------|-------------|
| 1 | `010-audio8-asr-backend` | Phase 1–3 (setup + foundational + US1 adapter) | — | hermetic suite + quickstart S1–S5 |
| 2 | `010-audio8-asr-backend` | Phase 4 (US2 comparison) | #1 | `results/audio8-comparison.md` checked in + SC-002/005/006 scans clean |
| 3 | `010-audio8-asr-backend` | Phase 5–6 (US3 snap + polish) | #1 (#2 recommended) | hermetic + quickstart S6–S7 + confined smoke |

### MVP First (User Story 1 Only)

1. Complete Phase 1: Setup (T001–T002)
2. Complete Phase 2: Foundational (T003–T005 — spikes resolved)
3. Complete Phase 3: US1 (T006–T013)
4. **STOP and VALIDATE**: quickstart S1–S5, unit suite, coverage parity
5. Audio8 dictation usable from any myna client; US2 comparison can start

### Incremental Delivery

1. Setup + Foundational → runtime de-risked, spike gates resolved with data
2. US1 adapter → working Audio8 backend (MVP)
3. US2 comparison → adopt/drop decision artifact
4. US3 snap → shippable backend (sequenced after US2 per spec intent)
5. Polish → watermarks recorded, quickstart fully validated

### Single-Developer Strategy

Execute T001→T026 in order, batching the marked parallel opportunities (spikes together; snap scaffolding together; both corpus runs back-to-back). The spikes (T004/T005) are the highest-risk items — do not skip them to "save time"; T006's capabilities depend on their outcomes.

---

## Notes

- [P] tasks = different files, no dependencies
- [Story] label maps task to specific user story for traceability
- Unit tests (T013) are required by FR-017 despite the harness-tier TDD exemption; they are weights-free by design (lazy engine import, T006)
- The staged engine is NEVER committed to the git tree (GPLv3 vs CC-BY-NC boundary, research.md Decision 2) — adapter loads it via `AUDIO8_MODEL_DIR`
- The upstream `language` parameter is a no-op (`del language` in source); pinning goes through the `_build_prompt` seam only, spike-gated (T004)
- FR-009 (amended): upstream `_extract_features` silently truncates > 30 s — the adapter chunks audio into ≤ 30 s pieces and stitches, so long audio is unbounded (never reject, never truncate)
- Keep the project's `onnxruntime>=1.27,<1.28` pin (sherpa/parakeet VERS-node hazard); upstream's 1.22 pin is soft (Decision 12), verified by T003
