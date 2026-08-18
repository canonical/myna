# Data Model: Audio8-ASR Backend

**Date**: 2026-08-17
**Feature**: `specs/010-audio8-asr-backend`

The session wire contract, event model, and capabilities schema are unchanged
from `myna.core` — this feature introduces no new wire entities (same posture
as feature 009; no `contracts/` directory). Below are the feature-local
entities.

## Audio8 model bundle (staged, gitignored)

The on-disk artifact set staged by `dev/fetch_audio8_model.py` from the
ONNX release repo, consumed by the adapter and packed into the snap component.

| Field | Type | Notes |
|---|---|---|
| `model_bundle/metadata.json` | JSON | graph metadata, `max_audio_seconds` (30), token names, `response_prefix` |
| `model_bundle/audio_hidden_int8.onnx` | ONNX graph | audio tower (encoder + MLP projector front half) |
| `model_bundle/lm_cache_prefill_{int8,int4}.onnx(.data)` | ONNX graphs | decoder prefill, per precision |
| `model_bundle/lm_cache_decode_{int8,int4}.onnx(.data)` | ONNX graphs | decoder step, per precision |
| `model_bundle/weights/token_embedding.npy` | numpy | prompt embedding lookup (all precisions) |
| `model_bundle/weights/audio_projector.npz` | numpy | projector norm/linear weights |
| `model_bundle/{tokenizer.json,vocab.json,merges.txt}` | tokenizer | Qwen BPE tokenizer files |
| `asr_onnx_runtime.py`, `hotword/` | Python | staged engine source (Decision 2) |

**Validation rules**:
- Bundle completeness checked at load: missing/corrupt file → fail fast with a
  clear error (spec edge case; never download at session time).
- Precision auto-selected from staged variants: `int8` default; `int4` only
  when requested and present; `fp32` graphs never required.
- License acknowledgment recorded by the fetch script before staging
  (`--accept-license`, FR-014).

## Adapter configuration (constructor-time)

Mirrors the existing adapters' constructor-flag pattern (no wire changes).

| Field | Type | Default | Notes |
|---|---|---|---|
| `model_dir` | path | staged snapshot | `AUDIO8_MODEL_DIR` env override |
| `language` | enum | `auto` | `auto` or one of `en zh fr de ja ko yue`; pinned path via prompt seam (Decision 4, spike-gated) |
| `cache_precision` | enum | `int8` | `int8` / `int4` |
| `audio_precision` | enum | `int8` | `int8` |
| `max_new_tokens` | int | `256` | generation bound (FR-008) |
| `max_audio_seconds` | int | 30 | from bundle metadata; chunk size for chunk-and-stitch (FR-009 amended) |
| `silence_threshold` | float | spike-tuned | RMS gate (Decision 7) |

## Capabilities document (existing schema, new values)

| Field | Value |
|---|---|
| `languages` | `["auto", "en", "zh", "fr", "de", "ja", "ko", "yue"]` (pinning subject to Decision 4 spike) |
| `formats` | 16 kHz mono s16le PCM (audio-push invariant; no resampling) |
| `punctuation` | `true` (measured — results/spike-audio8-posture.md; FR-007) |
| `streaming` | `false` (batch/commit only, FR-004) |

## Benchmark run record (existing schema)

Per-clip JSONL, identical shape to existing backend runs (see
`results/bench-funasr-real.jsonl`). Distinguishing fields:

| Field | Value |
|---|---|
| `label` | `audio8/cpu` or `audio8/nvidia-gpu` |
| `served_models` | `["audio8-asr-0.1b"]` |
| `streaming_strategy` | `batch` |

## Comparison report

Aggregated cross-backend table (accuracy WER/CER, commit latency, RTF) over
shared corpus clips, produced by `dev/aggregate.py` from the recorded
baselines plus the new Audio8 runs; checked into `results/`.

## State transitions

Backend lifecycle (unchanged contract): `preparing` (model load + warm-up
inference, Decision 9) → `ready` → `transcribing` → `ready`. Snap service
lifecycle (unchanged): idle-unload via the existing model-control mechanism.
