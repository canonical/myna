# CLAUDE.md — UbuSTT (myna) project context

Lean context for a coding agent. **`docs/project-plan.md` is the living task
tracker (stable IDs T01–T32) — read it first** for status and next steps.

## What this is

Ubuntu Desktop speech-to-text (dictation): hold a hotkey, speak, transcribed
text is inserted into the focused app. Local, offline, privacy-preserving — no
cloud, no persistent audio.

## Current state (2026-06-14)

- **Testbed**: harness + session contract over two transports (loopback,
  WebSocket-UDS), same contract tests; fake adapter (permanent fixture); WAV +
  live-mic sources; WER/CER metrics; capabilities discovery (T24); bench + matrix
  aggregator (`dev/bench.py`, `dev/aggregate.py`). Two corpora, both regenerated
  (gitignored), not committed: synthetic espeak (`fixtures/`,
  `dev/generate_fixtures.py`) and **real recorded speech** (`corpus/real/`,
  LibriSpeech CC-BY, `dev/fetch_real_corpus.py` — T25). Real WER is trustworthy;
  synthetic WER is plumbing/latency only (Nemotron: 0% real vs 44.6% synthetic,
  same model).
- **Adapters** (built, hardware-verified): faster-whisper (AED) and Nemotron /
  FastConformer (native transducer). Qwen3 not started.
  `myna-server --adapter whisper|nemotron` serves either on a UDS.
- **Snaps**: `whisper-snap/`, `nemotron-snap/` (one per family) — modelctl/IE108,
  weights as components, GPU engines, idle-unload; run in strict confinement.
- **Desktop**: `dev/dictate.py` push-to-talk demo. Session controller + IBus
  injection (T21/T22) not started.

## Invariants (don't violate)

- **Audio-push**: the *client* owns PipeWire capture and pushes PCM; the STT
  service has no microphone access. Design interfaces accordingly. The client
  also owns format conversion: the service advertises accepted `input_formats`
  (capabilities, T24) and **rejects** off-format audio — adapters never resample.
- **Never persist audio; don't log transcription content by default.**
- **Fix the adapter, not the harness.** The harness speaks only the IE114-shaped
  `myna.core` interfaces; all model messiness lives in adapters. The fake adapter
  is a permanent regression fixture.
- **Transport behind an abstraction.** WebSocket-over-UDS is implemented
  (`myna/core/transport_ws.py`; snaps serve `ws+unix`) — keep
  adapters/harness transport-agnostic.

## Transport & events

WebSocket over a Unix socket: PCM binary frames in, JSON events out, one
connection per session.

Event vocabulary (`myna.core.events`; provisional until agreed with team):
- `transcription.progress` — liveness; `phase` is `preparing` (model loading) or
  `transcribing`. Optional unstable `snippet` for UI; never committed text.
- `transcription.final` — committed text for a segment; never retracted.
- `transcription.done` — terminal; full transcript.
- `transcription.error` — terminal; `code` + `message`.

No `partial`/`replace`/epoch retraction (dropped as confusing). Adding an event?
Document it here and flag it provisional.

Discovery: before a session a client may send `capabilities.query` (WS) /
`client.capabilities()` and get a `Capabilities` doc (models, languages,
`input_formats`, punctuation, translation) — `myna.core.capabilities`, T24,
provisional.

## Models

| Model | License | Notes |
|---|---|---|
| Whisper (faster-whisper) | MIT | AED; streaming is bolt-on chunked re-decode (LocalAgreement). CTranslate2. Built + snapped. |
| Nemotron / FastConformer | — | Cache-aware RNNT, *natively streaming* (each frame once), `att_context_size` latency dial, native punctuation, English-only. NeMo. Built + snapped. |
| Qwen3-ASR | Apache-2.0 | vLLM streaming; isolate in adapter. Not started. |

Key distinction: native transducer (Nemotron) vs AED re-decode (Whisper) drives
streaming latency / partial churn. The Open ASR Leaderboard (batch WER) can't
answer dictation-quality questions — the testbed exists to fill that gap.

Model cache: `HF_HOME` fixed dir; `hf download` (resumable); verify offline with
`HF_HUB_OFFLINE=1`.

## Environment & conventions

- Tooling `uv`; GPU CUDA; PipeWire audio. Extras: `whisper`, `nemotron`.
- New spec artifacts: plain text, IE114/UD129 house style. Design notes in
  `docs/asr-inference-snap-design.md` + `docs/architecture/`.

## Open questions (plan workstream E)

Error model / stable codes (T31); performance contract / latency SLOs (needs T12 lab runs);
residency default policy (T29).
