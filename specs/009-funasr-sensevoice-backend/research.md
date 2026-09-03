# Research: FunASR / SenseVoice Backend

**Date**: 2026-07-31
**Feature**: `specs/009-funasr-sensevoice-backend`

All API surfaces verified locally on this machine. The reference review of
MyVoiceTyping (`reference/MyVoiceTyping/`) established the thin-runtime pattern
(ONNX Runtime + kaldi-native-fbank + sentencepiece, no torch/NeMo).

## Decision 1: Use PyPI `funasr-onnx`, not vendored code from the reference

**Decision**: Depend on `funasr-onnx` 0.4.2 from PyPI (MIT-licensed).
Provides `SenseVoiceSmall` (batch CTC decoder) and `CT_Transformer`
(punctuation — for the future post-processing feature).

**Rationale**:
- The reference app vendored `src/vendor/funasr_onnx/` because it bundles
  everything via PyInstaller for macOS .app distribution. We compile a snap,
  where standard pip dependencies are the normal path (matching whisper-snap's
  wheel installation of faster-whisper).
- `funasr-onnx` is maintained by Alibaba DAMO Academy (the same people
  upstream of SenseVoice); it receives ONNX export updates and runtime fixes.
  Vendoring would cut us off from upstream fixes.
- The PyPI package includes transitive deps `kaldi-native-fbank`,
  `sentencepiece`, `onnxruntime`, `jieba`, `librosa`, `scipy` — all wheeled
  for Linux arm64/amd64, no system libraries needed.
- The `SttService` adapter protocol (`run_session` signature) accepts a
  constructor-time library reference like we do with faster-whisper and
  sherpa-onnx — no import-time coupling.

**Alternatives considered**:
- Vendoring `reference/MyVoiceTyping/src/vendor/funasr_onnx/`: rejected —
  the PyPI package is the same code, upstream-maintained, and pip-friendly.
- Using FunASR's `AutoModel` (torch path): rejected — torch is a multi-GB
  dependency; the ONNX path is the whole point of this feature.

## Decision 2: Model staging — ModelScope download at component-build time, HF cache as offline mirror

**Decision**: `dev/fetch_funasr_model.py` downloads the ONNX model bundle from
ModelScope (`iic/SenseVoiceSmall`) into the snap component directory at build
time (same model `botaruibo/SenseVoiceSmall-onnx` uses — it is the official
`iic/SenseVoiceSmall` export republished). The Hugging Face mirror
(`FunAudioLLM/SenseVoiceSmall`) serves as the offline/local-cache fallback
(mirroring `dev/fetch_sherpa_model.py`). No runtime download — no `network`
plug.

Model artifacts (≈ 937 MB fp32, ≈ 234 MB int8 quant):

| File | Purpose |
|------|---------|
| `model.onnx` (or `model_quant.onnx`) | CTC graph weights |
| `config.yaml` | Frontend config (fbank: 80 mel bins, 25 ms frame, 10 ms shift, LFR 7→6→1) |
| `am.mvn` | Global CMVN statistics (mean/variance normalization) |
| `chn_jpn_yue_eng_ko_spectok.bpe.model` | SentencePiece tokenizer (5-language vocab) |

**Rationale**:
- The reference app downloads from ModelScope at first use (with GUI progress);
  we download at snap component-build time, consistent with our existing snap
  pattern (`dev/download-models.sh` for whisper-snap).
- ModelScope is the primary distribution channel for SenseVoice (it's
  Alibaba's model); the HF mirror is the offline-capable fallback.
- Downloads are resumable (`hf download --resume`); size is comparable to
  whisper-base CT2 (≈ 280 MB) + small (≈ 960 MB) — well within snap component
  size expectations.

**Alternatives considered**:
- Hugging Face-only: rejected — `iic/SenseVoiceSmall` on HF is a mirror, not
  the authoritative source, and may lag ModelScope updates.
- Runtime download with `snapshot_download`: rejected — violates offline
  invariant and adds `network` plug.

## Decision 3: Batch-only (commit-on-finalize), one disposed segment per utterance

**Decision**: The adapter collects all audio frames into a buffer, decodes
once at end-of-audio via `SenseVoiceSmall(waveform)` (numpy ndarray input),
emits a single `TranscriptionFinal(disposition=COMMITTED)` followed by
`TranscriptionDone`. No re-decode loop, no strategies, no `segment_index`
sequencing beyond the single final.

**Rationale**:
- SenseVoice-Small is a non-autoregressive CTC encoder — single forward pass
  over the full utterance yields the greedy hypothesis. It has no incremental
  emission path (unlike a streaming transducer). Spec FR-004 explicitly
  defers streaming.
- This is identical to the sherpa batch path (`SherpaAdapter._decode_oneshot`)
  and the whisper adapter's non-streaming path — no new patterns.
- The `commit-on-finalize` strategy is the simplest correct implementation
  and aligns with the `streaming_strategy` candidate label (matching all other
  batch-mode adapters).

**Alternatives considered**:
- Rolling re-decode (LocalAgreement-style, as in whisper streaming): deferred
  to a future feature — SenseVoice doesn't natively emit word timestamps from
  the ONNX path (the graph outputs CTC logits, not attention weights), so
  word-level timestamp stability is untested for this model family.
- FunASR streaming Paraformer ONNX export: deferred — a different model
  family, different export artifacts, different evaluation, and the feature
  explicitly targets SenseVoice-Small for its Chinese accuracy.

## Decision 4: Language control — `auto` default, explicit pin via session-level hint

**Decision**: The `language` tag to `SenseVoiceSmall.__call__` is either
`"auto"` (model-side detection — the ONNX graph's `lid` head) or an explicit
string from `{"zh", "en", "yue", "ja", "ko"}`. Expose language selection as a
constructor argument defaulting to `"auto"`; the session's `SessionConfig`
field is the natural carrier if/when the wire contract gains a language hint
(out of scope here). For now, a server flag selects the adapter's operating
language.

The language set matches SenseVoice's published `lid_dict`:
```python
{"auto": 0, "zh": 3, "en": 4, "yue": 7, "ja": 11, "ko": 12, "nospeech": 13}
```

**Rationale**:
- The ONNX graph receives language as an integer tag tensor alongside audio
  features; auto-detection uses the built-in LID head.
- A pinned language avoids auto-detection ambiguity on short utterances (an
  explicitly documented edge case in the spec).
- The wire contract currently has no language-selection field — we follow the
  existing pattern of constructor-level configuration (like whisper's
  `model_size`, nemotron's `device`), with capabilities advertising the
  supported set.

**Alternatives considered**:
- Drop explicit `"ko"` and `"ja"` (not tested in evaluation, no Chinese corpus
  relevance): rejected — the model supports them at zero marginal cost, so
  capabilities should advertise the truth. We evaluate only zh/en because those
  are the corpora we have, but we don't artificially restrict the model.

## Decision 5: Inverse text normalization — `woitn` as default, `withitn` as optional flag

**Decision**: Run SenseVoice with `textnorm="woitn"` by default — this is the
model's "without ITN" mode that produces readable text with digits/currency/etc
rendered as spoken words. The `withitn` mode (ITN: "twenty twenty-five" →
"2025") is exposed as an optional adapter constructor flag. Default to
`woitn` so committed transcripts are ready-to-type in the user's application.

**Rationale**:
- The reference app uses `woitn` (the MyVoiceTyping `transcribe` call uses
  `textnorm="woitn"` per the code review). ITN is a decode-time choice — the
  model can produce either.
- Dictation users expect "twenty twenty five" in a document, not "2025" unless
  they're dictating dates to a form. `woitn` is the conservative dictation
  default.
- Per the spec (FR-007), the option must exist; it lives as a constructor flag
  like other adapter configuration.

**Measured follow-up (2026-07-31, FLEURS cmn_hans_cn test, 25 clips ≥ 5 s)**:
`woitn` micro CER 13.21 % vs `withitn` 13.81 % — a wash. `withitn` recovers
digit-heavy clips (32.4 % → 5.4 % on the best case) but conditions the decoder
into different (sometimes worse) text elsewhere. Default confirmed. **Also
discovered: `withitn` makes SenseVoice natively emit punctuation (，。、) —
the model can punctuate itself, which means the future post-processing
feature may not need a separate CT-Transformer stage for SenseVoice (sherpa
still needs one). Recorded for the post-processing spec.**

**Alternatives considered**:
- `withitn` as default: rejected — the reference app's experience shows users
  prefer readable text; ITN mistakes (e.g., converting "one two three" to
  "123") cause more friction than no ITN.

## Decision 6: Tag stripping — regex post-pass matching the reference app

**Decision**: Strip all SenseVoice rich-transcription tags from output with a
regex matching the `<|...|>` pattern before emitting any wire event. The
regex is identical to the reference app's `rich_transcription_postprocess`:

```python
import re
text = re.sub(r'<\|.*?\|>', '', text).strip()
```

This removes language markers (`<|zh|>`, `<|en|>`, …), emotion tags
(`<|HAPPY|>`, `<|SAD|>`, …), audio-event markers (`<|APPLAUSE|>`,
`<|nospeech|>`), and ITN instruction tokens.

**Rationale**:
- These tags are control signals, not dictation output. SC-006 requires zero
  residual tags across the evaluation corpus.
- The reference app's approach is battle-tested on the same model; adopting it
  verbatim eliminates an error surface.
- No false-positive risk on normal dictation text (pipe-delimited tokens with
  angle brackets are not a natural dictation pattern).

**Alternatives considered**:
- Filtering specific known tags: rejected — the model may add tags in future
  exports; a general `<|...|>` regex is forward-compatible, and the
  false-positive risk on dictation text is negligible.

## Decision 7: Chinese evaluation corpus — Common Voice zh-CN subset

**Decision**: `dev/fetch_chinese_corpus.py` downloads a curated subset of
Mozilla Common Voice zh-CN v18.0 (CC0 license — no attribution or
registration barriers). Script filters to `validated.tsv` entries at least
5 s, selects up to 50 clips, and writes the same corpus layout as
`corpus/real/` (manifest.csv, audio/*.wav directories, reference text).

**Rationale**:
- AISHELL-1 requires registration and the license terms are ambiguous for
  redistribution; Common Voice zh-CN is CC0, zero-barrier fetch-and-go.
- 50 clips × avg 8 s ≈ 400 s of Chinese speech is adequate for CER baseline
  measurement (SC-001: CER within 1 pp of published SenseVoice benchmarks).
  Our real corpus for English is 100 clips — the Chinese corpus is the
  same scale class.
- The script pattern mirrors `dev/fetch_english_corpus.py` exactly — same
  manifest format, same gitignored output directory.

**Alternatives considered**:
- AISHELL-1 sample: rejected — requires registration, license unclear for
  redistribution.
- Synthetic espeak-zh: rejected — synthetic CER is not predictive (per
  CLAUDE.md: synthetic WER for Nemotron is 44.6 % vs 0 % real).
- Mandarin-LibriSpeech: rejected — requires download from OpenSLR with
  registration; Common Voice is lower-friction for the same quality class.

## Decision 8: Warm-up — synthetic 6 s noise, matching the reference app

**Decision**: At load time, run one inference pass with 6 s of low-amplitude
synthetic Gaussian noise before reporting `PHASE_READY`. The noise amplitude
and length match the reference app's `warm_up()` exactly, and the approach
(reproducible seed, low amplitude to avoid degenerate fbank paths) is directly
adopted.

```python
rng = np.random.default_rng(0)
synth = (rng.standard_normal(int(16000 * 6.0)) * 50.0).astype(np.float32)
_ = self._model(synth)
```

**Rationale**:
- ORT dynamic-shape optimization is lazy — the first real-length input triggers
  graph compilation costing "hundreds of ms to seconds" (per reference app
  inline comments). This cost MUST NOT be charged to the first real utterance
  (FR-009, SC-003).
- Not zeros: the reference app explicitly warns that pure zeros hit a
  degenerate fbank branch that may skip operators, leaving the warm-up
  incomplete.
- We adopt the exact parameters used in production by a shipped app rather
  than guessing.

**Alternatives considered**:
- Warm with a real audio clip from the corpus: rejected — adds a file I/O
  dependency for a one-time initialization; synthetic noise is pure in-memory,
  zero-file-dependency, and works identically inside a snap.
