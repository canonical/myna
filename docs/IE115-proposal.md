# IE115 — review comments & update proposal (T18)

**Date:** 2026-06-17
**Status:** Proposed — for the IE115 spec owner (Farshid)
**Authors:** Claude, with Charles
**Companion:** `docs/IE115-deviations.md` (full per-feature rationale),
`docs/project-plan.md` Workstream F (the implemented reconciliation)

This is the UbuSTT team's response to the IE115 braindump. It is written as a
set of **discrete, anchored comments** so each can be dropped onto the relevant
part of the IE115 doc, with a one-paragraph position up front. Where a point is
already prototyped, the working prior art is named (it is not theoretical).

---

## Position (the one-paragraph version)

**Adopt IE115's direction; strip its lineage.** IE115 is right where it matters:
it ratifies the two pivots we already made against IE114 — **audio-push** (the
client streams PCM up; no `pipewire-node-name`) and **WebSocket** (the forced
consequence: SSE cannot carry a client→server audio stream). We have both
working today (`myna/core/transport_ws.py`, ws+unix, contract-tested). The
problem is that IE115 imports OpenAI's *Realtime* (speech-to-speech, conversational)
object model wholesale into what is only a local, single-user dictation
transcriber. The recommendation is to keep IE115's transport and push-direction
and remove the conversational/cloud machinery, then add the three things a local
service needs that IE115 omits (versioning, capabilities discovery, a
model-loading signal). Each point below is justified by a project invariant or a
measured number, not by taste.

---

## Comments, by IE115 section

### Transport
**Endorse, with one change.** WebSocket over UDS is correct. Drop the TCP
`ws://api.openai.com/v1/realtime` endpoint — we serve `ws+unix` only (local,
no network). Prior art: `transport_ws.py`.

### Audio format — "Sample rate: 24 kHz"
**Push back: do not fix the rate in the spec; negotiate it.** 24 kHz is OpenAI's
TTS-driven rate. Every model we ship wants **16 kHz mono** (faster-whisper,
Nemotron, Qwen3). Hard-coding 24 kHz forces the client to resample to a rate no
adapter wants. The accepted format(s) belong in **capabilities discovery**
(`input_formats`); the client delivers a match and the service rejects
off-format audio rather than resampling (client owns conversion — audio-push
invariant). See also the open audio-encoding question (int16 vs float32, T33).

### Audio transport encoding — "base64 in JSON"
**Push back: send raw binary WebSocket frames, not base64-in-JSON.** Audio is the
hottest path in the system; base64 is ~33% bandwidth overhead plus a JSON
encode/decode per chunk. OpenAI base64s because it multiplexes everything onto
one JSON stream; we have a dedicated binary frame direction and don't need that.
Prior art: binary frames in `transport_ws.py`, already contract-tested.

### Client event — `session.update`
**Keep the mechanism; strip the payload.** Per-session config over the wire is
good. But the example carries the full `gpt-realtime` session object
(`output_modalities`, `voice`, `instructions`, `truncation`, `tools`,
`tool_choice`, `create_response`, `max_output_tokens`). None apply to a
transcribe-only service — keeping them invites clients to set fields we silently
ignore (a Hyrum-law trap). Reduce to a transcription profile: `language`,
`output_language` (translation), `prompt` (biasing), input audio format,
timestamp granularity. We keep this **flat**, not nested under
`session.audio.input.transcription` — that nesting only exists to co-locate the
s2s *output* config we don't have. Prior art: `myna.core.session.SessionConfig`.

### Client events — `input_audio_buffer.append` / `.commit` and server VAD
**Make turn detection client-driven; server VAD optional, off by default.**
IE115 has the server detect `speech_started`/`speech_stopped` and auto-commit.
That is a *conversational* feature (so a cloud AI knows when to respond). Our
primary flow is **push-to-talk dictation**, where the client already owns the
boundary (hotkey down = start, hotkey up = commit). Mandatory server VAD adds
latency, CPU, and a clipped-speech failure mode for no benefit here. Keep an
explicit client commit (`session.finish` in our prototype); offer server VAD as
an opt-in for hands-free clients, never as the assumed default.

### Server events — the `conversation.item` object graph
**Strip it.** `conversation.item.added`/`.done`, `role: "user"`,
`type: "message"`, `content:[{type:"input_audio"}]`, and the five-deep
`conversation.item.input_audio_transcription.{delta,completed,failed}` are
OpenAI's *chat* object model. There is no conversation in dictation — one audio
stream becomes text. Collapse to a flat transcript vocabulary:

| IE115 / OpenAI | UbuSTT |
|---|---|
| `…input_audio_transcription.delta` | `transcription.progress` (liveness; `snippet` is *unstable*, not committed) |
| `…input_audio_transcription.completed` | `transcription.final` (per segment) + `transcription.done` (session end) |
| `…input_audio_transcription.failed` | `transcription.error` (single error channel) |
| `session.created`/`session.updated` | `session.created` ack only (no mid-session reconfig) |

Prior art: `myna.core.events`.

### Server event fields — `obfuscation`, `usage`
**Drop both.** `obfuscation` is OpenAI's token-length side-channel padding for a
public-internet threat model — meaningless on a local UDS. `usage` (token
accounting) is cloud billing telemetry; locally it's noise and brushes against
the "don't log transcription content by default" posture.

### Error event
**Shape is fine; the code set is missing — same gap as IE114.** `{type, code,
message}` is reasonable, but `unknown_parameter` is an example, not an
enumeration. We need a stable taxonomy (terminal vs recoverable, client vs
server fault, retryable). Also: dropping the conversation model collapses
IE115's two error channels (top-level `error` + per-item `failed`) into one.
Tracked as a follow-up (error-code taxonomy); the prototype already emits stable
codes (`unsupported_protocol_version`, `unsupported_audio_format`,
`inference_failed`, `adapter_crash`) that seed it.

### Deviations section — protocol versioning is missing
**Add it — this is the load-bearing omission for "support any future model".**
IE114 had `/v1/` in the path; IE115 dropped it and replaced it with nothing.
Recommend an **in-band** version negotiated in the handshake (not a WS
subprotocol token, so it stays transport-agnostic): the client declares
`protocol_version` in `session.start`; the server acks `session.created`
echoing the version it serves, or returns a terminal
`transcription.error(unsupported_protocol_version)` on mismatch. One number
versions the whole contract (handshake + events + config + capabilities).
**Implemented and tested** in the prototype (`myna.core.protocol`,
`tests/test_protocol_version.py`).

### Missing — capabilities discovery
**Add it.** IE115 has no way for a client to ask what a given snap supports
(models, languages, accepted formats, punctuation, translation) before starting.
For a vendor-neutral API fronting different model families (a multilingual Qwen
snap vs an English-only Nemotron snap), this is a real omission. Prior art:
`capabilities.query` → `Capabilities` (`myna.core.capabilities`).

### Missing — a model-loading signal
**Add a `preparing` phase.** Local models cold-load (measured 0.9–2.2 s for
Whisper, more for NeMo); OpenAI never needs this because the cloud is always
warm. Without a signal the client can't tell "loading model…" from a hang.
Recommend a `phase` field on the liveness event (`preparing` vs `transcribing`),
not a new event and not a state machine. Prior art:
`transcription.progress.phase`.

---

## What IE114 had that IE115 silently drops — decide on purpose

| Feature | Recommendation |
|---|---|
| Segment timestamps (`start`/`end`) | Probably drop for dictation (text injection doesn't use them); keep the request field optional, default off. |
| Confidence `score` | **Keep optional** — useful for low-confidence rejection in noise. |
| `prompt` / biasing | **Keep** — Qwen3 supports it. |
| `output-language` (translation) | **Keep** — both specs have it. |
| `Last-Event-ID` reconnect/replay | Drop — WS has no standard resume; overkill for a local session. |

---

## Still open (flag in the doc, don't block on)

- **Audio sample encoding** (int16 vs float32, and where the one conversion
  lives) — bring to the team (T33).
- **Error-code taxonomy** — enumerate the full set with semantics (T31).
- **Performance contract** — latency SLOs grounded in testbed numbers, per
  hardware tier (pending lab runs).
