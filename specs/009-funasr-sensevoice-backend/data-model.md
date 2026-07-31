# Data Model: FunASR / SenseVoice Backend

**Feature**: `specs/009-funasr-sensevoice-backend`

The wire-visible event shapes are unchanged from the existing session contract
(`myna.core.events`, `myna.core.wire_ie115`). This document covers only the
server-side entities introduced by the FunASR adapter.

## FunasrModelBundle

The on-disk artifact set for SenseVoice recognition. Staged in the snap
component (or `HF_HOME` cache for testbed use) as a flat directory.

| Field | Type | Notes |
|---|---|---|
| `model_dir` | `Path` | Directory containing the four files below |
| `onnx_path` | `Path` | Auto-detected: prefers `model_quant.onnx` (int8) if present; falls back to `model.onnx` (fp32) — matches T006 |
| `config_yaml` | `Path` | `config.yaml` — frontend config (fbank params, LFR, CMVN path) |
| `am_mvn` | `Path` | `am.mvn` — global mean/variance normalization statistics |
| `bpe_model` | `Path` | `chn_jpn_yue_eng_ko_spectok.bpe.model` — SentencePiece tokenizer |
| `is_quantized` | `bool` | Derived: True if `model_quant.onnx` is present and loaded |

**Validation rules**:
- `model_dir` MUST exist and contain at minimum `config.yaml`, `am.mvn`, the
  `.bpe.model` file, and at least one `.onnx` file.
- If neither `model.onnx` nor `model_quant.onnx` is present, model loading
  fails with a clear error.

**Lifecycle**: Created at adapter construction (loaded from disk into ORT
session); released on `unload()`.

## FunasrAdapterState (per adapter instance, in-memory)

| Field | Type | Notes |
|---|---|---|
| `model` | `SenseVoiceSmall` | ORT inference session — constructed once, reused across sessions |
| `language` | `"auto"` \| `"zh"` \| `"en"` \| `"yue"` \| `"ja"` \| `"ko"` | Default `auto`; server-flag override |
| `textnorm` | `"woitn"` \| `"withitn"` | Default `woitn`; constructor flag (FR-007) |
| `model_lock` | `asyncio.Lock` | Guards model load/unload (idempotent, thread-safe) |
| `num_threads` | `int` | ORT intra-op threads (default 4 per SenseVoice defaults) |
| `streaming` | `bool` | Always `False` for this feature (batch-only) |

**State transitions**:

```text
CONSTRUCTED → (load) → LOADED → (warm-up) → READY → (session_N) → READY → … → (unload) → CONSTRUCTED
```

- `CONSTRUCTED`: model not yet loaded; `model_lock` available.
- `LOADED`: ORT session created (first-load cost paid), warm-up pending.
- `READY`: warm-up complete; the `preparing`→`ready` lifecycle has been
  emitted; sessions can be served.
- `unload()` returns to `CONSTRUCTED` (idle-unload, T27 pattern).

## FunasrSessionState (per active session, in-memory only)

| Field | Type | Notes |
|---|---|---|
| `buffered_audio` | `bytearray` | Accumulated S16LE PCM until end-of-audio |
| `seconds_since_progress` | `float` | Drives periodic `TranscriptionProgress` liveness ticks |
| `format` | `AudioFormat` | Validated against `SHERPA_FORMAT` equivalent (16k s16le mono) — reject on mismatch (FR-002) |

**State transitions**:

```text
session start → buffering → (end-of-audio) → decoding → final + done → session end
```

- `buffering`: accumulate PCM chunks; emit `TranscriptionProgress` on interval.
- `decoding`: `SenseVoiceSmall(waveform)` → text; strip tags (FR-005,
  Decision 6); emit `TranscriptionFinal(disposition=COMMITTED)`.
- `final + done`: emit `TranscriptionDone`; clear buffer.

## Capabilities (advertised per adapter instance)

| Field | Value | Notes |
|---|---|---|
| `models` | `("sensevoice-small",)` | Single model — no size variants in this feature |
| `languages` | `("auto", "zh", "en", "yue", "ja", "ko")` | Full model-supported set (FR-006) |
| `input_formats` | `(AudioFormat(sample_rate_hz=16000, channels=1, sample_width_bytes=2),)` | Standard format — audio-push invariant |
| `punctuation` | `False` | Unpunctuated output (FR-008, sherpa-compatible) |
| `translation` | `False` | Not a translation model |
| `streaming` | `False` | Batch-only (FR-004); may become `True` in a future streaming feature |

## Candidate

The adapter's `candidate` property exposes the evaluation-matrix entry:

| Field | Value |
|---|---|
| `model` | `"sensevoice-small"` |
| `engine` | `"funasr-onnx-cpu"` |
| `streaming_strategy` | `"commit-on-finalize"` |

## ChineseReferenceCorpus (evaluation artifact, gitignored)

| Field | Type | Notes |
|---|---|---|
| `manifest` | `Path` | `corpus/chinese/manifest.csv` — `clip_id,audio_path,duration_s,reference_text` |
| `audio_dir` | `Path` | `corpus/chinese/audio/` — WAV files, 16k mono S16LE |
| `source` | `str` | `"common-voice-zh-CN-v18.0"` — provenance tracked |
| `clip_count` | `int` | Target ~50; actual count per download |

Fetched by `dev/fetch_chinese_corpus.py`; regenerated on demand; never
committed. Mirrors `corpus/real/` layout and `dev/fetch_real_corpus.py`
pattern.
