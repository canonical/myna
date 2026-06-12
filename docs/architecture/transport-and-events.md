# Transport Abstraction and Event Vocabulary

**Date:** 2026-06-12
**Status:** Accepted (abstraction); the concrete transport and the event set
remain **provisional** pending IE114 updates
**Authors:** Claude, with Charles

## Context

IE114 as approved specifies HTTP-over-UDS with an SSE response, but the design
has moved to an audio-push model: the client captures PipeWire audio and
pushes PCM to the service. SSE is unidirectional and no longer sufficient.
Direction of travel is WebSocket over a Unix domain socket (PCM binary frames
in, JSON events out, one connection), but this is not yet written into the
spec. Separately, the event vocabulary was simplified after feedback that
mid-stream retraction (`partial`/`replace`/epochs) confuses users.

## Decision

### Transport: code to a session contract, not a wire protocol

All clients (harness, desktop orchestrator) use the `SttClient`/`SttSession`
protocols; all services (adapters, the future inference snap) implement
`SttService` (`myna/core/transport.py`). The session lifecycle is fixed:

1. open session with `SessionConfig`
2. push `PcmChunk`s while events arrive concurrently
3. `finish_audio()` signals end of input (hotkey release)
4. service flushes, emits remaining `final`s, then **exactly one terminal
   event** (`done` or `error`), and the event stream ends

Two transports implement this contract:

- `LoopbackClient` — in-process queues, no serialization. Phase 0 fixture and
  the reference for session semantics. Permanent.
- WebSocket-over-UDS client/server — added once IE114 settles; must pass the
  same contract tests (`tests/test_contract.py`) with only wiring swapped.

No code outside `myna/core/transport.py` and the future WS module may mention
sockets, HTTP, SSE, or WebSocket.

### Events: progress / final / done / error, no retraction

`myna/core/events.py` implements the working vocabulary from CLAUDE.md:
`transcription.progress` (liveness, unstable snippet, UI-animation only),
`transcription.final` (committed, never retracted), `transcription.done`
(terminal, full transcript), `transcription.error` (terminal, structured
`code` + `message`). The wire shape `{"event": ..., "data": {...}}` mirrors
IE114's SSE framing so a future transport only frames these objects.

`SessionConfig` mirrors the IE114 request body minus `pipewire-node-name`,
which is obsolete under audio-push (the client owns capture), and adds
`audio_format` so the service can validate/resample.

## Rationale

Alternatives considered for the seam:

- **Code directly to WebSocket now** — the spec hasn't landed; if IE114 picks
  gRPC bidirectional streaming instead (the plausible runner-up), every
  adapter and the harness would churn. The contract seam costs one thin
  module and removes that risk.
- **Keep the approved SSE design** — unidirectional, incompatible with
  audio-push; already superseded in discussion.
- **Abstract at the byte level (pluggable framing)** — over-general; what
  components actually share is session semantics, not framing.

For events, the four-event set is taken as-is from the stakeholder decision
to drop retraction (`partial`/`replace`/epochs); this ADR just makes that
decision executable and testable rather than re-arguing it.

## Consequences

- Swapping SSE→WebSocket (or anything else) touches one module plus new
  contract-test wiring; adapters and harness are untouched.
- "No retraction" is enforceable by test (`test_finals_are_never_retracted`),
  turning a UX decision into a regression check.
- If IE114 reinstates retraction or adds events, change `events.py` and
  CLAUDE.md together and extend the contract tests; nothing else should know.
