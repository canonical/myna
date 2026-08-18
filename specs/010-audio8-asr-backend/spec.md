# Feature Specification: Audio8-ASR Backend (Adapter + Benchmark Comparison)

**Feature Branch**: `010-audio8-asr-backend`

**Created**: 2026-08-17

**Status**: Draft

**Input**: User description: "We have a new ASR model to check out — Audio8/Audio8-ASR-0.1B on Hugging Face. We already benchmarked most of our ASR adapters; the goal is to integrate this promising new model and compare it to what we currently have available, following the example of existing specs and keeping test coverage high like the others."

## Background

Audio8-ASR-0.1B is a compact autoregressive ASR model (a ~0.1B-parameter
Qwen-style causal LM decoder over a Qwen3-ASR audio encoder, ~0.32B parameters
end-to-end) published on Hugging Face. It targets multilingual recognition
(English, Chinese, French, German, Japanese, Korean, Cantonese) at 16 kHz and
reports Open ASR Leaderboard English accuracy (7.03% mean WER) in the same
class as far larger models, plus strong Chinese figures. That profile —
small footprint, multilingual, LLM-class accuracy — is exactly the gap myna's
current backend roster does not obviously cover.

Every existing backend (whisper, sherpa, parakeet, nemotron, qwen, funasr) has
already been benchmarked through the same testbed adapter + corpus + metrics
pipeline. This feature adds Audio8 as a first-class testbed backend and runs it
through that identical pipeline, producing an apples-to-apples comparison
against the recorded baselines. Two properties of the model shape the scope:

- **License**: the checkpoint is CC-BY-NC-4.0 (non-commercial). Per the
  2026-08-17 clarification, the snap ships like the other backends and license
  compliance is the integrator's responsibility; tooling surfaces the license
  for explicit acknowledgment rather than blocking distribution.
- **Autoregressive LLM decoding**: unlike the CTC/encoder backends, decode is
  generative with a documented 30-second audio cap, no native streaming mode,
  and a hallucination risk on non-speech audio. The adapter must bound decode
  and handle silence honestly.

## Clarifications

### Session 2026-08-17

- Q: Is snap packaging in scope despite the CC-BY-NC-4.0 license? → A: Yes —
  ship a strictly-confined inference snap like the other backends; license
  compliance is the integrator's/distributor's responsibility, surfaced (not
  enforced) by the fetch/build tooling. This supersedes the evaluation-only
  scope in the original draft.
- Q: Does Audio8 support streaming, and is GPU acceleration in scope? → A: No
  native streaming exists (full-sequence audio encoder + autoregressive
  decode, 30 s cap; neither the Transformers checkpoint nor the publisher's
  ONNX release documents a streaming mode) — this feature ships batch/commit
  only. GPU acceleration IS in scope where applicable, following the per-family
  snap engine pattern (cpu baseline + nvidia-gpu component, whisper precedent).
- Measured follow-up (implementation, 2026-08-17): language *pinning* is
  empirically inert — prompt-based "transcribe in X" instructions are ignored
  (results/spike-audio8-language.md), so FR-006 is amended: selection is
  `auto`-only, the seven-language set remains recognition scope under
  auto-detection. Output is natively punctuated/capitalized
  (`punctuation: true`, results/spike-audio8-posture.md), and the model emits
  empty output on silence/noise (no hallucination).

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Audio8 dictation through the standard testbed (Priority: P1)

A developer (or evaluator) starts `myna-server` with the Audio8 backend and
dictates with the existing push-to-talk client — English, Chinese, or any other
supported language. Speech is transcribed with Audio8-class accuracy and
committed to the client exactly as with the whisper or funasr backends today:
no client changes, no new wire events, no user-visible protocol differences.

**Why this priority**: The adapter over the existing session contract is the
enabler for everything else — it is what makes the model exercisable by the
harness, the metrics pipeline, and the benchmarker. It is independently
valuable the moment a `myna-server` instance can serve Audio8 over the wire.

**Independent Test**: Start `myna-server` with the Audio8 adapter, run
`myna-dictate` against it (both wire dialects), dictate or replay an English
and a Chinese utterance, and verify committed text is returned. Delivers a
working Audio8 backend even before any benchmarking runs.

**Acceptance Scenarios**:

1. **Given** a running Audio8 backend and a client connected over the internal
   wire dialect, **When** the user pushes 16 kHz mono PCM of an English
   utterance and commits, **Then** the client receives a `transcription.final`
   followed by `transcription.done` containing the transcript.
2. **Given** the same backend, **When** a client connects using the IE115
   dialect, **Then** the session completes with the standard IE115 event flow
   (`completed` after commit) with no protocol errors.
3. **Given** a Chinese utterance, **When** it is transcribed with language set
   to `auto`, **Then** the output contains the Chinese transcript without any
   user configuration.
4. **Given** audio in an unsupported format (wrong sample rate or channel
   count), **When** the client sends it, **Then** the backend rejects it per
   the audio-push invariant instead of silently resampling.
5. **Given** any transcription output, **When** it reaches the client, **Then**
   it contains no residual special tokens, prompt fragments, or chat-template
   markup — only text the user could have spoken.
6. **Given** near-silent audio, **When** it is committed, **Then** the backend
   returns an empty transcript rather than hallucinated text.

---

### User Story 2 - Benchmark comparison against existing backends (Priority: P2)

An evaluator runs the Audio8 backend through the existing benchmark pipeline on
the existing reference corpora (English real corpus and Chinese corpus) and
produces a comparison report placing Audio8 alongside every previously
benchmarked backend — accuracy, latency, and throughput on identical clips,
through identical scoring.

**Why this priority**: This is the stated goal of the feature — deciding
whether Audio8 earns a place in the roster. It depends on US1 (the adapter is
the instrument), but the adapter alone already delivers a usable backend, so
the comparison is a separate, independently demonstrable slice.

**Independent Test**: Run the benchmarker against a running Audio8 backend with
a distinctive results label, aggregate the output with the recorded baselines
of the other backends, and verify the report contains comparable per-backend
metrics on the same clips.

**Acceptance Scenarios**:

1. **Given** a running Audio8 backend, **When** the benchmarker sweeps the
   English real corpus, **Then** one JSONL record per clip is appended with
   WER, latency, and RTF fields identical in shape to existing backend runs.
2. **Given** the same backend, **When** the benchmarker sweeps the Chinese
   corpus, **Then** CER results are recorded comparable to the funasr Chinese
   baseline runs.
3. **Given** completed Audio8 runs and the recorded baselines of the other
   backends, **When** the aggregator runs, **Then** a single report ranks all
   backends on the same corpus clips by accuracy and latency.
4. **Given** the comparison results, **When** they are reviewed, **Then** they
   are checked into the repository (or reproducibly regenerable) so the
   go/no-go decision on the model is auditable.

---

### User Story 3 - Strictly-confined Audio8 inference snap (Priority: P3)

A user installs an `audio8` snap alongside the `myna` client snap, and
dictation works end-to-end under strict confinement with no network access at
runtime — model weights arrive as snap components, and the client reaches the
backend over the shared session socket exactly as with the whisper or funasr
snaps. On machines with a compatible GPU, the user can select the GPU engine
for faster decoding.

**Why this priority**: Packaging makes the backend shippable to real Ubuntu
desktops, but it only earns that place if the US2 comparison is favorable, and
the adapter (US1) is fully exercisable before any snap exists.

**Independent Test**: Install the snap and its model component on an Ubuntu
desktop, connect the confined `myna` client snap via the content-shared socket,
and complete a dictation session with the network disabled; on GPU hardware,
switch engines and repeat.

**Acceptance Scenarios**:

1. **Given** the snap and model component installed, **When** the backend
   starts, **Then** it serves sessions without any network connection.
2. **Given** the confined `myna` client snap, **When** it connects over the
   content-shared socket and dictates, **Then** text is returned as with any
   other backend snap.
3. **Given** a cold start of the backend, **When** the first session connects,
   **Then** the client observes the standard `preparing` → `ready` lifecycle
   and is never left waiting without a status indication.
4. **Given** a machine with a compatible GPU, **When** the user selects the
   GPU engine, **Then** sessions are served by the GPU engine and benchmark
   runs are distinguishable from CPU runs by their label.
5. **Given** the GPU engine selected on a machine without a compatible GPU,
   **When** the backend starts, **Then** it fails fast with a clear error
   rather than silently serving a different engine.

---

### Edge Cases

- Empty or near-silent audio: autoregressive decoders hallucinate on silence;
  the backend returns an empty transcript rather than invented text.
- Degenerate decoding (repetition loops on ambiguous audio): generation is
  bounded by a maximum output length; the session completes with a final
  event instead of hanging.
- Utterances beyond the model's maximum per-decode input length (~30 s): the
  backend splits them into per-chunk decodes and stitches the transcripts
  (chunk-and-stitch, FR-009 amended) — audio is unbounded like the other
  adapters; a word may be cut at a chunk boundary (documented v1 tradeoff).
- Language auto-detection picks the wrong language on short ambiguous
  utterances: pinning is NOT available (measured: prompt pinning inert —
  results/spike-audio8-language.md); the model auto-detects only, and this
  limitation is documented in capabilities.
- A request for a model the backend does not serve: rejected
  (`model_not_available`), never silently substituted.
- Model files missing or corrupt at startup: the backend fails fast with a
  clear error and never attempts a runtime download (offline invariant).
- First real-length inference after load incurs one-time
  initialization/compilation cost: absorbed by warm-up during `preparing`, not
  charged to the user's first utterance.
- GPU engine selected but no compatible GPU present: the backend fails fast
  with a clear error at startup, never silently substituting the CPU engine
  mid-configuration.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The system MUST provide an Audio8-ASR-0.1B adapter for
  `myna-server` implementing the existing `myna.core` session contract, so all
  harness, metrics, and client tooling work against it unchanged.
- **FR-002**: The adapter MUST accept only the audio format it advertises
  (16 kHz mono, the project's standard sample encoding) and MUST reject
  off-format audio per the audio-push invariant; the adapter MUST NOT resample.
- **FR-003**: The adapter MUST support both existing wire dialects (internal
  and IE115) with no dialect-specific behavior — translation happens only at
  the existing codec edges.
- **FR-004**: The adapter MUST operate in batch/commit mode: every emitted
  transcript segment is committed (never retracted), consistent with the
  streaming contract's `committed` disposition; streaming emission is out of
  scope for this feature.
- **FR-005**: The adapter MUST strip all special tokens, control tokens, and
  any chat-template or prompt artifacts from decoded output before emitting any
  transcript event.
- **FR-006**: The adapter MUST operate in `auto` language mode — the model's
  seven-language recognition scope (`en`, `zh`, `fr`, `de`, `ja`, `ko`, `yue`)
  is handled by model-side auto-detection. The adapter MUST advertise
  `languages=("auto",)` via capabilities discovery. Explicit language pinning
  is out of scope: measured inert during implementation
  (results/spike-audio8-language.md), superseding the earlier pin-the-set
  requirement.
- **FR-007**: The adapter MUST advertise a capabilities document that reflects
  empirically verified behavior — in particular its punctuation and
  capitalization posture (generative decoders typically punctuate, unlike the
  CTC backends' `punctuation: false`), confirmed by test rather than assumed.
- **FR-008**: Generation MUST be bounded (maximum output length, deterministic
  decoding) so that pathological audio cannot stall a session indefinitely.
- **FR-009**: Audio of any length MUST be accepted (unbounded, like the other
  adapters). Because the model's audio encoder is fixed-length (~30 s — the
  ONNX audio tower truncates beyond it), audio longer than that MUST be
  split into per-chunk decodes and the transcripts stitched into one committed
  final. The adapter MUST NOT silently truncate audio, and MUST NOT reject
  long audio.
- **FR-010**: The backend MUST perform a warm-up inference at load time so that
  one-time runtime initialization costs are incurred during the `preparing`
  phase, before the backend reports `ready`; first real-utterance latency MUST
  NOT include this cost.
- **FR-011**: The backend MUST emit the standard lifecycle progress events
  (`preparing`, `ready`, `transcribing`) so clients can distinguish model
  loading from decoding.
- **FR-012**: The system MUST NOT persist audio to disk at any point in the
  pipeline, and MUST NOT log transcription content by default (privacy
  invariants).
- **FR-013**: A request for a model the backend does not serve MUST be rejected
  (`model_not_available`), never silently substituted. The backend MUST NOT
  download model artifacts at session time; weights are staged in advance by a
  fetch script (offline invariant).
- **FR-014**: A fetch script MUST stage the model checkpoint from Hugging Face
  for local, offline use, and MUST surface the CC-BY-NC-4.0 (non-commercial)
  license for explicit acknowledgment before downloading; per the 2026-08-17
  clarification, license compliance is the integrator's responsibility — the
  tooling informs, it does not gate.
- **FR-015**: The testbed MUST evaluate the adapter on the existing English
  real corpus (WER) and Chinese corpus (CER) through the existing metrics
  pipeline, with results appended to the results store under a distinctive
  backend label.
- **FR-016**: The system MUST produce a comparison report aggregating the
  Audio8 runs with the recorded baselines of the other benchmarked backends on
  the same corpora, covering accuracy, commit latency, and RTF per backend.
- **FR-017**: The adapter MUST ship with a unit test suite comparable to the
  existing adapters' (output sanitization, language handling, audio-format
  validation, error paths, bounded generation) and MUST be wired into the
  adapter-coverage tooling so its merged coverage (unit tests + instrumented
  harness session) is reported alongside the other adapters.
- **FR-018**: The backend MUST run on CPU as the baseline with no GPU
  requirement, and MUST offer GPU acceleration where applicable, following the
  established per-family snap engine pattern (CPU and GPU engines selectable,
  whisper precedent); benchmark runs MUST be labeled per engine so results are
  never conflated.
- **FR-019**: The system MUST provide a strictly-confined `audio8` inference
  snap following the established per-family pattern: model weights shipped as
  snap components, idle-unload via the model-control mechanism, no `network`
  plug, and the session socket shared via the existing content-share mechanism.
- **FR-020**: The snap MUST fail fast with a clear error when a selected engine
  cannot run on the host (e.g., GPU engine without a compatible GPU), never
  silently substituting another engine.

### Key Entities *(include if feature involves data)*

- **Audio8 model bundle**: the staged checkpoint (weights, tokenizer, processor
  configuration, and any runtime code modules), fetched once and used offline
  thereafter; versioned and staged as snap components for the shipped snap,
  under the CC-BY-NC-4.0 license (integrator's responsibility).
- **Capabilities document**: the backend's advertised languages, input format,
  and punctuation posture, consumed by clients before session start.
- **Benchmark run records**: per-clip JSONL results (accuracy, latency, RTF)
  tagged with the Audio8 backend label, stored alongside existing backend runs.
- **Comparison report**: the aggregated cross-backend accuracy/latency summary
  on the shared corpora; the decision artifact for adopting or dropping the
  model.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: A full dictation session (connect → dictate → commit) succeeds
  over both wire dialects with zero protocol errors, indistinguishable from a
  whisper-backend session from the client's perspective.
- **SC-002**: English accuracy (WER) and Chinese accuracy (CER) on the existing
  reference corpora are measured for Audio8 and reported side by side with
  every previously benchmarked backend on identical clips, with 100% of corpus
  clips scored (no skipped or errored clips unaccounted for); each engine
  benchmarked (CPU baseline, GPU where available) appears under a distinct
  label.
- **SC-003**: On the reference CPU environment, end-to-end commit latency for a
  typical dictation utterance (≤ 15 s) is under 2 seconds after the backend has
  reported `ready`; the first utterance after load is not measurably slower
  than subsequent ones.
- **SC-004**: Peak memory during a benchmarked session is recorded and stays
  within the watermark tolerance recorded for a small-model backend in the
  performance baselines.
- **SC-005**: Near-silent clips across the evaluation corpus produce empty or
  trivially short transcripts — zero hallucinated multi-word outputs on
  non-speech audio (automated scan).
- **SC-006**: Committed transcripts contain zero residual special tokens or
  template artifacts across the entire evaluation corpus (automated scan).
- **SC-007**: Merged test + harness coverage of the adapter module reaches
  parity with the existing adapters' merged coverage as reported by the
  adapter-coverage tooling (at minimum the floor recorded for the funasr
  adapter), and the unit suite passes in CI without model weights present.
- **SC-008**: The installed snap completes a full dictation session with the
  network interface disabled, and peak memory during a session stays within the
  watermark tolerance recorded for a small-model backend in the performance
  baselines.

## Assumptions

- **Snap packaging is in scope; licensing is the integrator's call** (2026-08-17
  clarification): the checkpoint is CC-BY-NC-4.0, and the snap ships like the
  other backends. Fetch/build tooling surfaces the license for explicit
  acknowledgment (FR-014) but does not block distribution; compliance rests
  with whoever integrates and distributes the snap.
- The Hugging Face Transformers checkpoint is the reference runtime for this
  evaluation; the publisher's ONNX release (which ships int8/int4 quantized
  decoder variants and needs no trust-remote-code) is a strong planning-stage
  candidate for the snap's CPU engine. Runtime choice is a plan concern, not a
  spec concern.
- Only English and Chinese are benchmarked (the existing reference corpora);
  the remaining supported languages (fr, de, ja, ko, yue) are served by the
  adapter but not accuracy-evaluated in this feature.
- The model's decode-time hotword boosting is out of scope: the wire contract
  has no hotword channel today (same posture as the funasr feature).
- Streaming emission is out of scope: the model has no native streaming mode
  (2026-08-17 clarification), so the adapter ships batch/commit mode only —
  unlike parakeet/sherpa, there is no streaming variant to defer to; any future
  streaming would require a rolling re-decode design and its own spec.
- The model's ~30 s per-decode input length is handled by chunk-and-stitch
  (FR-009 amended): the audio tower is fixed-length, so longer audio is
  decoded in ≤30 s pieces and stitched. Word-boundary cuts at chunk edges are
  an accepted v1 tradeoff, matching the unbounded posture of the other
  adapters; a smarter overlap/dedup or long-form mode is a future refinement
  if the model is adopted.
- The model's trust-remote-code loading requirement must be resolved during
  planning before snap shipment (e.g., vendoring the pinned, reviewed code
  modules into the snap, or using the ONNX release path which needs none);
  loading remote code implicitly is not acceptable in a shipped artifact.
- CPU is the reference baseline environment; the GPU engine is benchmarked
  where compatible hardware is available and reported under a distinct label
  (whisper `cpu`/`nvidia-gpu` precedent).
- The adapter lives in the Python evaluation-harness tier (exempt from the Rust
  rule and TDD per the constitution), consistent with the existing
  whisper/nemotron/qwen/funasr adapters; the high-coverage expectation is met
  through the unit suite plus adapter-coverage tooling (FR-017), not through
  TDD ordering.
