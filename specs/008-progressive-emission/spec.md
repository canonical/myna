# Feature Specification: Progressive Streaming Emission

**Feature Branch**: `008-progressive-emission`

**Created**: 2026-07-27

**Status**: Draft

**Input**: Plumb `--streaming` through to the whisper adapter with real mid-audio emission, supporting multiple streaming methods (LocalAgreement, tail-mutation, fixed-head) — the protocol must carry any of them. Plumb streaming through the nemotron adapter (native cache-aware loop). Add a Parakeet-based snap informed by murmure's learnings (separate from the full-fat NeMo snap) and a sherpa-onnx-based snap. This concludes the streaming investigation.

Feature 007 (dual-mode streaming) shipped the wire contract (`disposition: committed|unstable`, revision semantics, `segment_index`), the client FSM routing, `--mode auto|streaming|batch`, and the tier-gate infrastructure — but the adapters only emit *after* full decode (documented FR-008 gap). Empirically confirmed 2026-07-27: `myna-dictate --mode streaming --show-unstable` against both our server and the canonical/whisper-snap produces zero mid-audio unstable output. This feature makes streaming real at the source: emission while audio is still arriving.

## Clarifications

### Session 2026-07-27

- Q: Should the whisper adapter support driving decode via an external WhisperLive-compatible subprocess? → A: No — removed from scope. The tail-mutation strategy implements the same algorithm (rolling re-decode, commit-all-but-the-trailing-segment) in-adapter, so a managed subprocess adds packaging and failure-mode complexity with no new capability. The interop lessons from the canonical snap are already captured in 007's interop report.

## User Scenarios & Testing *(mandatory)*

### User Story 1 — Whisper emits hypotheses and committed text while the user is still speaking (Priority: P1)

A user dictating with the default whisper-backed service on a streaming-capable tier sees provisional text forming within a couple of seconds of speech start, and sees chunks of text lock in as permanent while still speaking — not everything-at-once after they stop. An operator (or snap packager) can choose *how* the adapter produces this behavior — agreement-based prefix commit, conservative tail replacement, or fixed-head windowing — without any client or protocol change.

**Why this priority**: Whisper is the default, multilingual, CPU-feasible model most users run. This closes the FR-008 gap for the primary backend and is the direct fix for the failed manual validation of 2026-07-27.

**Independent Test**: Run `myna-dictate --clip <≥8s real-speech clip> --mode streaming --show-unstable` (realtime-paced) against a streaming-enabled whisper service: unstable lines appear *during* clip playback, at least one committed segment arrives before end-of-audio, and the final transcript equals the concatenation of committed segments.

**Acceptance Scenarios**:

1. **Given** a whisper service in streaming mode on its target tier, **When** an 8+ second utterance is streamed in realtime, **Then** at least one unstable hypothesis is emitted while audio is still arriving, and at least one committed segment is emitted before end-of-audio.
2. **Given** any of the supported streaming strategies is selected, **When** a session runs, **Then** committed text is append-only (never retracted or duplicated), unstable text supersedes only the most recent unstable text, and the final transcript equals the concatenation of committed segments.
3. **Given** the same model and hardware, **When** an utterance is transcribed in streaming mode and in batch mode, **Then** the transcripts agree within the accuracy tolerance (WER within 2 percentage points on the real corpus).
4. **Given** an utterance shorter than the first emission window, **When** the session ends, **Then** the final transcript is still correct even if no mid-audio emission occurred.

---

### User Story 2 — Nemotron streams natively with per-frame cost independent of utterance length (Priority: P1)

A user on a GPU tier with the nemotron-backed service gets true transducer streaming: each audio frame is processed exactly once, provisional text tracks speech with a small fixed latency, and committed text appears at natural boundaries mid-utterance. A 30-second dictation finalizes as quickly after end-of-speech as a 5-second one — there is no growing re-decode backlog.

**Why this priority**: This is the architectural point of the nemotron backend (cache-aware transducer) and the quality ceiling for the streaming investigation; it also demonstrates the frame-once property that re-decode strategies cannot offer.

**Independent Test**: Stream a 30-second realtime clip to a streaming-enabled nemotron service: unstable partials arrive continuously during playback, time-to-first-committed is comparable to that of a 5-second clip, and finalize latency after end-of-audio is sub-second on the target tier.

**Acceptance Scenarios**:

1. **Given** a nemotron service in streaming mode, **When** audio is streamed in realtime, **Then** each audio frame is encoded exactly once (no re-decode of previously processed audio).
2. **Given** a 30-second utterance, **When** end-of-audio is signalled, **Then** the final transcript is emitted within the finalize watermark on the target tier.
3. **Given** the latency/accuracy dial is configured, **When** sessions run at different settings, **Then** measured latency moves in the expected direction without a protocol or client change.

---

### User Story 3 — A small transducer snap (Parakeet-class) serves CPU tiers with progressive commit (Priority: P3)

A user on modest CPU-only hardware installs a small transducer snap (a fraction of the full-fat NeMo snap's size) and gets progressive committed dictation via chunked decode: speech is cut at pauses, each finalized chunk is decoded once and committed while the user keeps speaking. Informed by murmure's proven constants (silence-arm ~15 s, ~500 ms silence cut, ~60 s force-cut with ~1 s overlap, dedupe at merge) and its int8 ONNX packaging.

**Why this priority**: Packaging/footprint play that widens tier coverage — streaming-class UX on hardware that cannot run the GPU snap. Builds on US1's validated chunk-commit semantics.

**Independent Test**: Install the snap confined, dictate a multi-sentence utterance against it with `myna-dictate`, and verify committed segments arrive mid-utterance and the installed size meets the footprint criterion.

**Acceptance Scenarios**:

1. **Given** the small transducer snap on a CPU-only target tier, **When** the user dictates continuously past the first speech pause, **Then** at least one committed segment is emitted before end-of-audio.
2. **Given** the snap is strictly confined, **When** a session runs, **Then** the full session contract is served unchanged (greeting, capabilities, ready gating, disposition-tagged events) with no audio persisted.

---

### User Story 4 — A sherpa-onnx-based snap provides turnkey small-footprint native streaming (Priority: P3)

A user or operator can choose a small snap built on a streaming-transducer runtime that emits partial hypotheses and committed segments natively (chunk-at-a-time, built-in endpointing), without the heavyweight training-framework dependency of the full NeMo snap.

**Why this priority**: This is the "turnkey" comparison point that concludes the streaming investigation: native streaming, small package, no custom decode loop to maintain. Its measurements decide whether it becomes the recommended small transducer snap or a documented alternative.

**Independent Test**: Install the snap confined, stream a realtime clip, and verify continuous unstable partials plus mid-utterance committed segments; record its size, latency, and accuracy alongside the other backends for the final streaming report.

**Acceptance Scenarios**:

1. **Given** the sherpa-onnx snap, **When** audio streams in realtime, **Then** unstable partials arrive continuously and committed segments are emitted at its native endpoint boundaries.
2. **Given** the investigation concludes, **When** the final streaming report is produced, **Then** it compares all delivered backends (whisper strategies, nemotron native, small transducer snaps) on accuracy, latency profile, footprint, and tier coverage, with a recommendation per hardware tier.

---

### Edge Cases

- **Short utterances** (below the first emission window): a session may legitimately produce zero mid-audio emissions; the final transcript MUST still be complete and correct.
- **End-of-audio with uncommitted tail**: any unstable or uncommitted text MUST be resolved to committed text (or discarded if empty) before the terminal event — a session never ends with text stuck in unstable limbo.
- **Long utterances**: committed-prefix advancement MUST bound the uncommitted audio window so memory stays within bounded-buffer limits (no unbounded growth for multi-minute dictation).
- **CPU tier overruns**: if a re-decode-based strategy cannot keep up on the active tier, the tier gate MUST exclude it (auto mode falls back to batch) rather than deliver a stuttering stream.
- **Strategy selection**: fixed per service instance (server flag / snap config); a client never negotiates strategy mid-session, and a running session's strategy never changes.
- **Silence-only audio**: no spurious committed segments; the session completes with an empty transcript as in batch mode.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The whisper adapter in streaming mode MUST emit at least one unstable hypothesis delta while audio is still arriving, for utterances exceeding the first emission window on the target tier.
- **FR-002**: The whisper adapter in streaming mode MUST emit at least one committed segment before end-of-audio for any utterance longer than 5 seconds on the target tier (this makes 007's FR-008 real for whisper).
- **FR-003**: The whisper adapter MUST support at least three selectable streaming strategies behind a server-side option: agreement-based prefix commit ("LocalAgreement"-style), conservative tail replacement ("tail-mutation"-style), and fixed-head windowing ("fixed-head"-style). The default MUST be the agreement-based strategy.
- **FR-004**: Strategy selection MUST NOT change the wire contract, event vocabulary, or client behavior. All strategies MUST express themselves through the existing committed/unstable dispositions and revision semantics (007 contract); no new event types and no protocol version bump.
- **FR-005**: Across all strategies and backends, committed text MUST be append-only and never retracted, and the final transcript MUST equal the concatenation of committed segments (no gaps, overlaps, or duplicates).
- **FR-006**: The nemotron adapter in streaming mode MUST use the model's cache-aware incremental decode path so each audio frame is encoded exactly once; per-frame processing cost MUST NOT grow with utterance length. The existing latency/accuracy dial MUST remain effective.
- **FR-007**: The nemotron adapter in streaming mode MUST emit unstable partials mid-utterance and committed segments at its native segment/endpoint boundaries.
- **FR-008**: Batch mode MUST remain unchanged on every adapter: a single committed segment delivered after inference completes (degenerate streaming), with no new latency or behavior regressions.
- **FR-009**: Every streaming strategy and backend MUST be measurable through the existing testbed, recording per-tier watermarks for at least: time-to-first-unstable, time-to-first-committed, finalize latency after end-of-audio, real-time factor, and peak memory. Tier gating MUST consume these watermarks so incapable tiers fall back to batch in auto mode.
- **FR-010**: A Parakeet-class transducer snap MUST be deliverable as a strictly confined package, separate from and substantially smaller than the full NeMo snap, targeting CPU tiers, decoding finalized chunks once and emitting committed segments progressively.
- **FR-011**: A sherpa-onnx-based snap MUST be deliverable as a strictly confined package with native chunk-at-a-time streaming emission (continuous partials plus endpoint-driven commits).
- **FR-012**: Every new backend MUST serve the existing session contract unchanged — server-first greeting, capabilities with accepted input formats, ready gating before audio, disposition-tagged transcript events — and MUST reject off-format audio rather than resample (audio-push invariant).
- **FR-013**: Privacy invariants bind all backends: audio MUST NOT be persisted; transcription content (including unstable text) MUST NOT be logged by default; unstable text MUST NOT be injected into the focused field (007's FR-007 carries).
- **FR-014**: Streaming accuracy MUST hold per backend and strategy: WER on the real corpus within 2 percentage points of the same model's batch mode (007's SC-002 carried into per-strategy gates).

### Key Entities

- **Streaming strategy**: a named emission policy for re-decode-based adapters (agreement-based, tail-replacement, fixed-head), with a cost profile (re-decode window, cadence) and a commit rule. Server-selected; invisible on the wire.
- **Backend snap**: a model-family package (whisper, nemotron full-fat, parakeet-class small, sherpa-onnx small) serving the same session contract, with a footprint class and target hardware tier.
- **Emission watermark set**: per-backend, per-tier recorded baselines (time-to-first-unstable, time-to-first-committed, finalize latency, RTF, peak memory) consumed by tier gating and regression checks.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: On the target tier, an 8+ second realtime utterance against the default whisper strategy produces first provisional text within 2 seconds of speech start and first permanent text within 5 seconds of speech start.
- **SC-002**: Commit stability is 100% across all strategies and backends in automated sweeps: no committed text is ever retracted, restated, or duplicated (carries 007's SC-004).
- **SC-003**: Streaming-mode accuracy is within 2 percentage points WER of batch mode for each model on the real corpus (carries 007's SC-002, now enforced per strategy).
- **SC-004**: On the nemotron backend, a 30-second utterance finalizes within 1 second of end-of-audio on the target tier, and time-to-first-committed for a 30-second utterance is within 1.5× that of a 5-second utterance (frame-once evidence).
- **SC-005**: Each small transducer snap's installed size is at most 25% of the full NeMo snap's installed size (and under 1 GB absolute) while meeting its tier's emission watermarks.
- **SC-006**: The previously failing manual validation now passes on both plumbed adapters: `myna-dictate --clip <real-speech ≥8 s> --mode streaming --show-unstable` shows provisional and committed lines arriving during playback for whisper and nemotron services.
- **SC-007**: The concluding streaming report covers all delivered backends with measured accuracy, latency profile, footprint, and tier coverage, and names a recommended backend per hardware tier.

## Assumptions

- The 007 wire contract is sufficient as-is for all three strategies (committed/unstable dispositions, supersede-most-recent-unstable revision semantics, segment indexing); this feature verifies that per strategy rather than extending the protocol. Any gap discovered becomes an additive contract change flagged in the plan.
- Streaming strategy is a server-side/operator choice (server flag or snap configuration), not per-session client negotiation; clients remain strategy-agnostic.
- The tail-mutation strategy deliberately reuses the WhisperLive commit heuristic (commit all but the trailing segment, stuck-partial escape) implemented in-adapter; its commit guarantee is the weakest of the three strategies and is measured like any other (SC-002) rather than assumed.
- The Parakeet-class snap defaults to the multilingual (25-language) int8 ONNX export; murmure's published chunking constants are starting points to be re-validated on our corpora, not adopted blindly.
- The sherpa-onnx snap uses an exported NeMo-family transducer model (FastConformer/Parakeet-class) under its streaming recognizer.
- The full-fat NeMo snap remains the GPU/reference tier; the two small snaps target CPU/edge tiers.
- Out of scope: unstable-hypothesis display in the desktop UI/indicator (remains gated on design sign-off per 007), dictionary/boost biasing (murmure's boost-tree learning — candidate for its own feature), wake word, and LLM post-processing.
- Python adapter work in `myna-server`/testbed is evaluation-harness tier under the constitution (TDD-exempt) but remains bound by the privacy and offline invariants; any Rust client changes follow red-green TDD.
