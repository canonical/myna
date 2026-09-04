# IE115 — Deviations from OpenAI Realtime API (UbuSTT position)

**Date:** 2026-06-17
**Status:** Proposed (input to Workstream F reconciliation; team-facing)
**Authors:** Claude, with Charles
**Tracks:** plan T34 · feeds T18 (spec update), T35 (versioning), T36 (events),
T37 (session config), T33 (audio format), T31 (error codes)

IE115 (`IE115-spec.txt`) proposes a WebSocket transcription API modelled on
OpenAI's **Realtime API**. IE115 is still a *braindump* — it does not yet
supersede the approved IE114. This note is our position on it: where IE115 is
right, and where it imported OpenAI's speech-to-speech conversational machinery
into what is only a local, single-user dictation transcriber.

IE115's own "Deviations from OpenAI Realtime API" section lists two trivial
deviations (no `Authorization` header, no session-`model` query param). The real
list is below. Each deviation is justified by a project **invariant** (CLAUDE.md)
or a **measured number** (testbed), not by taste.

---

## 0. Why IE115 is the right direction at all

The framing matters before the push-backs land. IE115 is **closer to what we
already built than IE114 is**:

- IE114-as-approved has the *backend* capture from PipeWire
  (`pipewire-node-name` in the request). We rejected that for the **audio-push
  invariant**: the client owns capture and streams PCM up; the service has no
  microphone. IE115's client-pushed `input_audio_buffer.append` is exactly that.
- Audio-push forces a **bidirectional** channel. SSE (IE114's output protocol)
  is server→client only and cannot carry a client→server audio stream.
  WebSocket is the forced consequence, not a preference. IE115 gets this right;
  we already have it in `myna/core/transport_ws.py` (`ws+unix`).

So adopting IE115's transport and push-direction costs us nothing — it ratifies
the pivot. The disagreements are all about the **payload and object model** IE115
inherited from a conversational speech-to-speech protocol.

---

## 1. Deviations — what we strip or change

### 1.1 Drop the `conversation.item` object graph

**IE115:** transcription is wrapped in OpenAI's conversation model —
`conversation.item.added` / `conversation.item.done`, items with
`role: "user"`, `type: "message"`, `content: [{type: "input_audio"}]`, and the
five-deep event names `conversation.item.input_audio_transcription.delta` /
`.completed` / `.failed`.

**Position:** strip it. There is no conversation in dictation — there is one
audio stream becoming text. The conversation/item/role/message graph exists in
the Realtime API because it is a *chat* protocol where transcription is one
content type among many (audio responses, tool calls). For a transcribe-only
service it is pure ceremony.

**Replacement:** our flat `transcription.*` vocabulary
(`myna.core.events`): `progress` / `final` / `done` / `error`. Mapping —
`…transcription.delta` → `transcription.progress` (or a committed-delta stream,
see §3.2); `…transcription.completed` → `transcription.final` (per segment) and
`transcription.done` (session end); `…transcription.failed` →
`transcription.error`.

**Why:** *invariant* — "the harness speaks only the IE114-shaped `myna.core`
interfaces; all model messiness lives in adapters." Importing a chat object
graph into the core interface is the opposite of that. (T36)

### 1.2 Audio format: negotiate, don't hardcode 24 kHz

**IE115:** "Encoding: PCM16 little-endian; Channels: mono; **Sample rate:
24 kHz**."

**Position:** push back. 24 kHz is OpenAI's TTS-output-driven rate. **Every
model we ship wants 16 kHz** — faster-whisper, Nemotron/FastConformer, and the
Qwen3 pure-C runtime all consume 16 kHz mono. Hardcoding 24 kHz in the protocol
would force the client to resample to a rate no adapter wants.

**Replacement:** the rate (and encoding) is **negotiated** via capabilities, not
fixed in the spec. The service advertises accepted `input_formats`
(`myna.core.capabilities`, T24); the client delivers a matching format; the
service **rejects** off-format audio rather than resampling (audio-push
invariant: the client owns conversion).

**Why:** *invariant* (client owns conversion; adapters never resample) +
*measured* (all three shipped adapters are 16 kHz). This is the audio half of
the open **T33** question — IE115's fixed-rate mandate is precisely the
anti-pattern T33 exists to avoid. Settle the *one* wire encoding (s16le today;
f32le is the alternative since every adapter converts int16→float32 anyway) via
capabilities, not a per-model negotiation axis that solves a divergence that
doesn't exist yet.

### 1.3 Binary PCM frames, not base64-in-JSON

**IE115:** `input_audio_buffer.append` carries audio as
`"audio": "<base64_pcm16_bytes>"` inside a JSON event.

**Position:** push back. We send **raw binary WebSocket frames**; the JSON
channel carries only control + events. base64 is ~33% bandwidth overhead plus a
JSON encode/decode on the **hottest path in the system** (every audio chunk, for
the whole session). OpenAI base64s audio because the Realtime API multiplexes
*everything* — audio in, audio out, events — over one JSON event stream. We
have a dedicated binary frame type and a single audio direction; we don't need
the multiplexing tax.

**Replacement:** binary frames in, JSON text frames out — already implemented
and contract-tested in `transport_ws.py`.

**Why:** *measured concern* — audio is continuous and high-frequency; this is
the one place wire efficiency is non-negotiable. Documented as a deliberate
deviation.

### 1.4 Server VAD / auto-commit is optional, not mandatory

**IE115:** the server detects speech boundaries
(`input_audio_buffer.speech_started` / `speech_stopped`) and "commits buffer
automatically … typically triggered automatically after speech stops"
(`server_vad` turn detection).

**Position:** make it optional and **off by default**. Server VAD is a
*conversational* feature: in the Realtime API the server must detect end-of-turn
so the AI knows when to respond. Our primary flow is **push-to-talk dictation**,
where the *client* already owns the boundaries — hotkey down = start, hotkey up
= `session.finish`. Mandatory server VAD adds latency, CPU, and a real failure
mode (clipped speech / false endpointing) for zero benefit in that flow.

**Replacement:** client-driven commit via `session.finish` (already in the
transport). Server VAD remains available for clients that want it (e.g. a
hands-free mode), negotiated, not assumed.

**Why:** *invariant* (audio-push: the client drives the session) + the UD129
push-to-talk model. The client knows the boundary better and earlier than any
server-side VAD.

### 1.5 Drop `obfuscation` and `usage`

**IE115:** delta events carry an `obfuscation` field; `completed` carries a
`usage` token-accounting block.

**Position:** drop both. `obfuscation` is OpenAI's token-length side-channel
padding for their *streaming-over-the-public-internet* threat model — meaningless
on a local UDS. `usage` (audio/text/total tokens) is cloud **billing**
telemetry; locally it's noise, and emitting per-token counts brushes against the
"don't log transcription content by default" posture.

**Why:** *invariant* (privacy posture; local transport) — neither field has a
local consumer.

### 1.6 Strip the speech-to-speech session config

**IE115:** the `session.created` / `session.updated` examples carry the full
`gpt-realtime` session object: `output_modalities: ["audio"]`, `voice: "marin"`,
`instructions: "…witty, friendly AI…"`, `truncation`, `tool_choice`, `tools`,
`max_output_tokens`, `create_response`, output `format`/`speed`.

**Position:** strip to a transcription profile. None of these apply to a
transcribe-only service — they configure an AI *responder*. Keeping them in the
schema invites clients to set fields the service silently ignores (a Hyrum-law
trap).

**Replacement:** a `session.update` carrying only transcription-relevant config:
`language`, `output_language` (translation, kept — see §2), `prompt` (biasing,
kept — see §2), audio `input_formats`, timestamp granularity, and turn-detection
mode (§1.4). This is `myna.core.session.SessionConfig` today. (T37)

---

## 2. What IE114 had that IE115 drops — conscious decisions

IE115's copy from OpenAI silently loses things IE114 specified. Decide these on
purpose, don't lose them by omission.

| Feature | IE114 | IE115 | Decision |
|---|---|---|---|
| Segment timestamps | `timestamp_granularities`, segment `start`/`end` | absent | **Probably drop for dictation** — text injection doesn't use timestamps. Keep the field optional in `SessionConfig`; default off. |
| Confidence `score` | per-segment `score`, reject < −1 | absent | **Keep optional.** Genuinely useful for low-confidence rejection in noise. `Segment.score` already exists in `myna.core.events`. |
| `prompt` / biasing | `prompt` param | `transcription.prompt: null` (expressible) | **Keep.** Qwen3 supports prompt biasing; it's in `SessionConfig`. Ensure it survives the session-config reshape. |
| `output-language` (translation) | yes | yes (`output.language`) | **Keep** — both have it; aligned. |
| Reconnect / `Last-Event-ID` | SSE resume + missed-message replay | absent (WS has no standard resume) | **Drop.** Overkill for a local UDS session; note it. |

---

## 3. Gaps IE115 introduces that we must close

### 3.1 No protocol versioning (Charles's explicit requirement)

Neither spec versions the wire properly. IE114 at least had `/v1/` in the path;
IE115 dropped it (it's WebSocket) and replaced it with **nothing** — there is no
`protocol_version` anywhere in `session.created`/`session.update`. For a
"support any future model" goal this is the wrong place to be loose.

**Position:** negotiate a version in the handshake — client states supported
version(s) in `session.start`/`session.update`, server echoes the agreed version
in `session.created`/`session.updated`. The WebSocket subprotocol token
(`Sec-WebSocket-Protocol: myna.v1`) is the standard alternative. The event
**vocabulary is versioned as a set**, so adding events (we already proved
`progress.phase` round-trips forward/back-compatibly) is a minor bump with
documented compatibility. (T35)

### 3.2 No model-loading lifecycle signal

IE115 has no event for "the model is loading, nothing is happening yet."
OpenAI never needs one — their models are always warm. We have **cold load**
(measured: 0.9–2.2 s for whisper, more for NeMo), so the gap is real and
user-visible.

**Position:** keep our **T26** resolution — `transcription.progress` with a
`phase` field (`preparing` while loading, `transcribing` otherwise). A field on
the liveness we already emit, not a new event and not a state machine (the
audio-push model makes "listening" the client's own business). The client shows
"loading model…" distinctly from a hang. (T36)

### 3.3 No capabilities discovery

IE115 has **no** capabilities query — a client cannot ask what models,
languages, formats, or features a given snap supports before starting. For a
vendor-neutral API meant to front "any future model" (a multilingual Qwen snap
vs an English-only Nemotron snap, different accepted formats), this is the real
omission, not a stylistic one.

**Position:** keep `capabilities.query` (T24) — the client may query a
`Capabilities` doc (models, languages, `input_formats`, punctuation,
translation) before a session. This is the mechanism that makes the API flexible
across models without protocol bumps.

---

## 4. Error model — unchanged gap (T31)

IE115 has two error channels: a top-level `error`
(`{type, code, message}`) and a per-item
`conversation.item.input_audio_transcription.failed`. The *shape*
(`code`/`message`) is fine and matches `TranscriptionError`. But, exactly like
IE114, **IE115 defines no code set** — `"code": "unknown_parameter"` is an
example, not an enumeration. The stable error-code taxonomy (terminal vs
recoverable, client vs server fault, retryable) remains **T31**; IE115 neither
helps nor hurts it. (If we drop the conversation-item model per §1.1, we also
collapse to a single error channel — `transcription.error` — which is simpler.)

---

## 5. Summary table

| # | IE115 (from OpenAI Realtime) | UbuSTT position | Basis |
|---|---|---|---|
| 1.1 | `conversation.item.*` object graph | Flat `transcription.*` vocab | Invariant: core interface, not a chat graph |
| 1.2 | Fixed 24 kHz PCM16 | Negotiate via `input_formats` (16 kHz today) | Invariant + all adapters 16 kHz (T33) |
| 1.3 | base64 audio in JSON | Binary WebSocket frames | Hot-path efficiency |
| 1.4 | Mandatory server VAD / auto-commit | Optional; client-driven `session.finish` | Invariant: client owns boundaries |
| 1.5 | `obfuscation`, `usage` | Drop both | Privacy posture; local transport |
| 1.6 | s2s session config (voice/tools/instructions/…) | Transcription-only config | No local consumer; Hyrum trap |
| 3.1 | No protocol version | Negotiated `protocol_version` / subprotocol | Flexibility for future models |
| 3.2 | No model-loading signal | `progress.phase = preparing` (T26) | Measured cold-load 0.9–2.2 s |
| 3.3 | No capabilities query | Keep `capabilities.query` (T24) | Vendor-neutral discovery |

What we **adopt** from IE115 unchanged: WebSocket-over-UDS transport,
client-pushed audio, per-session `session.update` config mechanism, and
append-only deltas with no retraction (which matches our own simplification).
