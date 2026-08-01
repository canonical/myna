# Tasks: FunASR / SenseVoice Backend (Adapter + Inference Snap)

**Input**: Design documents from `specs/009-funasr-sensevoice-backend/`

**Prerequisites**: plan.md (required), spec.md (required for user stories), research.md, data-model.md, quickstart.md

**Tests**: Per constitution §I, the Python adapter (`server/src/myna/testbed/funasr.py`) and snap packaging are evaluation-harness tier — exempt from TDD. Acceptance is through quickstart validation scenarios (T020–T022), not upstream test tasks. No test-fail-before-implement requirement applies here.

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2)
- Include exact file paths in descriptions

## Path Conventions

Paths follow the project structure from `plan.md`:
- Adapter: `server/src/myna/testbed/funasr.py`
- Server config: `server/src/myna/testbed/__main__.py`, `server/pyproject.toml`
- Snap: `funasr-snap/`
- Evaluation: `dev/bench.py`, `dev/matrix.py`, `dev/fetch_chinese_corpus.py`

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Model fetch tooling, dependency scaffolding, and Chinese corpus — everything that both US1 and US2 need before implementation begins.

- [x] T001 Add `funasr` extra to `server/pyproject.toml` with `funasr-onnx>=0.4.2` and its transitive deps: `onnxruntime`, `kaldi-native-fbank`, `sentencepiece`, `numpy`, `PyYAML`, `soundfile`, `jieba`, `librosa`, `scipy`. Verify `uv sync --extra funasr` installs cleanly.
- [x] T002 [P] Create `dev/fetch_funasr_model.py` — downloads SenseVoice-Small ONNX artifacts from ModelScope `iic/SenseVoiceSmall` (HF mirror `FunAudioLLM/SenseVoiceSmall` as fallback), staged to `$HF_HOME` cache or a configurable target directory. Includes `--quantize` flag to prefer `model_quant.onnx`. Output: `model.onnx` (or `model_quant.onnx`), `config.yaml`, `am.mvn`, `chn_jpn_yue_eng_ko_spectok.bpe.model`.
- [x] T003 [P] Create `dev/fetch_chinese_corpus.py` — downloads a curated subset of Mozilla Common Voice zh-CN v18.0 (`validated.tsv`, CC0). Filters to clips ≥ 5 s, selects up to 50, writes `corpus/chinese/manifest.csv` + `corpus/chinese/audio/*.wav`. Mirrors `dev/fetch_real_corpus.py` layout. Add `corpus/chinese/` to `.gitignore`.

**Checkpoint**: FunASR model and Chinese corpus are fetchable; `funasr` extra installs.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Verify the runtime works end-to-end on this machine before writing adapter boilerplate. Minimal stub to confirm the library imports and loads a model.

- [x] T004 Verify `funasr-onnx` imports and SenseVoice model loads: a one-shot script (or adapter `__main__` invocation) that calls `SenseVoiceSmall(model_dir)` with the fetched model, runs `model(numpy.zeros(16000, dtype=numpy.float32))`, prints the output, and confirms no import/load errors. **Throwaway smoke test** — the temporary `funasr.py` stub is replaced by T005; T004 exists only to de-risk the library import before investing in adapter boilerplate.

**Checkpoint**: Runtime confirmed. Adapter implementation begins.

---

## Phase 3: User Story 1 - Chinese dictation through the standard client (Priority: P1) 🎯 MVP

**Goal**: A FunASR/SenseVoice adapter in `myna-server` that serves batch dictation over both wire dialects, advertises capabilities, runs warm-up, strips model tags, and is evaluable on both English and Chinese corpora.

**Independent Test**: Start `myna-server --adapter funasr`, run `myna-dictate` against both wire dialects with English and Chinese clips, verify committed text is returned (unpunctuated, tag-free) and injected.

### Implementation for User Story 1

- [x] T005 [US1] Implement `FunasrAdapter` class in `server/src/myna/testbed/funasr.py`: constructor accepts `model_dir`, `language` (default `"auto"`), `textnorm` (default `"woitn"`), `num_threads` (default 4); properties `streaming` (always `False`), `candidate`, `capabilities`; `model_lock` for thread safety. Capabilities advertises `models=("sensevoice-small",)`, `languages=("auto","zh","en","yue","ja","ko")`, `punctuation=False`, `translation=False`, `input_formats` (16k s16le mono). Follows the `Adapter` protocol from `server/src/myna/testbed/adapter.py`.
- [x] T006 [US1] Implement `_load_model()` in `server/src/myna/testbed/funasr.py`: creates `SenseVoiceSmall(model_dir, device_id="-1", quantize=<auto-detect>, intra_op_num_threads=<N>)`. Auto-detects quantization: prefers `model_quant.onnx` if present, else `model.onnx`. Stores model on `self._model`. Guarded by `model_lock`; idempotent.
- [x] T007 [US1] Implement warm-up in `server/src/myna/testbed/funasr.py`: at load time, run one inference with 6 s synthetic low-amplitude Gaussian noise (`rng.standard_normal(int(16000*6.0))*50.0`, seed=0) before reporting `PHASE_READY`. Emits `TranscriptionProgress(phase=PHASE_PREPARING)` with heartbeat during load, then `TranscriptionProgress(phase=PHASE_READY)` after warm-up. Model loading happens via `asyncio.to_thread`; heartbeat interval matches existing adapters (2 s).
- [x] T008 [US1] Implement `run_session()` in `server/src/myna/testbed/funasr.py`: validates `config.audio_format` against `(16000 Hz, 1 ch, s16le)` — rejects with `TranscriptionError(code="unsupported_audio_format", ...)` on mismatch per audio-push invariant (FR-002). Accumulates PCM chunks into a `bytearray`; emits `TranscriptionProgress()` every 1 s. On end-of-audio, converts buffer to float32 ndarray and passes to decode.
- [x] T009 [US1] Implement decode + emit in `server/src/myna/testbed/funasr.py`: calls `self._model(wav, language=self._language, textnorm=self._textnorm)` via `asyncio.to_thread`. Strips control tags with `re.sub(r'<\|.*?\|>', '', text).strip()` (FR-005, SC-006). Emits `TranscriptionFinal(text=stripped, disposition=Disposition.COMMITTED)` then `TranscriptionDone(text=stripped)`. Handles empty/near-silent audio gracefully (empty string, not an error). Wraps all exceptions in `TranscriptionError(code="inference_failed", ...)`.
- [x] T010 [US1] Register adapter in `myna-server` CLI (`server/src/myna/server/cli.py`): add `--adapter funasr`; the model dir rides the standard `--model` flag (like sherpa — default: staged ModelScope cache snapshot), with funasr-specific flags `--funasr-language` (default `auto`) and `--funasr-textnorm` (default `woitn`). Adapter instantiation follows the same `if adapter_name == "funasr":` dispatch pattern as whisper/nemotron/qwen/sherpa.
- [x] T011 [US1] Implement `unload()` method in `server/src/myna/testbed/funasr.py`: releases the ORT session (`self._model = None`), calls `gc.collect()`. Guards with `model_lock`; idempotent. Follows the T27 idle-unload pattern used by sherpa and whisper adapters.

**Checkpoint**: `myna-server --adapter funasr` serves Chinese and English dictation over both wire dialects. Quickstart S1–S3 pass.

---

## Phase 4: User Story 2 - Strictly-confined FunASR inference snap (Priority: P2)

**Goal**: A `funasr` snap under strict confinement, shipping SenseVoice weights as a component, accessible to the confined `myna` client snap over the content-shared socket.

**Independent Test**: Install snap + model component, connect `myna` snap's socket plug, dictate with network disabled — transcript returned.

### Implementation for User Story 2

- [x] T012 [US2] Create `funasr-snap/` directory structure: `snap/snapcraft.yaml`, `components/model-sensevoice-onnx/`, `engines/cpu/`, `runtimes/funasr-onnx-cpu/`, `models/`, `scripts/server.sh`, `wheels/`, `dev/prepare.sh`. Mirror `whisper-snap/` layout exactly.
- [x] T013 [US2] Create `funasr-snap/dev/prepare.sh`: builds the myna wheel from `server/` (via `uv build`), copies the wheel + funasr-onnx wheels into `funasr-snap/wheels/`. Follows `whisper-snap/dev/prepare.sh` pattern.
- [x] T014 [US2] Create `funasr-snap/snap/snapcraft.yaml`: base `core24`, `confinement: strict`, `compression: lzo`. `apps.server` as daemon running `bin/server.sh`, plugs `hardware-observe`, `opengl`, `network-bind` (no `network`). `slots.ubustt-socket` content share (writable, source: `$SNAP_COMMON/run`) matching the whisper snap's slot definition. Parts: `cli` (modelctl binary), `server-app` (Python venv with `myna[funasr]` wheel from `wheels/`), `models/` (staged model artifacts), `scripts/server.sh`, `model-components` (SenseVoice ONNX files organized into component, `prime: [-*]` to exclude from base). Components: `model-sensevoice-onnx` (type: standard). Mirrors `whisper-snap/snap/snapcraft.yaml` structure — specifically the python part pattern and content-share slot.
- [x] T015 [US2] Create `funasr-snap/scripts/server.sh`: launches `myna-server --adapter funasr --socket $SNAP_COMMON/run/ubustt.sock --funasr-model-dir <model-component-path>`. Uses modelctl to resolve the active model component path (same `$SNAP_COMPONENTS` pattern as whisper snap's `server.sh`). Follows `whisper-snap/scripts/server.sh` pattern.
- [x] T016 [US2] Create `funasr-snap/engines/cpu/engine.yaml` and runtimes/funasr-onnx-cpu/runtime.yaml`: modelctl engine manifest declaring the CPU engine, and runtime manifest declaring the Python venv as the runtime. Follow the `whisper-snap/engines/cpu/` and `whisper-snap/runtimes/faster-whisper-cpu/` patterns exactly, substituting `funasr-onnx-cpu` for `faster-whisper-cpu`.
- [x] T017 [US2] Populate `funasr-snap/components/model-sensevoice-onnx/`: run `dev/fetch_funasr_model.py` with output directed to the component directory. Include `model.onnx` (or `model_quant.onnx`), `config.yaml`, `am.mvn`, `chn_jpn_yue_eng_ko_spectok.bpe.model`. Ensure the flat file layout matches what `SenseVoiceSmall(model_dir=...)` expects.

**Checkpoint**: `sudo snap install --dangerous funasr_*.snap funasr+model-sensevoice-onnx.comp` → confined dictation works. Quickstart S6–S7 pass.

---

## Phase 5: Polish & Cross-Cutting Concerns

**Purpose**: Evaluation integration, watermarks, validation.

- [x] T018 [P] Add FunASR candidate to `dev/bench.py`: register `funasr` adapter with the bench harness so `dev/bench.py --adapter funasr --corpus real` and `--corpus chinese` run. Follows the existing `if adapter_name == "funasr":` dispatch pattern.
- [x] T019 [P] Add FunASR candidate to `dev/matrix.py`: include sensevoice-small in the evaluation matrix CSV output. Follows existing adapter registration pattern.
- [x] T020 Run quickstart.md validation end-to-end: execute all 7 quickstart scenarios (S1–S7) and confirm they pass. Document any deviations.
- [x] T021 Record performance watermarks: run `dev/bench.py --adapter funasr --corpus real` and `--corpus chinese`, capture commit latency, peak memory, WER (English), CER (Chinese). Check against SC-001 (CER ≤ published + 1pp), SC-002 (WER ≤ whisper-tiny baseline), SC-003 (latency ≤ 2 s for ≤ 15 s utterance), SC-006 (zero residual tags). Record in `results/`.
- [x] T022 Confined end-to-end smoke test with snap: install `funasr` snap + model component, connect `myna` snap socket plug, run dictation on both corpora, verify network can be disabled, verify idle-unload/reload cycle. Confirm SC-005 (peak memory within small-model watermark tolerance).

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies — can start immediately.
- **Foundational (Phase 2)**: Depends on Setup (T001, T002 — model must be fetchable before load verification).
- **User Story 1 (Phase 3)**: Depends on Foundational (T004 verifies runtime; T005–T011 build on that).
- **User Story 2 (Phase 4)**: Depends on US1 (the adapter must exist and work before it can be snapped). Depends on Setup (T002 — model download script needed for component population in T017).
- **Polish (Phase 5)**: Depends on US1 (bench/matrix need the adapter). US2 validation (T022) depends on US2 completion.

### User Story Dependencies

- **User Story 1 (P1)**: Can start after Foundational (Phase 2). No dependencies on US2.
- **User Story 2 (P2)**: Depends on US1 completion — the snap packages the adapter implemented in US1.

### Within Each User Story

- T006 depends on T005 (skeleton before load)
- T007 depends on T006 (warm-up needs model loaded)
- T008 depends on T005 (session handler needs skeleton)
- T009 depends on T006, T008 (decode needs model + accumulated buffer)
- T010 depends on T005 (CLI needs adapter class)
- T011 is independent of T008/T009/T010 (can be written alongside)
- T013 depends on T001 (prepare.sh needs the funasr extra declared)
- T014–T016 can be written in any order within US2 (different files)
- T017 depends on T002 (model fetch script) and T014 (snapcraft yaml defines component layout)

### Parallel Opportunities

- T002 and T003 are independent (model fetch vs corpus fetch)
- T005 and T010 can proceed in parallel (adapter class + CLI registration are different concerns)
- T011 is parallel with T008/T009 (unload is independent of session logic)
- T012–T016 in US2 are all different files — T012 (directory), T013 (prepare.sh), T014 (snapcraft), T015 (server.sh), T016 (engine/runtime yamls) can be created in parallel
- T018 and T019 are parallel (bench vs matrix, different concerns)
- T020–T022 are sequential (must validate after all implementation)

---

## Parallel Example: User Story 2

```bash
# Create all snap scaffolding files in parallel (different files):
Task: T012 "Create funasr-snap/ directory structure"
Task: T013 "Create funasr-snap/dev/prepare.sh"
Task: T014 "Create funasr-snap/snap/snapcraft.yaml"
Task: T015 "Create funasr-snap/scripts/server.sh"
Task: T016 "Create funasr-snap/engines/cpu/engine.yaml and runtimes/funasr-onnx-cpu/runtime.yaml"

# Then populate model component (depends on T002 + T014):
Task: T017 "Populate funasr-snap/components/model-sensevoice-onnx/"
```

---

## Implementation Strategy

### Branch Staging Plan (constitution "Staged Delivery in Feature Branches")

| # | Branch | Scope (phases/stories) | Prerequisite branches | Merge gates |
|---|--------|------------------------|-----------------------|-------------|
| 1 | `009-funasr-sensevoice-backend` | Phase 1–3 (setup + foundational + US1 adapter) | — | hermetic suite + quickstart S1–S5 |
| 2 | `009-funasr-sensevoice-backend` | Phase 4 (US2 snap) | #1 | hermetic + quickstart S6–S7 + confined smoke |

(Note: Both increments land on the same feature branch per plan.md's 2-increment layout. The first merge delivers the working adapter; the second adds the snap wrapper.)

### MVP First (User Story 1 Only)

1. Complete Phase 1: Setup (T001–T003)
2. Complete Phase 2: Foundational (T004)
3. Complete Phase 3: User Story 1 (T005–T011)
4. **STOP and VALIDATE**: Run quickstart S1–S5, bench on both corpora
5. Chinese dictation is usable from any myna client — deploy/demo

### Incremental Delivery

1. Setup + Foundational → model and corpus fetchable, runtime verified
2. Add US1 adapter → Chinese dictation works bare-metal (MVP!)
3. Add US2 snap → confined, shippable backend
4. Polish → watermarks recorded, quickstart fully validated

### Single-Developer Strategy

All tasks are sequential within this branch — no parallel team needed. A single developer can execute T001–T022 in order, with the parallel opportunities noted for time-batching where applicable (e.g., create all snap files together, bench+matrix together).

---

## Notes

- [P] tasks = different files, no dependencies
- [Story] label maps task to specific user story for traceability
- No TDD test tasks: the adapter is harness-tier Python (constitution §I exemption). Acceptance is through quickstart validation (T020–T022).
- The `funasr-onnx` PyPI package is the runtime — no vendored code from `reference/MyVoiceTyping/` (research.md Decision 1).
- Model artifacts are staged at component-build time, never downloaded at runtime (offline invariant).
- Warm-up with synthetic 6 s noise before `ready` — exact parameters from the reference app (research.md Decision 8).
- Tag stripping regex `<\|.*?\|>` is sufficient for all known SenseVoice control tokens (research.md Decision 6).
