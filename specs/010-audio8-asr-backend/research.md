# Research: Audio8-ASR Backend

**Date**: 2026-08-17
**Feature**: `specs/010-audio8-asr-backend`

All publisher surfaces verified against the live Hugging Face repos
(`Audio8/Audio8-ASR-0.1B` base checkpoint; `AutoArk-AI/Audio8-ASR-0.1B-onnx-runtime`
— redirects to `Audio8/Audio8-ASR-0.1B-onnx-runtime`). The ONNX runtime source
(`asr_onnx_runtime.py`, 735 lines) was read in full.

## Decision 1: Runtime — the publisher's ONNX cache engine, not Transformers+torch

**Decision**: Build the adapter on the ONNX Runtime release's
`OnnxCacheAsrEngine` (decoder `int8` + audio tower `int8` by default; `int4`
decoder and `fp32` reference variants included upstream). No torch, no
`trust_remote_code`.

**Rationale**:
- The base checkpoint requires `trust_remote_code=True` plus torch — a multi-GB
  dependency and, per the spec Assumptions, implicit remote-code loading is a
  blocking concern for a shipped snap. The ONNX release is self-contained:
  "Everything needed for CPU ONNX inference is in `model_bundle/`."
- The engine API is exactly our shape:
  `OnnxCacheAsrEngine("model_bundle", cache_precision="int8", audio_precision="int8").transcribe(wav_bytes, language=None, max_new_tokens=128, hotwords=None)`
  — bytes in, text out, no file paths, no network.
- Runtime imports are light: `numpy`, `onnxruntime`, `psutil`, `tokenizers`,
  and `transformers` (only `WhisperFeatureExtractor`, which is numpy-based —
  no torch is pulled in). Hotword trie is bundled but unused by us.
- Publisher-documented peak memory ≈ 1.1 GB; int4 decoder variant lowers it
  further — both fit the small-model watermark class.
- Cache-engine prefill/decode graphs avoid the full-context fallback graph
  (`lm_logits.onnx`, 414 MB) entirely.

**Alternatives considered**:
- Transformers + torch base checkpoint: rejected — torch dependency weight,
  `trust_remote_code` unacceptable in a shipped artifact, and no quantized CPU
  path. Remains the reference for behavior cross-checks only.
- Vendoring/reimplementing the decode loop over the raw ONNX graphs: rejected
  for v1 — the publisher's engine is maintained alongside the graphs;
  reimplementation risks subtle prompt/embedding drift. Revisit only if the
  staged-file integration (Decision 2) proves unworkable.

## Decision 2: Runtime-as-data — stage the engine with the model, never commit it

**Decision**: `dev/fetch_audio8_model.py` stages BOTH the `model_bundle/`
artifacts and the runtime files (`asr_onnx_runtime.py`, `hotword/`) from the
ONNX release repo into a gitignored directory (HF cache layout, mirroring
`dev/fetch_funasr_model.py`). The adapter loads the engine from the staged
directory via importlib (qwen adapter's `QWEN_ASR_LIB` pattern:
`AUDIO8_MODEL_DIR` env override → default staged snapshot). Nothing
CC-BY-NC-licensed enters the git tree.

**Rationale**:
- Myna is GPLv3. Vendoring CC-BY-NC-4.0 code/weights into the repo would
  contaminate the tree for commercial redistributors; staging at fetch/build
  time keeps the license boundary at the artifact level, where the 2026-08-17
  clarification placed responsibility (the integrator's).
- The runtime is not on PyPI; it is versioned with the graphs in the same HF
  repo, so fetching them together guarantees graph/engine compatibility.
- Snap components are built from the staged directory at component-build time —
  identical to the whisper-snap/funasr-snap model-component flow.

**Alternatives considered**:
- Vendoring `asr_onnx_runtime.py` into `server/src/`: rejected — GPLv3 tree
  contamination (above).
- `pip install` from a git URL: rejected — no packaged distribution exists,
  and it would blur the license acknowledgment flow (FR-014).

## Decision 3: License handling — explicit acknowledgment flag, tooling informs

**Decision**: The fetch script refuses to download until passed
`--accept-license "CC-BY-NC-4.0"` (or an interactive affirmative), printing the
license summary and integrator-responsibility notice. The snap's component
build script passes the flag explicitly. Per clarification: tooling surfaces,
never gates.

**Rationale**: FR-014 verbatim; matches the clarification that compliance is
the integrator's responsibility.

## Decision 4: Language control — `_build_prompt` seam, spike-verified

**Decision**: Default operation is `auto` (the upstream prompt is a fixed
"Please transcribe this audio." — the ONNX runtime's `language` parameter is
explicitly ignored: `def _build_prompt(...): del language`). For pinned
languages, subclass `OnnxCacheAsrEngine` and override `_build_prompt` to
inject a language instruction (e.g., "Please transcribe this audio in
Chinese.") — a single, clean seam requiring no fork. Whether the model
reliably obeys prompt-based pinning is UNDOCUMENTED upstream → verified by a
spike task before the capability is advertised (see tasks.md spike).

**Fallback**: if the spike shows pinning is unreliable, the adapter advertises
`auto` only, FR-006 is amended via a spec clarification, and the language set
is advertised as recognition-supported (not pinnable). The comparison (US2) is
unaffected — corpora evaluation runs under `auto` regardless.

**Measured follow-up (2026-08-17, results/spike-audio8-language.md)** — the
fallback branch FIRED. The prompt seam is mechanically correct (prompts
provably differ), yet pinned-en and pinned-zh produce byte-identical output on
both English and Chinese clips: the model auto-detects from the audio encoder
and ignores the "in X" instruction entirely. Adapter now advertises
`("auto",)` only; the seam is removed as dead code; FR-006 requires a spec
amendment (recognition stays 7-wide under `auto`).

**Rationale**:
- Auto-detection is the model's documented behavior across all 7 languages;
  both evaluation corpora (en, zh) exercise it directly.
- The `_build_prompt` override is 5 lines and touches no upstream code —
  preferable to forking the engine or re-implementing prompt assembly.

## Decision 5: Output sanitization — upstream normalizer + defense-in-depth sweep

**Decision**: Emit upstream's `normalize_prediction_text()` output, then apply
our own residual sweep: the `<|...|>` regex from the funasr adapter plus a
leading `language X` prefix strip. Covered by unit tests (FR-005, SC-006).

**Rationale**: The upstream normalizer already handles `<|text|>`/`<asr_text>`
splits, a `language [A-Za-z]+` prefix (the model demonstrably emits these —
the regex exists because the behavior exists), special tokens, and whitespace.
Defense-in-depth costs nothing and keeps SC-006 (zero residual artifacts
across the corpus) robust against upstream normalizer changes.

## Decision 6: unbounded audio — chunk-and-stitch, never truncate or reject

**Decision (amended 2026-08-17)**: The model's audio tower is fixed-length
(`max_audio_seconds` ≈ 30 s — the ONNX graph pads/truncates to
`frames_padded`, and the engine's `_extract_features` slices to
`max_samples`). The adapter must never let audio reach that silent-truncate
path: audio up to the cap decodes in one pass, and longer audio is split
into ≤ cap chunks and the transcripts stitched into one committed final
(FR-009). Audio is unbounded, matching the other adapters (whisper chunks
internally; funasr/sherpa feed the full buffer).

**Amended from** the original "reject at commit" decision: rejection was the
wrong posture — a user dictating past 30 s must not get an error. Chunk
boundaries may cut a word (no overlap/dedup without word timestamps); that
is an accepted v1 tradeoff, a smarter long-form strategy is future work.

## Decision 7: Silence handling — conservative RMS gate + corpus scan

**Decision**: Before decode, compute frame RMS energy; below a conservative
threshold (clearly-digital-silence), emit an empty transcript without invoking
the model. Threshold validated against the corpus `quiet` category so quiet
speech is never gated. The full corpus scan (SC-005) verifies zero
hallucinated multi-word outputs on non-speech clips.

**Rationale**: Generative decoders hallucinate on silence; there is no
model-side no-speech signal (unlike SenseVoice's `nospeech` tag, which is
emitted but post-hoc). An energy gate is deterministic, cheap, and testable.

**Alternatives considered**: decode-then-filter (rejected — the hallucinated
text has no reliable signature); VAD library (rejected — new dependency for a
10-line energy check at batch-commit granularity).

## Decision 8: Bounded generation — `max_new_tokens` cap, greedy decode

**Decision**: `max_new_tokens=256` adapter default (dictation utterances ≤ 30 s
map well under this; upstream example default is 128 for short clips).
Decode is greedy argmax in the engine's loop (no sampling knobs exist).
Repetition loops self-terminate at the cap and still emit a final (FR-008).

## Decision 9: Warm-up — synthetic noise pass during `preparing`

**Decision**: One 6 s low-amplitude Gaussian-noise inference (seeded,
funasr-adapter-identical parameters) at load, before reporting `ready`.
Absorbs ORT graph optimization and arena allocation (FR-010, SC-003). The
warm-up output is discarded through the same sanitization path.

## Decision 10: Precision/quantization — int8 default, int4 flag, fp32 excluded from snap

**Decision**: Adapter/engine default `cache_precision=int8,
audio_precision=int8` (upstream default). `int4` decoder selectable via
constructor flag / snap setting. The snap model component ships int8 + int4
decoder graphs, int8 audio tower, and shared weights (~870 MB) — fp32 graphs
(~2 GB: `audio_hidden.onnx` 880 MB, prefill/decode/logits 414 MB each) are
excluded as reference-only. The dev fetch script can stage fp32 optionally.

Bundle size budget (measured from the HF API):

| Staged for snap | Size |
|---|---|
| `audio_hidden_int8.onnx` | 234.7 MB |
| `lm_cache_prefill_int8.onnx(.data)` + `lm_cache_decode_int8.onnx(.data)` | 209.8 MB |
| `lm_cache_prefill_int4`/`lm_cache_decode_int4` (.data) | 111.0 MB |
| `weights/token_embedding.npy` + `audio_projector.npz` | 313.3 MB |
| tokenizer/vocab/merges/metadata | ~17 MB |
| **Snap component total** | **≈ 886 MB** |

## Decision 11: GPU engine — onnxruntime-gpu CUDA EP, whisper snap pattern

**Decision**: The `audio8-snap` gains `engines/cpu` and `engines/nvidia-gpu`
mirroring whisper-snap. The GPU engine installs `onnxruntime-gpu` in its
runtime part and selects the CUDA provider; startup fails fast if the provider
is unavailable (FR-020). Bench labels: `audio8/cpu`, `audio8/nvidia-gpu`.

**Rationale**: Upstream documents "GPU use requires installing a compatible
ONNX Runtime GPU package and selecting an available provider" — the graphs are
provider-agnostic, so the engine split is packaging-only, exactly the whisper
`faster-whisper-cpu`/`faster-whisper-cuda` precedent.

## Decision 12: Dependency alignment — onnxruntime 1.27.x project pin, not upstream's 1.22

**Decision**: The `audio8` extra in `server/pyproject.toml` uses the project's
existing `onnxruntime>=1.27,<1.28` pin (sherpa/parakeet VERS-node
compatibility), plus `tokenizers`, `transformers` (feature extractor only),
`numpy`, `psutil`. Upstream pins `onnxruntime==1.22.0` "for reproducible local
behavior" — a soft pin. Graph compatibility with 1.27.x is verified by the
load smoke task (same de-risk pattern as funasr T004).

**Measured follow-up (2026-08-17)** — VERIFIED: the cache engine loads and
decodes under onnxruntime 1.27.0 (dev smoke; full session `Copy that.`). No
torch in the resolve. Peak RSS 1.08 GB on the int8 path — matches the
publisher's ~1.1 GB claim (SC-004).

**Alternatives considered**: a second onnxruntime version in an isolated
venv/snap part — rejected; dual native libonnxruntime copies in one process
space is exactly the VERS-node hazard the project pin exists to avoid.

## Decision 13: Benchmark & comparison — existing pipeline, two corpora, per-engine labels

**Decision**: Run `dev/bench.py` against the Audio8 backend socket with labels
`audio8/cpu` (and `audio8/nvidia-gpu` where hardware exists) on `corpus/real`
(en) and `corpus/chinese` (zh); aggregate with `dev/aggregate.py` alongside
existing `results/bench-*.jsonl` baselines; check the comparison summary into
`results/` (FR-015/016, SC-002). No corpus or pipeline changes needed — both
corpora and the metrics path already exist from feature 009.

## Decision 14: Capabilities truthfulness — punctuation posture set by spike, not assumed

**Decision**: A spike task records actual output characteristics (punctuation,
capitalization, ITN behavior) on corpus clips before capabilities are
finalized (FR-007). Generative ASR typically punctuates; if confirmed, the
adapter advertises punctuation support truthfully — a first among the
small-model backends — otherwise `punctuation: false` (sherpa/funasr posture).

**Measured follow-up (2026-08-17, results/spike-audio8-posture.md)** —
`punctuation=True` confirmed: output is natively punctuated and capitalized
(en `Copy that.`, zh `这并不是告别。`); zero residual tags/prefixes across the
corpus clips; and the model emits empty output on silence AND loud noise (0
tokens) — no hallucination, so the RMS silence gate is defense-in-depth, not
a correctness requirement.

## Resolved spec tension

- **FR-006 (language pinning) vs runtime reality** (`language` ignored
  upstream): resolved by Decision 4's prompt-seam override + spike gate, with
  an explicit spec-amendment fallback. Flagged here so tasks.md sequences the
  spike BEFORE capabilities advertising is finalized.
