# Implementation Plan: FunASR / SenseVoice Backend (Adapter + Inference Snap)

**Branch**: `009-funasr-sensevoice-backend` | **Date**: 2026-07-31 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `specs/009-funasr-sensevoice-backend/spec.md`

## Summary

Add a FunASR/SenseVoice-Small inference backend: a `myna.testbed` adapter in
`myna-server` (`server/src/myna/testbed/funasr.py`, sibiling to
whisper/nemotron/qwen/sherpa), the runtime dependency as a `funasr` pip extra,
and a strictly-confined `funasr-snap/` inference snap shipping ONNX-exported
model weights as components. The surface is batch-mode only, both wire dialects,
with capabilities advertising the 6-language set and `punctuation: false`
(sherpa-compatible posture — post-processing deferred to a shared feature).
Clients, harness, and metrics all work unchanged.

## Technical Context

**Language/Version**: Python 3.12 for server/adapter/snap (evaluation harness
tier — TDD-exempt per constitution); Rust client expected unchanged.

**Primary Dependencies**: `funasr-onnx` 0.4.2 (PyPI, MIT — provides
`SenseVoiceSmall`); `onnxruntime` (CPU EP); `kaldi-native-fbank` (fbank feature
extraction); `sentencepiece` (tokenizer); `numpy`, `PyYAML`, `soundfile`,
`jieba`, `librosa`, `scipy` — all pip-installable, no torch/NeMo/CTranslate2 or
system deps. The existing `myna.core` session framework, `myna.testbed` adapter
protocol, and both wire-dialect codecs.

**Storage**: N/A (in-memory session state only; model weights staged as snap
components or cached via HF/ModelScope at component-build time).

**Testing**: `pytest` (Python — harness tier); `dev/bench.py` / `dev/matrix.py`
extended with FunASR candidate; Chinese reference corpus fetched by a new script
(`dev/fetch_chinese_corpus.py`); confined end-to-end against the `funasr` snap
(`myna-snap/` smoke-test pattern).

**Target Platform**: Ubuntu Desktop (current LTS+) with PipeWire; snapped
services. CPU-only; GPU acceleration out of scope.

**Project Type**: Inference service adapter + snap packaging (one new adapter
in the existing server, one new inference snap).

**Performance Goals** (from spec SCs): commit latency ≤ 2 s for ≤ 15 s
utterance after warm-up; CER on Chinese reference corpus within 1 pp of
published SenseVoice-Small (SC-001); WER on English real corpus no worse than
whisper-tiny baseline (SC-002); peak memory within small-model watermark
tolerance (SC-005).

**Constraints**: No network at runtime; no persisted audio; no transcription
logging by default; unpunctuated output (punctuation: false); audio-push
invariant (reject off-format, never resample); batch/committed-only
disposition; warm-up during `preparing` so first utterance latency excludes
ORT graph-optimization cost.

**Scale/Scope**: Single-user desktop dictation, one session at a time. One
new adapter. One new inference snap. One Chinese evaluation corpus.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Status | Notes |
|-----------|--------|-------|
| I. Red-Green TDD | ✅ Pass | All work is Python harness-tier (adapter, snap server) — exempt per constitution. Rust client is expected untouched; if the adapter surfaces a client gap, that change is TDD. |
| II. Integration-Test Readiness | ✅ Pass | Validation runs against live `myna-server` instances and the confined snap — same pattern as existing features. No audio-server dependency for adapter tests (pipe audio from files). |
| III. Performance Watermarks | ✅ Pass | New watermarks (commit latency, Chinese CER, peak memory) recorded via `dev/bench.py` / `dev/matrix.py` alongside existing `results/` artifacts; SC-001/002/003/SC-005 gates consume them. |
| IV. Workshop Dev Env | ✅ Pass | New Python deps are pip-installable via a `funasr` extra in `server/pyproject.toml` — no new system deps. Snap packaging follows the existing per-family snap pattern (whisper-snap/). |
| V. Privacy-First Offline | ✅ Pass | In-memory audio buffer cleared on session close; no audio persistence; tag-stripped transcripts never contain metadata tokens; offline models only (no network plug in the snap). |
| Staged Delivery | ✅ Plan | 2 increments matching user stories, each independently testable (see below). |
| Commit Communication | ✅ | No AI attribution. |

**No violations. No Complexity Tracking entries required.**

### Staged delivery

1. **US1 adapter** (P1): `funasr.py` adapter in `myna-server`, capabilities
   advertising, both wire dialects, warm-up lifecycle, batch-mode only,
   Chinese corpus evaluation → Chinese dictation usable from any client.
2. **US2 snap** (P2): `funasr-snap/` with ONNX model components, strictly
   confined, content-shared socket, idle-unload → shippable backend.

## Project Structure

### Documentation (this feature)

```text
specs/009-funasr-sensevoice-backend/
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
└── tasks.md             # Phase 2 output (/speckit-tasks — NOT created here)
```

### Source Code (repository root)

```text
server/
├── src/myna/testbed/
│   └── funasr.py                # NEW: SenseVoiceSmall adapter (SttService)
├── pyproject.toml               # gains [project.optional-dependencies] funasr
└── dev/
    └── fetch_funasr_model.py    # NEW: stage ONNX model from ModelScope/HF

funasr-snap/                     # NEW: inference snap (whisper-snap pattern)
├── snap/
│   └── snapcraft.yaml
├── components/
│   └── model-sensevoice-onnx/   # model weights (ONNX, downloaded pre-pack)
├── engines/
│   └── cpu/
├── runtimes/
│   └── funasr-onnx-cpu/
├── models/
├── scripts/
│   └── server.sh
└── wheels/

dev/
├── fetch_chinese_corpus.py      # NEW: Chinese reference corpus fetcher
└── benchmark_results/
    └── ...                      # gains FunASR candidate entries
corpus/chinese/                  # gitignored — fetched by dev/fetch_chinese_corpus.py

reference/MyVoiceTyping/         # already available (reference review)
```

**Structure Decision**: The adapter follows the exact same `SttService`
protocol as all other adapters — `funasr.py` joins `whisper.py`,
`nemotron.py`, `qwen.py`, `sherpa.py`, `parakeet.py` under `testbed/`. The
`funasr-snap/` directory mirrors `whisper-snap/` layout exactly. No new
directories or architectural patterns introduced. The vendored approach in
the reference app (`src/vendor/funasr_onnx/`) is rejected in favor of the
`funasr-onnx` PyPI package (research.md Decision 1) — no vendored code.

## Complexity Tracking

> No constitution violations — table intentionally empty.
