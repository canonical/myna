# Feature Specification: FunASR / SenseVoice Backend (Adapter + Inference Snap)

**Feature Branch**: `009-funasr-sensevoice-backend`

**Created**: 2026-07-31

**Status**: Draft

**Input**: User description: "Reference review of MyVoiceTyping (macOS app integrating FunASR models, best-in-class for Chinese). Snap a FunASR model (SenseVoice-Small ONNX + CT-Transformer punctuation) and ensure it works with our protocols — a myna-server adapter plus a strictly-confined inference snap serving the existing session wire contract."

## Background

A reference review of the MyVoiceTyping macOS app (`reference/MyVoiceTyping/`)
showed that FunASR's SenseVoice-Small can be served with a very thin runtime —
ONNX Runtime + kaldi-native-fbank + sentencepiece, no torch/NeMo — and is
best-in-class for Chinese while also covering English, Cantonese, Japanese, and
Korean with automatic language detection. Myna currently has no Chinese-capable
adapter (Whisper is weak on Chinese; Nemotron is English-only; Qwen3-ASR is
multilingual but CPU-bound through a custom runtime). This feature adds SenseVoice
as a first-class myna backend: an adapter in `myna-server` for the testbed, and a
strictly-confined inference snap shipping the model as a component, both speaking
the existing session wire contract unchanged.

## Clarifications

### Session 2026-07-31

- Q: SenseVoice CTC output is unpunctuated — is punctuation restoration in scope? →
  A: No. This feature ships unpunctuated output (same posture as the existing
  sherpa backend, which advertises `punctuation: false`); punctuation restoration
  and any other transcript post-processing will be specced separately as a shared
  post-processing feature. This supersedes the punctuation scope implied by the
  original feature input.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Chinese dictation through the standard client (Priority: P1)

A user who dictates in Chinese (or mixed Chinese/English) selects the FunASR
backend and dictates with the existing push-to-talk client. Their speech is
transcribed with SenseVoice-class accuracy and injected into the focused
application, exactly as with the whisper or nemotron backends today — no client
changes, no new wire events, no user-visible protocol differences.

**Why this priority**: This is the entire point of the feature — unlocking
best-in-class Chinese ASR for myna users. It is independently valuable the moment
a `myna-server` instance can serve SenseVoice over the existing wire.

**Independent Test**: Start `myna-server` with the FunASR adapter, run
`myna-dictate` against it (both wire dialects), dictate or replay a Chinese
utterance, and verify committed text is returned and injected. Delivers a working
Chinese dictation backend even without punctuation restoration or packaging.

**Acceptance Scenarios**:

1. **Given** a running FunASR backend and a client connected over the internal
   wire dialect, **When** the user pushes 16 kHz mono PCM of a Chinese utterance
   and commits, **Then** the client receives a `transcription.final` followed by
   `transcription.done` containing the Chinese transcript.
2. **Given** the same backend, **When** a client connects using the IE115
   dialect, **Then** the session completes with the standard IE115 event flow
   (`completed` after commit) with no protocol errors.
3. **Given** a mixed Chinese-English utterance, **When** it is transcribed,
   **Then** the output contains both the Chinese and English spans correctly
   recognized (code-switching works without user configuration).
4. **Given** audio in an unsupported format (wrong sample rate or channel
   count), **When** the client sends it, **Then** the backend rejects it per the
   audio-push invariant instead of silently resampling.
5. **Given** any transcription output, **When** it reaches the client, **Then**
   it contains no residual model control tags or metadata tokens — only text the
   user could have spoken.

---

### User Story 2 - Strictly-confined FunASR inference snap (Priority: P2)

A user installs a `funasr` snap from the store (or sideloads it), alongside the
`myna` client snap, and dictation works end-to-end under strict confinement with
no network access at runtime — model weights arrive as snap components, and the
client reaches the backend over the shared session socket exactly as with the
whisper snap.

**Why this priority**: Packaging is what makes the backend shippable to real
Ubuntu desktops, but the adapter (US1) is fully exercisable and evaluable in
the testbed before any snap exists.

**Independent Test**: Install the snap and its model component on an Ubuntu
desktop, connect the confined `myna` client snap via the content-shared socket,
and complete a dictation session with the network disabled.

**Acceptance Scenarios**:

1. **Given** the snap and model component installed, **When** the backend
   starts, **Then** it serves sessions without any network connection.
2. **Given** the confined `myna` client snap, **When** it connects over the
   content-shared socket and dictates, **Then** text is returned and injected as
   with any other backend snap.
3. **Given** a cold start of the backend, **When** the first session connects,
   **Then** the client observes the standard `preparing` → `ready` lifecycle and
   is never left waiting without a status indication.

---

### Edge Cases

- Empty or near-silent audio: the backend returns an empty transcript rather
  than an error or hallucinated text (the model's no-speech detection is
  honored).
- Very long utterances beyond typical dictation length: the backend completes
  successfully without unbounded memory growth.
- Language auto-detection picks the wrong language on short ambiguous
  utterances: the user (or client) can pin a language for the session.
- Model files are missing or corrupt at startup: the backend fails fast with a
  clear error, never silently substituting a different model.
- First real-length inference after load incurs a one-time graph-optimization
  cost: this is absorbed by warm-up before the backend reports `ready`, not
  charged to the user's first utterance.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The system MUST provide a FunASR/SenseVoice-Small adapter for
  `myna-server` implementing the existing `myna.core` session contract, so all
  harness, metrics, and client tooling work against it unchanged.
- **FR-002**: The adapter MUST accept only the audio formats it advertises
  (16 kHz mono, the project's standard sample encoding) and MUST reject
  off-format audio per the audio-push invariant; the adapter MUST NOT resample.
- **FR-003**: The adapter MUST support both existing wire dialects (internal and
  IE115) with no dialect-specific behavior — translation happens only at the
  existing codec edges.
- **FR-004**: The adapter MUST operate in batch/commit mode: every emitted
  transcript segment is committed (never retracted), consistent with the
  streaming contract's `committed` disposition; native streaming emission is out
  of scope for this feature.
- **FR-005**: The adapter MUST strip all model control/rich-transcription tags
  (language, emotion, event, and no-speech markers) from output before emitting
  any transcript event.
- **FR-006**: The adapter MUST support language selection of `auto`, `zh`, `en`,
  `yue`, `ja`, and `ko`, with `auto` (model-side detection) as the default, and
  MUST advertise exactly this set via capabilities discovery.
- **FR-007**: The adapter MUST support optional inverse text normalization
  (digits, dates) as a decode-time choice, defaulting to `woitn` ("without
  ITN" — spoken words rendered as-is, matching dictation expectations; per
  research.md Decision 5). The `withitn` mode is available as a constructor
  flag.
- **FR-008**: The backend MUST emit transcripts as the model produces them —
  unpunctuated, without capitalization restoration — and MUST advertise
  `punctuation: false` in capabilities, matching the existing sherpa backend's
  posture. Punctuation restoration and all other transcript post-processing
  (e.g., a CT-Transformer stage, LLM polish) are out of scope here and will be
  specced separately as a shared post-processing feature serving all
  unpunctuated backends (sherpa included).
- **FR-009**: The backend MUST perform a warm-up inference at load time so that
  the one-time runtime graph-optimization cost is incurred during the
  `preparing` phase, before the backend reports `ready`; first real-utterance
  latency MUST NOT include this cost.
- **FR-010**: The backend MUST emit the standard lifecycle progress events
  (`preparing`, `ready`, `transcribing`) so clients can distinguish model
  loading from decoding.
- **FR-011**: The system MUST NOT persist audio to disk at any point in the
  pipeline, and MUST NOT log transcription content by default (privacy
  invariants; the reference app's record-to-disk behavior is explicitly
  rejected).
- **FR-012**: A request for a model the backend does not serve MUST be rejected
  (`model_not_available`), never silently substituted.
- **FR-013**: The system MUST provide a strictly-confined `funasr` inference
  snap following the established per-family pattern: model weights shipped as
  snap components, idle-unload via the model-control mechanism, no `network`
  plug, and the session socket shared via the existing content-share mechanism.
- **FR-014**: The snap MUST run on CPU with no GPU requirement; GPU acceleration
  is out of scope for this feature.
- **FR-015**: The testbed MUST be able to evaluate the adapter on both English
  and Chinese reference corpora, reporting accuracy (WER for English, CER for
  Chinese) through the existing metrics pipeline; a Chinese reference corpus
  with a fetch/generate script MUST be added for this purpose.
- **FR-016**: Model quantization MUST be auto-detected from the staged model
  files (quantized weights used when present), so component variants can ship
  either precision without configuration changes.

### Key Entities *(include if feature involves data)*

- **FunASR model bundle**: the on-disk artifact set for recognition (graph
  weights, frontend configuration, normalization statistics, tokenizer),
  versioned and staged as snap components.
- **Capabilities document**: the backend's advertised languages, input formats,
  and punctuation posture (`punctuation: false` for this backend), consumed by
  clients before session start.
- **Chinese reference corpus**: reference audio + transcripts for Chinese
  accuracy evaluation, gitignored and regenerated by a fetch script like the
  existing real corpus.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: Chinese dictation accuracy on the Chinese reference corpus meets
  or exceeds the model family's published benchmark accuracy (CER within 1
  percentage point of published SenseVoice-Small figures on comparable data).
- **SC-002**: English utterances from the existing real corpus transcribe
  through the FunASR backend with accuracy no worse than the established
  whisper-tiny baseline on the same corpus.
- **SC-003**: On the reference CPU environment, end-to-end commit latency for a
  typical dictation utterance (≤ 15 s) is under 2 seconds after the backend has
  reported `ready`; the first utterance after load is not measurably slower than
  subsequent ones.
- **SC-004**: A full dictation session (connect → dictate → commit → inject)
  succeeds over both wire dialects with zero protocol errors, and is
  indistinguishable from a whisper-backend session from the client's
  perspective.
- **SC-005**: The installed snap completes a full dictation session with the
  network interface disabled, and peak memory during a session stays within the
  watermark tolerance recorded for a small-model backend (tolerance defined in
  the performance watermarks baselines alongside whisper-tiny; recorded in
  T021).
- **SC-006**: Committed transcripts contain zero residual control tags across
  the entire evaluation corpus (automated scan).

## Assumptions

- The SenseVoice-Small ONNX export (graph, frontend config, CMVN stats,
  tokenizer) is redistributable as a snap component under its Apache-2.0/MIT
  licensing; final license review happens during planning. (The CT-Transformer
  punctuation export rides with the future post-processing feature, not this
  one.)
- Model artifacts are fetched from ModelScope (with a Hugging Face mirror as
  fallback) at component-build or first-stage time — never at snap runtime.
- The thin vendored ONNX runtime approach (ONNX Runtime + fbank + tokenizer, no
  torch/NeMo) is the reference implementation for the adapter; the adapter lives
  in the Python evaluation-harness tier (exempt from the Rust rule and TDD),
  consistent with the existing whisper/nemotron/qwen adapters.
- Hotword/biasing support is out of scope: the ONNX path has no native hotword
  mechanism, the wire contract has no hotword channel today, and the reference
  app's fuzzy post-replacement is judged too fragile to adopt.
- Unpunctuated output is an accepted v1 limitation (sherpa precedent); accuracy
  evaluation accounts for it (CER/WER normalization), and the post-processing
  feature is expected to follow as its own spec.
- Chinese reference audio will come from a permissively licensed corpus (e.g., a
  Common Voice zh-CN subset or AISHELL sample), fetched by script and
  gitignored, mirroring `dev/fetch_english_corpus.py`.
- Streaming emission for SenseVoice (rolling re-decode or a streaming Paraformer
  variant) is explicitly deferred; this feature ships batch mode only.
- The GPU runtime tier and any LLM-based transcript polishing (the reference
  app's GGUF correction pass) are out of scope.
