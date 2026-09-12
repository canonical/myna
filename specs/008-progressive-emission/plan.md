# Implementation Plan: Progressive Streaming Emission

**Branch**: `008-progressive-emission` | **Date**: 2026-07-27 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `specs/008-progressive-emission/spec.md`

## Summary

Make streaming real at the source: adapters emit *while audio is still arriving*.
The whisper adapter gains a rolling re-decode loop with three selectable commit
strategies (LocalAgreement default, tail-mutation, fixed-head); the nemotron
adapter switches from offline `model.transcribe()` to NeMo's cache-aware
incremental path (frame-once). Two new small transducer snaps (Parakeet-class
int8 ONNX; sherpa-onnx runtime) conclude the investigation. The 007 wire
contract is reused unchanged — strategies are invisible on the wire. Two
timeboxed spikes de-risk the plan: faster-whisper word-timestamp stability
(LocalAgreement's input) and the NeMo 2.7.3 `streaming_utils` live-feed pattern.

## Technical Context

**Language/Version**: Python 3.12 for server/adapters/snaps (evaluation harness
tier — TDD-exempt per constitution); Rust (stable, 2024 edition) client expected
unchanged (any client touch is TDD).

**Primary Dependencies**: faster-whisper 1.2.1 (verified locally:
`word_timestamps=True` yields `Word(start, end, word, probability)`; built-in
Silero `vad_filter` + `VadOptions`; `no_speech_prob`/`avg_logprob` confidence);
nemo_toolkit 2.7.3 (verified locally: `streaming_utils` exposes
`CacheAwareStreamingAudioBuffer`, `FrameBatchASR`, `BatchedFrameASRRNNT`,
`BatchedFrameASRTDT`, `StreamingEncoder`); onnxruntime + int8 Parakeet ONNX
export (Parakeet snap); sherpa-onnx Python bindings (sherpa snap); the existing
`myna.core` event/wire framework and 007 disposition contract.

**Storage**: N/A (in-memory session state only; strategy selection via server
flag / snap config).

**Testing**: `pytest` (Python — harness tier); `dev/bench.py` / `dev/matrix.py`
extended with emission watermarks; live validation via `myna-dictate --mode
streaming --show-unstable` on realtime-paced real-corpus clips; confined
end-to-end for the new snaps (whisper-snap pattern).

**Target Platform**: Ubuntu Desktop (current LTS+) with PipeWire; snapped
services. CPU-only and CUDA tiers.

**Project Type**: Inference service adapters + snap packaging (2 existing
adapters plumbed; 2 new snaps).

**Performance Goals** (from spec SCs): first unstable ≤ 2 s of speech start and
first committed ≤ 5 s (whisper, target tier); nemotron finalize ≤ 1 s after
end-of-audio at 30 s utterance; streaming WER within 2 pp of batch; commit
stability 100 %; small snaps ≤ 25 % of the full NeMo snap size and < 1 GB.

**Constraints**: No network at runtime; no persisted audio; committed text
append-only; unstable text never injected and never logged by default
(constitution V); strategies server-selected and wire-invisible; no protocol
version bump.

**Scale/Scope**: Single-user desktop dictation, one session at a time. Two
existing adapters (whisper, nemotron), two new snaps. Qwen-C remains batch-only
(architecture unproven for streaming).

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Status | Notes |
|-----------|--------|-------|
| I. Red-Green TDD | ✅ Pass | All work is Python harness-tier (adapters, strategies, snap servers) — exempt. Rust client is expected untouched; if the strategy work exposes a client gap, that change is TDD. |
| II. Integration-Test Readiness | ✅ Pass | Validation runs against live `myna-server` instances (WAV clip sources, realtime-paced) and confined snaps — same pattern as 007/feature 005. No audio-server dependency for adapter tests. |
| III. Performance Watermarks | ✅ Pass | New emission watermarks (time-to-first-unstable, time-to-first-committed, finalize latency, RTF, peak memory) recorded per backend×tier alongside `results/streaming-tiers.json`; SC gates consume them. |
| IV. Workshop Dev Env | ✅ Pass | New deps (onnxruntime, sherpa-onnx) are Python wheels via `uv` extras; no new system deps. Snap packaging follows the existing per-family snap pattern. |
| V. Privacy-First Offline | ✅ Pass | Bounded in-memory buffers with committed-prefix advancement (long utterances cannot grow memory unboundedly); no audio persistence; unstable text display-only, not logged by default; offline models only. |
| Staged Delivery | ✅ Plan | 5 increments matching user stories, each independently testable (see below). |
| Commit Communication | ✅ | No AI attribution. |

**No violations. No Complexity Tracking entries required.**

### Staged delivery

1. **S1 spike → whisper strategies** (US1): spike validates word-timestamp
   stability; then strategy seam + LocalAgreement default + tail-mutation +
   fixed-head; watermarks.
2. **S2 spike → nemotron native loop** (US2): spike pins the NeMo 2.7.3
   live-feed pattern; then incremental decode path + dial; frame-once watermarks.
3. **Parakeet-class snap** (US3): int8 ONNX adapter (murmure-informed chunked
   commit) + snap packaging.
4. **sherpa-onnx snap + concluding report** (US4): turnkey streaming snap;
   cross-backend report with per-tier recommendation (SC-007).

## Project Structure

### Documentation (this feature)

```text
specs/008-progressive-emission/
├── plan.md              # This file
├── research.md          # Phase 0 output — decisions + spike protocols
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output — validation guide
├── contracts/           # Phase 1 output
│   ├── emission-semantics.md   # invariants every strategy must satisfy
│   └── strategy-config.md      # server/snap strategy-selection surface
└── tasks.md             # Phase 2 output (/speckit-tasks — NOT created here)
```

### Source Code (repository root)

```text
server/src/myna/testbed/
├── whisper.py               # gains streaming path (strategy seam)
├── nemotron.py              # gains incremental cache-aware path
├── streaming/               # NEW: re-decode loop + commit strategies
│   ├── strategies.py        #   local-agreement | tail-mutation | fixed-head
│   └── window.py            #   rolling buffer, committed-prefix advancement
├── parakeet.py              # NEW: int8 ONNX TDT adapter (chunked commit)
└── sherpa.py                # NEW: sherpa-onnx OnlineRecognizer adapter

parakeet-snap/               # NEW snap (CPU tier, int8 ONNX)
sherpa-snap/                 # NEW snap (sherpa-onnx runtime)

results/                     # extended emission watermarks (streaming-tiers.json)
docs/interop/                # concluding streaming report (SC-007)
```

**Structure Decision**: All inference work stays behind the existing adapter
seam (`myna.testbed` `SttService` implementations served by `myna-server`) —
"fix the adapter, not the harness". Re-decode strategies are shared machinery
under `testbed/streaming/` so the strategies share window
management; each strategy only decides *what to commit when*. The two new snaps
mirror the existing per-model-family snap layout (whisper-snap/, nemotron-snap/).
Client (`client/`) is untouched: the 007 FSM already routes dispositions.

## Complexity Tracking

> No constitution violations — table intentionally empty.
