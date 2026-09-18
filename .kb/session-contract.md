# Preface

Read this document when changing session lifecycle, event semantics, transports, or cross-language compatibility. Consult the implementation and parity tests for exact wire shapes.

Read the top-level `.kb/agents.md` file before continuing below.

# Architecture

The client owns microphone capture and pushes PCM to the inference backend over WebSocket on a Unix socket. Backends do not access audio devices and reject unsupported formats rather than resampling. The one place that resamples is the IE115 dialect edge in the transport: a stock OpenAI client sends 24 kHz, the adapter receives its own rate and never learns the wire's.

A connection may carry multiple committed utterances. For each utterance:

1. The client opens or reuses a session and waits for the backend to become ready.
2. Capture starts on activation, but forwarding is gated until readiness so speech during model loading is retained.
3. The client sends PCM, marks the utterance boundary on release, and waits for one terminal outcome.
4. A commit ends the current utterance, not the connection. The client decides when to close the connection.

Server ingress ends one of two ways: close (an IE115 commit, `session.finish`, or a malformed IE115 frame after its error is sent) drains audio already accepted then ends the stream; abort (client disconnect or clean close, even after a commit or `session.finish`, an adapter exit, or server shutdown) discards whatever is queued, releases a blocked reader and cancels the adapter session wherever it is, so no final transcription runs for a peer that is gone; anything still reading the audio gets `AudioAborted`, never a normal end. The queue bounds the adapter's normalized PCM backlog to 1 MiB per connection regardless of message size - an oversized put is split into whole-frame pieces so nothing waits on room it can never get, and a slow adapter backpressures the reader and, through the socket, the client. websockets' own `max_size` (1 MiB) and `max_queue` (1) bound what sits ahead of that queue.

On the IE115 dialect the server acknowledges `input_audio_buffer.commit` with `input_audio_buffer.committed` when the frame arrives, as OpenAI does, not when the adapter reaches the boundary - so the ack never waits behind the decodes the utterance still owes. The ack names the item the commit closes and promises nothing about its transcript: a client that leaves after it still aborts the utterance. Conversation items are assigned in commit order, so a later utterance's `committed` can precede the previous one's `completed`; every delta and `completed` names its own item and `previous_item_id` chains them. An item is announced when it is minted, never later than the first frame that names it: a `conversation.item.created` follows the `committed` that minted the item (OpenAI's own order), and leads the frames of an item minted without a commit - a streaming adapter transcribing before the client commits, or a terminal after a malformed frame ended the audio. A client that builds the item on the frame announcing it therefore drops no delta, and no terminal ever names an item it was never told about.

PCM framing is whole-sample: a partial sample at the end of one append carries into the next. Only at the utterance boundary (commit, `session.finish`, or disconnect) is a still-incomplete trailing partial sample discarded, with a warning naming its byte count - never padded, never carried into the next utterance.

Backend residency is monotonic within an utterance: once Ready, a later loading/preparing report never re-closes the accept gate or drops audio; only an explicit backend error ends it from there. The client's capture ring absorbs audio it cannot yet forward, and a capture fault (open failure, ring overload) fails the session at once, even before Ready, instead of surfacing only after the backend loads. Abort travels out of band from queued audio and control input on both client and server, so a stalled write or a full ingress queue can never hide an abort or a fault behind backed-up audio. On the wire the client aborts by dropping the socket (EOF, no WebSocket close frame), which the server treats like any disconnect without a commit. The client's `BACKEND_PROGRESS_TIMEOUT` (300 s) arms once capture ends or end-of-audio is queued - never while capture is still live, where the capture ring's overload bound ends a stalled session instead - and any server data frame (even one that maps to no event, but not a WebSocket keepalive ping) or accepted outbound item restarts it; it never bounds utterance length.

Streaming decode inputs stay bounded independent of committed text: the rolling window tracks integer sample watermarks and accepts audio only up to its cap whether or not prior audio produced text; a full window forces its own boundary, processes what it holds, commits what it can, and retires down to its overlap. Batch presentation is the same bounded processing deferred rather than shown live: whisper and funasr batch decode bounded regions (pause cut armed at 30 s, forced cut at 60 s, 65 s window cap) exactly as streaming does, and the adapter accumulates committed regions to present once the audio ends - a pause cut retires without overlap only when the loudest raw VAD frame of its silence run measures at most 0.25x the tracked, peak-held speech level; an unverified pause cut keeps the same 1 s overlap and final-0.5 s hold-back as a forced cut. That same speech level caps the VAD noise floor (at 0.05x) so it cannot drift up inside continuous speech, and VAD framing tiles forward in exact integer samples, so each sample is scanned once and every cut lands on an exact sample. The commit watermark stops at the last word actually committed, not the cut, so a word the region missed is not skipped as already covered. No stage caps utterance duration; only accumulated transcript text grows with it.

Committed transcript text is append-only and must never be retracted. Unstable hypotheses are replaceable UI state and must never be committed as final text. Clients in batch display mode may accumulate committed deltas and publish them at completion.

# Important

- Preserve unknown additive events when compatibility allows; do not make event ordering assumptions beyond the state machine.
- Never send audio before readiness and never silently drop captured speech.
- Keep session parameters on the transcription connection. Provisioning and persistent backend configuration are separate control planes.
- Change Python and Rust contract types together and update their wire-parity tests in the same change.
- The IE115 dialect is a subset of the OpenAI Realtime Transcription API. `server/tests/test_openai_realtime_conformance.py` validates every server frame against the `openai` SDK's models (the pinned SDK version is the spec revision claimed); a new event or field on that wire is either a valid OpenAI one or a declared addition there, and the 16 kHz PCM rate is the only declared deviation.
- Keep exact event names, JSON fields, framing, and compatibility behavior in `server/src/myna/core/`, `client/myna-core/`, and their tests rather than duplicating them here.
