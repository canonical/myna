# Preface

Read this document when changing session lifecycle, event semantics, transports, or cross-language compatibility. Consult the implementation and parity tests for exact wire shapes.

Read the top-level `.kb/agents.md` file before continuing below.

# Architecture

The client owns microphone capture and pushes PCM to the inference backend over WebSocket on a Unix socket. Backends do not access audio devices and reject unsupported formats rather than resampling.

A connection may carry multiple committed utterances. For each utterance:

1. The client opens or reuses a session and waits for the backend to become ready.
2. Capture starts on activation, but forwarding is gated until readiness so speech during model loading is retained.
3. The client sends PCM, marks the utterance boundary on release, and waits for one terminal outcome.
4. A commit ends the current utterance, not the connection. The client decides when to close the connection.

Committed transcript text is append-only and must never be retracted. Unstable hypotheses are replaceable UI state and must never be committed as final text. Clients in batch display mode may accumulate committed deltas and publish them at completion.

# Important

- Preserve unknown additive events when compatibility allows; do not make event ordering assumptions beyond the state machine.
- Never send audio before readiness and never silently drop captured speech.
- Keep session parameters on the transcription connection. Provisioning and persistent backend configuration are separate control planes.
- Change Python and Rust contract types together and update their wire-parity tests in the same change.
- Keep exact event names, JSON fields, framing, and compatibility behavior in `server/src/myna/core/`, `client/myna-core/`, and their tests rather than duplicating them here.
