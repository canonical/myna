# CLAUDE.md — UbuSTT Project Context

This file gives a coding agent the context needed to work on the UbuSTT project.
Read it fully before writing any code or making any architectural decisions.

---

## What this project is

Ubuntu Desktop speech-to-text (dictation). The user holds a hotkey, speaks, and
transcribed text is inserted into the focused application. Recognition is local,
offline, privacy-preserving — no cloud, no persistent audio.

---

## Transport layer — IN FLUX (do not treat IE114 as settled here)

IE114 currently specifies HTTP over a Unix domain socket with a
`POST /v1/audio/transcriptions` endpoint and a Server-Sent Events (SSE) response.

**This is being superseded.** The design has moved to an audio-push model: the
client captures audio via PipeWire and pushes raw PCM frames to the STT service,
rather than the service pulling from PipeWire itself. SSE is unidirectional and
therefore no longer sufficient.

**Direction of travel: WebSocket over Unix domain socket.**

Rationale:
- Natural evolution of the existing HTTP-over-Unix-socket shape.
- Full duplex: PCM binary frames in, JSON transcript events out, single connection.
- Prior art: the NVIDIA Nemotron LiveKit reference server does exactly this.
- WebRTC is overkill (designed for NAT traversal / peer-to-peer across networks).
- gRPC/HTTP2 bidirectional streaming is a reasonable alternative but adds protobuf
  tooling complexity without meaningful benefit on a local socket.

The exact protocol (WebSocket vs. another bidirectional transport) is not yet
formally decided in the spec. **Do not hardcode assumptions about the transport
in shared interfaces.** Keep the transport behind an abstraction boundary so it
can be swapped without touching adapter or harness logic.

---

## Event vocabulary — IN FLUX (simplified from earlier proposals)

Earlier proposals included `transcription.partial`, `transcription.replace`, and
epoch counters to support mid-stream retraction of provisional text. This has been
**dropped** following feedback that retraction semantics are confusing to users.

**Current working vocabulary:**

| Event | Purpose |
|---|---|
| `transcription.progress` | Lightweight signal that audio is being processed. May carry a short unstable text snippet to animate a progress UI. No accuracy guarantee, no retraction semantics. |
| `transcription.final` | Stable, committed text for this utterance segment. Never retracted. |
| `transcription.done` | End of session. Carries the complete transcript. |
| `transcription.error` | Structured error with `code` and `message`. |

The progress UI should treat `transcription.progress` as "something is happening"
(waveform animation, "listening…" indicator) and snap to real text only on
`transcription.final`. Do not display progress text as committed output.

This vocabulary is still under experimentation. If you add events, document them
here and flag them as provisional.

---

## Architecture overview

Four layers, as defined in UD129:

1. **Dictation session controller** — hotkey handling, session lifecycle, target
   tracking, privacy/security enforcement.
2. **Audio pipeline** — PipeWire capture, resampling, VAD, bounded in-memory
   buffer, chunking for inference. Audio is *never* written to disk.
3. **Inference pipeline** — sends audio to the STT service, receives transcript
   events. Speaks the IE114 API (currently in flux re: transport — see above).
4. **Text output pipeline** — converts transcript events into IBus preedit/commit
   operations. Abstracted so non-IBus backends (Wayland-native) can be added later.

### Candidate-adapter testbed

A separate evaluation testbed is being built to run in a computer lab in Taipei.
Its purpose is to produce the reference hardware tiers both specs assume but do
not yet define, via a matrix sweep of model × hardware × streaming strategy.

Key concepts:
- **Candidate** = a specific (model, engine, streaming strategy) combination.
- **Adapter** = a server wrapping one candidate behind the IE114-conformant endpoint.
- **Harness** = a client that feeds audio, timestamps every received event, and
  writes structured result records with latency and accuracy metrics.
- **The harness speaks only IE114.** All model-specific messiness lives inside
  adapters. Never modify the harness to accommodate a candidate — fix the adapter.
- The **fake adapter** (emits scripted events with configurable timing, no model
  or audio) must be kept permanently as a regression-testing fixture.

Phased implementation plan:
- **Phase 0**: Fake adapter + harness skeleton. Contract verification, no model/audio.
- **Phase 1**: Real audio via virtual PipeWire at real-time rate.
- **Phase 2**: First real adapter in commit-on-finalize mode (Whisper or Qwen3-ASR).
- **Phase 3**: Streaming with real partial/final events.
- **Phase 4**: Second adapter + result aggregation into evaluation matrix.

---

## Model candidates

| Model | License | Size | Notes |
|---|---|---|---|
| Whisper (faster-whisper) | MIT | small→large-v3 | Mature streaming prior art via LocalAgreement/whisper_streaming. AED architecture — streaming is bolt-on chunked re-decode, not native. CTranslate2 backend (`Systran/faster-whisper-*`). |
| Qwen3-ASR | Apache-2.0 | 0.6B / 1.7B | Ships its own streaming wrapper via vLLM. Streaming semantics unclear from experimentation; Linux performance poor without patches. Patching pain expected — isolate inside adapter. |
| NVIDIA Nemotron ASR | — | ~600M | Cache-aware FastConformer-RNNT — *natively streaming transducer*, not AED. Processes each frame exactly once. Built-in latency/accuracy dial via `att_context_size`. Punctuation and capitalisation native. Strong candidate for streaming quality. Runs via NeMo toolkit, no Docker/A100 required. |

**Key architectural distinction**: natively streaming transducer models (Nemotron)
differ fundamentally from AED models (Whisper, Voxtral) requiring bolt-on chunked
re-decode. This matters for streaming latency and partial churn. The Open ASR
Leaderboard measures batch WER and cannot answer dictation-quality questions —
the testbed exists precisely to fill this gap.

Model cache: set `HF_HOME` as a fixed persistent directory. Use
`huggingface_hub[cli]` via `uv tool install` and `hf download` for resumable
downloads. Verify offline capability with `HF_HUB_OFFLINE=1`.

---

## Development environment

- Python tooling: `uv`
- GPU: CUDA
- OS: Linux (Ubuntu)
- Audio: PipeWire (including virtual PipeWire nodes for testbed use)
- Model serving: faster-whisper (CTranslate2), vLLM for Qwen3-ASR, NeMo for Nemotron

---

## Open questions (do not treat as resolved)

- **Transport**: WebSocket direction agreed in principle; not yet formally written
  into IE114. Keep transport behind an abstraction.
- **Event vocabulary**: working set above is provisional; still under experimentation.
- **PipeWire node identification**: node.name vs. object.serial stability tradeoffs
  not yet resolved in IE114.
- **Capabilities discovery**: no API yet for clients to query supported models,
  languages, punctuation support, etc.
- **Error model**: IE114 error responses are incomplete (see spec comments).
- **Performance contract**: latency SLOs (e.g. first event within 500ms of speech
  start) not yet formally specified.

---

## Conventions

- New artifacts are saved as plain text for use with coding agents.
- The audio push model means the *client* owns PipeWire capture; the STT service
  receives pre-captured PCM. Design interfaces accordingly.
- Do not persist audio. Do not log transcription content by default.
