# Implementation Plan: Audio8-ASR Backend (Adapter + Benchmark Comparison + Inference Snap)

**Branch**: `010-audio8-asr-backend` | **Date**: 2026-08-17 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `specs/010-audio8-asr-backend/spec.md`

## Summary

Add Audio8-ASR-0.1B as a first-class myna backend: a `myna.testbed` adapter
(`server/src/myna/testbed/audio8.py`, sibling to whisper/nemotron/qwen/sherpa/
funasr) built on the publisher's self-contained ONNX cache engine (int8
default, no torch, no `trust_remote_code`), staged — runtime source and model
bundle together — by a license-acknowledging fetch script (runtime-as-data;
nothing CC-BY-NC enters the GPLv3 tree). Then benchmark it through the
existing pipeline on the English and Chinese corpora against all recorded
backend baselines, and ship a strictly-confined `audio8-snap/` with cpu and
nvidia-gpu engines (whisper-snap pattern). Batch/commit only — the model has
no native streaming mode.

## Technical Context

**Language/Version**: Python 3.12 for server/adapter/snap (evaluation harness
tier — TDD-exempt per constitution); Rust client expected unchanged.

**Primary Dependencies**: publisher's ONNX runtime (`asr_onnx_runtime.py`,
staged — research.md Decision 2); `onnxruntime>=1.27,<1.28` (project pin,
Decision 12; `onnxruntime-gpu` in the snap's nvidia-gpu engine); `tokenizers`;
`transformers` (numpy-based `WhisperFeatureExtractor` only — no torch);
`numpy`, `psutil`. The existing `myna.core` session framework, `myna.testbed`
adapter protocol, and both wire-dialect codecs.

**Storage**: N/A (in-memory session state only; model bundle staged under the
HF cache for dev, shipped as a snap component for the snap — ≈ 886 MB int8+int4
profile, fp32 graphs excluded, Decision 10).

**Testing**: `pytest` (`server/tests/test_audio8_unit.py`, weights-free);
`dev/adapter_coverage.py` merged-coverage run; `dev/bench.py` /
`dev/aggregate.py` on `corpus/english` + `corpus/chinese`; confined end-to-end
against the `audio8` snap (whisper-snap smoke pattern). Two spike gates
(language pinning, Decision 4; output posture, Decision 14).

**Target Platform**: Ubuntu Desktop (current LTS+), snapped services; CPU
baseline, NVIDIA GPU where applicable.

**Project Type**: Inference service adapter + benchmark evaluation + snap
packaging (one new adapter, one new inference snap, no new corpora).

**Performance Goals** (from spec SCs): commit latency ≤ 2 s for ≤ 15 s
utterance after warm-up (SC-003); peak memory within small-model watermark
tolerance (SC-004/008; publisher documents ≈ 1.1 GB for the int8 path);
accuracy measured and ranked against all recorded baselines (SC-002).

**Constraints**: No network at runtime; no persisted audio; no transcription
logging by default; audio-push invariant (reject off-format, never resample);
unbounded audio via chunk-and-stitch (FR-009 amended — never truncate, never
reject long audio); batch/committed disposition only; bounded generation (`max_new_tokens=256`); warm-up during
`preparing`; CC-BY-NC-4.0 material staged/acknowledged, never committed to the
git tree.

**Scale/Scope**: Single-user desktop dictation, one session at a time. One new
adapter, one fetch script, one snap, two benchmark corpora runs, one
comparison report.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Status | Notes |
|-----------|--------|-------|
| I. Red-Green TDD | ✅ Pass | All work is Python harness-tier (adapter, fetch script, snap server) — exempt per constitution. Rust client expected untouched; any client gap surfaced is TDD. Unit suite + coverage parity are required by FR-017/SC-007 despite the TDD exemption. |
| II. Integration-Test Readiness | ✅ Pass | Validation runs against live `myna-server` instances and the confined snap; adapter tests pipe audio from files — no audio-server dependency. Unit suite runs weights-free in CI (SC-007). |
| III. Performance Watermarks | ✅ Pass | Commit latency, RTF, and peak memory recorded via `dev/bench.py` alongside existing `results/` artifacts; SC-003/004/008 gates consume them; per-engine labels keep baselines honest. |
| IV. Workshop Dev Env | ✅ Pass | New deps are pip-installable via an `audio8` extra in `server/pyproject.toml`; no new system deps. Snap packaging follows the whisper-snap per-family pattern. |
| V. Privacy-First Offline | ✅ Pass | In-memory audio only; no network plug; bundle staged in advance (fetch script), never downloaded at session time; CC-BY-NC material kept out of the git tree. |
| Staged Delivery | ✅ Plan | 3 increments matching user stories, each independently testable (below). |
| Commit Communication | ✅ | No AI attribution. |

**No violations. No Complexity Tracking entries required.**

### Staged delivery

1. **US1 adapter** (P1): fetch script + `audio8.py` adapter — capabilities,
   both dialects, warm-up lifecycle, batch mode, silence gate, chunk-and-
   stitch for long audio, unit suite + coverage wiring → Audio8 dictation usable from any client.
2. **US2 comparison** (P2): benchmark runs on en/zh corpora per engine label +
   aggregated cross-backend comparison report checked into `results/` → the
   adopt/drop decision artifact.
3. **US3 snap** (P3): `audio8-snap/` with model component, cpu + nvidia-gpu
   engines, content-shared socket, idle-unload → shippable backend.

## Project Structure

### Documentation (this feature)

```text
specs/010-audio8-asr-backend/
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
└── tasks.md             # Phase 2 output (/speckit-tasks — NOT created here)
```

No `contracts/`: the session wire contract, event model, and capabilities
schema are unchanged (same posture as feature 009).

### Source Code (repository root)

```text
server/
├── src/myna/testbed/
│   └── audio8.py                # NEW: Audio8 adapter (SttService), subclassing
│                                #     the staged OnnxCacheAsrEngine (prompt seam)
├── src/myna/server/cli.py       # gains --adapter audio8 + audio8 flags
├── pyproject.toml               # gains [project.optional-dependencies] audio8
└── tests/
    └── test_audio8_unit.py      # NEW: weights-free unit suite (FR-017)

dev/
├── fetch_audio8_model.py        # NEW: license-acknowledging bundle+engine fetch
├── adapter_coverage.py          # gains audio8 in the default adapter set
└── bench.py / aggregate.py      # unchanged — reused via labels

audio8-snap/                     # NEW: inference snap (whisper-snap pattern)
├── snap/snapcraft.yaml
├── components/model-audio8-onnx/    # int8+int4 graphs + shared weights (≈886 MB)
├── engines/{cpu,nvidia-gpu}/
├── runtimes/{audio8-onnx-cpu,audio8-onnx-cuda}/
├── scripts/server.sh
└── wheels/

results/                         # gains audio8/* bench JSONL + comparison report
```

**Structure Decision**: The adapter follows the exact `SttService` protocol of
all other adapters and joins them under `testbed/`; `audio8-snap/` mirrors
`whisper-snap/` layout (components/engines/runtimes). The staged engine source
lives outside the git tree (HF cache / snap component), loaded via
`AUDIO8_MODEL_DIR` with the qwen adapter's env-override pattern. No new
architectural patterns; the one novel mechanism — subclassing
`OnnxCacheAsrEngine._build_prompt` for language pinning — is spike-gated
(research.md Decision 4).

## Complexity Tracking

> No constitution violations — table intentionally empty.
