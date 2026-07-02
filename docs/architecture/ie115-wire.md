# IE115 wire dialect — concrete frame contract

**Date:** 2026-07-02
**Status:** Draft for implementation — pins the exact frames so the Python
`myna-server` and the Rust client can speak IE115 end-to-end and give the team
hands-on experience to continue the design discussion (open items flagged
inline).
**Authors:** Claude, with Charles
**Sources:** `IE115-spec.txt` (Farshid, 2026-06-30), `docs/IE115-resolution.md`
(team decisions, 2026-07-01), `docs/architecture/ie115-lifecycle.md`,
`src/myna/core/{events,session,transport_ws}.py` (the internal vocab + current
wire), the review comments `[c]`–`[n]` in `IE115-spec.txt`.

## 0. Why implement it now (and on both ends)

The 2026-07-01 resolution settled IE115 as a **suitable subset of OpenAI's
Realtime Transcription API + additive events**. The mapping from our internal
`myna.core` vocab to that subset is written down (in `events.py`, T36) but has
never been *executed*. Implementing it on both the Python server and the Rust
client — rather than waiting for Ivano's server — gives us:

1. A working IE115 loopback (Rust client ↔ Python server) that is demoable today
   and exercises whisper / nemotron / qwen, so the wire meets a native
   transducer, not just Whisper.
2. Hands-on pressure on the still-open design questions (error taxonomy, the
   overload/lag signal, the model-loading signal's final shape, `include`
   fields, the conversation-item object graph), which paper mapping can dodge and
   code cannot.
3. Real interop when Ivano's IE115 server lands — our client already speaks it.

**This does not replace the internal wire.** `myna.core`'s flat
`transcription.*` vocab stays the semantic core (contract tests unchanged). IE115
is added as a **selectable wire dialect** — a codec that translates flat ↔
OpenAI-nested on each end. That is the wire-agnostic-FSM bet (T40) being cashed
in: the Rust FSM never changes; IE115 is a second `BackendClient` (T43) and, on
the server, a second encode/decode path.

## 1. Dialect selection (how a peer asks for IE115)

Our handshake is already versioned (`myna.core.protocol`, T35): the client
declares itself in the first text frame and the server acks. We reuse that seam
rather than inventing a new negotiation.

- **Internal dialect (today, default):** client sends
  `{"type":"session.start","protocol_version":"1","config":{…}}`; server acks
  `{"type":"session.created","protocol_version":"1"}`.
- **IE115 dialect (new):** client's first frame is an IE115
  `session.update` (below). The server recognises the OpenAI-shaped frame
  (`"type":"session.update"` with a nested `session` object, no
  `protocol_version` field) and answers in IE115 for the rest of the connection.

> **OPEN (selection mechanism).** Two candidate triggers, decide before coding
> step 2: **(a)** *shape-sniff* — dialect inferred from the first frame's type
> (`session.start` = internal, `session.update` = IE115), zero new fields, most
> OpenAI-client-compatible (a stock client that opens and sends `session.update`
> just works); **(b)** *explicit* — a `protocol`/`dialect` token in the handshake
> or WS subprotocol. Recommendation: **(a)** for the PoC (maximises "a real
> OpenAI client connects"), keep the versioned internal handshake untouched
> beside it. Note (a) means the server must emit `session.created` *first* on
> IE115 (see §4 sequence), so it can't wait for the client's first frame to pick
> the dialect — it picks based on whether the client's first frame is
> `session.update` (IE115) vs `session.start` (internal). Resolve this ordering
> when we implement.

## 2. Session configuration — nested (the team kept OpenAI's shape)

The resolution **overruled** our flat-config pushback for compatibility, so we
implement OpenAI's nested `session.audio.input.transcription` envelope. Our flat
`SessionConfig` (`session.py`) maps into it:

| internal `SessionConfig` | IE115 path | notes |
|---|---|---|
| `audio_format.sample_rate_hz` | `session.audio.input.format.rate` | int Hz |
| `audio_format` (PCM16 mono) | `session.audio.input.format.type: "audio/pcm"` | we only accept PCM16 mono; `channels`/`width` are not in IE115's `format` (**OPEN**, §7) |
| `language` | `session.audio.input.transcription.language` | ISO-639-1, `^[a-z]{2}$` |
| `prompt` | `session.audio.input.transcription.prompt` \| top-level `session.prompt` | biasing; kept (resolution). IE115 shows `prompt` at *both* levels — pick one (**OPEN**) |
| `output_language` | — | **out of scope** (translation, resolution); do **not** encode. If set internally, reject or ignore (decide) |
| `timestamp_granularity` | `session.include: ["…segments"]` | opt-in via `include`, default off (resolution "leaning keep") — **not yet a defined include token** (**OPEN**, §7) |
| — | `session.type: "realtime"` | required const by IE115 schema; we emit it, ignore it inbound |
| — | `session.instructions` | s2s system prompt; **not in our subset** — ignore inbound, never emit (translation-via-instructions is scoped out) |
| — | `session.include: ["item.input_audio_transcription.logprobs"]` | confidence opt-in (§6) |

Client `session.update` (minimal, what our Rust client sends):

```json
{
  "type": "session.update",
  "session": {
    "type": "realtime",
    "audio": {
      "input": {
        "format": { "type": "audio/pcm", "rate": 16000 },
        "transcription": { "model": "whisper-base", "language": "en" }
      }
    }
  }
}
```

Server `session.created` (sent first, on connect — the server's *defaults*) and
`session.updated` (ack of a patch) carry the same `SessionConfig` shape. We emit
only the transcription-relevant subtree; we **do not** emit the s2s fields
(`output`, `voice`, `tools`, `output_modalities`, `truncation`, `max_output_tokens`,
`turn_detection`, `obfuscation`, `usage`, `expires_at`, `id`, `object`) — they
are out of our subset. `session.update` is a **merge/patch** onto server defaults
(IE115 §session.update).

> **Note (Hyrum, review comment `[i]`/`[k]`):** keeping the nested s2s-derived
> shape invites clients to set fields we silently ignore. The team accepted that
> cost for compat; implementing it is how we find out if it bites. Log-once on
> an ignored inbound field would surface it without breaking compat (**proposed**).

## 3. Event mapping — internal ↔ IE115 (the codec's whole job)

This realises the mapping already documented in `events.py`. Client→server on
the left of the split, server→client on the right.

### Client → server

| internal | IE115 frame | notes |
|---|---|---|
| `session.start` + `config` | `session.update` | §2; also the dialect trigger |
| binary PCM frame | `input_audio_buffer.append` `{audio: <base64 pcm16>}` **or** a raw WS **binary** frame | frame-type hatch, §5 |
| `session.finish` | `input_audio_buffer.commit` | end-of-utterance; client-driven turn end (no server VAD) |
| (close connection) | (close connection) | aborts, as today |

### Server → client

| internal event | IE115 frame(s) | notes |
|---|---|---|
| `session.created` (ack) | `session.created` (on connect) + `session.updated` (after a patch) | IE115 splits our single ack into two; see §4 ordering |
| `transcription.progress{phase}` | **`STATUS{state}`** additive liveness event | `preparing→loading`, `ready→ready`, `transcribing→transcribing`. **Not** an OpenAI event — additive, agreed 2026-07-01 (§lifecycle). `snippet` (unstable) has no IE115 home; carry it as an optional field on STATUS or drop for the PoC (**OPEN**) |
| `transcription.progress{snippet}` mid-stream | `conversation.item.input_audio_transcription.delta{delta}` — **only once we emit committed deltas** | today we emit *no* committed deltas (commit-on-finalize); our `snippet` is unstable liveness, IE115 `delta` is committed incremental text. Do **not** map snippet→delta (semantics differ). Streaming (T08) fills this in. For now: no `delta` frames; go straight to `completed` |
| `transcription.final{text}` | `conversation.item.input_audio_transcription.completed{transcript}` | one committed utterance segment |
| `transcription.done{text}` | (no direct IE115 frame) | terminal end-of-session. IE115 has no session-level "done" — the *last* `completed` is the transcript. **OPEN**: emit a final `completed` and rely on connection close, or add an additive `session.done`? For commit-on-finalize (one utterance) `completed` == `done` |
| `transcription.error{code,message}` | `error{error:{type,code,message}}` | §6 taxonomy |

**Object-graph fields we must synthesise.** IE115's transcription events require
`item_id` (string) and `content_index` (int) — vestiges of OpenAI's conversation
object graph. Dictation has no conversation. We mint one `item_id` per utterance
(e.g. `item_<random>`), `content_index: 0`. The full OpenAI example also emits
`conversation.item.added` / `conversation.item.done` / `input_audio_buffer.committed`
/ `input_audio_buffer.speech_started|stopped` — **the spec's own event list omits
these** (they appear only in the OpenAI-parity example). Decision for the PoC:
emit the **minimal** set the IE115 schema defines (`session.*`, `input_audio_buffer.append/commit`,
`…transcription.delta/completed`, `error`) + additive `STATUS`; **do not** emit
the conversation.item.added/done or speech_started/stopped noise. Flag if a
stock OpenAI client needs them.

## 4. End-to-end sequence (IE115 dialect, commit-on-finalize)

```
client                                   server
  |------ WS connect --------------------->|
  |<----- session.created (defaults) ------|   IE115 §sequence step 2
  |------ session.update (config) -------->|
  |<----- session.updated (merged) --------|
  |<----- STATUS{loading} -----------------|   additive; may precede/follow updated
  |<----- STATUS{ready} -------------------|   gate: client sends audio only now
  |------ input_audio_buffer.append ------>|   (binary frame OR base64 JSON)
  |------ input_audio_buffer.append ------>|
  |            ... (paced) ...             |
  |<----- STATUS{transcribing} ------------|   optional liveness
  |------ input_audio_buffer.commit ------>|   end of utterance (hotkey release)
  |<----- …transcription.completed --------|   {item_id, content_index:0, transcript}
  |------ WS close ----------------------->|
```

This differs from our internal sequence only in frame names + the
`created/updated` split; the **lifecycle** (residency gate on `ready`, pre-ready
audio drop, commit-drain, terminal error) is identical — that is why the FSM is
untouched. Cross-reference `docs/architecture/ie115-lifecycle.md` §1 for the
async ordering guarantees (STATUS may arrive before or after `session.updated`).

## 5. Audio frames — the frame-type dispatch hatch

The resolution **deferred** binary-vs-base64 "with a hatch". We implement the
hatch on both ends:

- **Binary WS frame** → raw PCM16 bytes, in the negotiated format. What we want
  long-term (audio is the hot path; base64 is ~33% larger + per-chunk
  encode/parse — review comments `[f]`/`[h]`).
- **Text WS frame** `{"type":"input_audio_buffer.append","audio":"<base64>"}` →
  base64 PCM16. What a stock OpenAI client sends; the compat story.

The server accepts either on the same connection (dispatch on WS frame opcode).
The Rust client sends **binary** by default, with a `--base64-audio` flag to
exercise the OpenAI-parity path. This lets us actually measure the CPU/alloc
cost review comment `[h]` asked for (benchmark per-`append` cost, not KB/s) — a
concrete artifact for the design discussion.

**Measured (2026-07-02, T46).** For the dictation chunk (100 ms of 16 kHz mono
s16le = 3200 B PCM), the base64 `append` path costs, per chunk, **1.35× wire
inflation** (3200 B → 4318 B, the base64 33% plus the JSON envelope) and ~16 µs
of CPU (≈9 µs encode + ≈7 µs decode) against ~0 for a raw binary frame — i.e.
≈+11 KB/s and ≈+160 µs/realtime-second on a single session. Negligible for one
dictation stream; real at fleet scale (many concurrent sessions on a shared
inference node). This is the evidence for **binary-default, base64-for-compat**:
keep the hot path raw, offer base64 only for stock OpenAI clients. End-to-end,
both paths produced byte-identical transcripts across whisper / nemotron /
qwen-c (T46), so the choice is purely cost, not correctness.

## 6. Errors and confidence

**Error taxonomy (feeds T31 — still open).** IE115 defines only four codes across
two types:

| type | code | our trigger |
|---|---|---|
| `invalid_request_error` | `unknown_parameter` | client set a field we don't know |
| `invalid_request_error` | `invalid_parameter` | e.g. off-format audio, bad language code |
| `server_error` | `server_error` | inference failure |
| `server_error` | `model_loading` | **contested** — see below |

Our internal codes (`unsupported_audio_format`, `inference_failed`,
`unsupported_protocol_version`, `language_not_supported`, …) must map onto these
four for the IE115 dialect. That mapping is **lossy** — IE115 has no
`unsupported_protocol_version`, no distinction between recoverable and terminal.
Implementing it forces the T31 question: *is four codes enough?* Almost certainly
not (no retryable/terminal axis, no client/server-fault axis beyond the type).
Record the collisions we hit as T31 evidence.

> **`model_loading` as an error is wrong** (review comment `[n]`, and our own
> position): loading is a **liveness property**, not a failure. We surface it as
> `STATUS{loading}` (additive) and **do not** emit `server_error/model_loading`.
> This is a conscious, documented divergence — the exact kind the resolution
> permits (additive event an unaware client ignores). Keep the error *code* in
> the schema for parity, never emit it.

**Confidence / logprobs (`include`).** Optional for the PoC, behind
`session.include: ["item.input_audio_transcription.logprobs"]`. When requested
and the adapter can produce them, attach `logprobs: [{token, logprob, bytes}]` to
`delta`/`completed`. When absent, omit the field. None of our three adapters
expose token logprobs cleanly today → **default: advertise as unsupported, omit
always**; wire the plumbing but leave production to a follow-up. The front-end
contract ("handle present *and* absent") is what we're validating.

## 7. Open questions surfaced (for the design discussion)

Consolidated; each is a thing code will force that paper left vague:

1. **Dialect selection** — shape-sniff vs explicit token (§1). Recommend
   shape-sniff for OpenAI-client compat.
2. **Audio format completeness** — IE115's `format` carries only `rate` + `type`.
   Channels (mono) and sample width (16-bit) are implicit. Where do we assert
   them, and how do we reject stereo/float without a field to name? (ties to T33
   — sample-encoding.)
3. **`prompt` placement** — top-level `session.prompt` vs
   `session.audio.input.transcription.prompt`; IE115 shows both. Pick one.
4. **`done` semantics** — no IE115 session-level terminal event. Rely on last
   `completed` + close, or add additive `session.done`? Matters once a session
   spans multiple utterances (streaming).
5. **STATUS shape** — field name (`state`), value set (`loading|ready|transcribing`),
   and whether `snippet` rides on it. Name is still "TBD" in the lifecycle doc.
6. **Overload / lag signal** (Matias) — same additive-event category as STATUS;
   not yet designed. The append path is where we'd detect "falling behind" — good
   place to prototype it once base wiring lands.
7. **`include` tokens** — only `…logprobs` is defined; segment-timestamps
   `include` token is referenced (resolution) but unnamed. Define it if we do
   timestamps.
8. **Object-graph minimalism** — confirm a stock OpenAI client is happy without
   `conversation.item.added/done` + `input_audio_buffer.committed/speech_*`. If
   not, the "minimal subset" claim needs revisiting.

## 8. Implementation plan (this note is step 1)

1. **This note** — frame contract + open questions. ✅
2. **Python `myna-server`: IE115 codec + selectable dialect.** New module
   (e.g. `myna.core.wire_ie115`) translating flat `myna.core` ↔ IE115 frames;
   `transport_ws` dispatches dialect per §1. Internal vocab untouched. Wire-parity
   test suite mirroring `tests/test_contract.py` (same semantics, IE115 frames). ✅
3. **Rust: IE115 `BackendClient`** (T43) behind the FSM — a second backend, FSM
   and driver unchanged. `--dialect ie115` on `myna-dictate`. ✅
4. **IE115 loopback + demo** — Rust client ↔ Python server over IE115, across
   whisper / nemotron / qwen; the base64-vs-binary append micro-benchmark (§5). ✅
   (all three families, byte-identical transcripts; base64 cost measured — §5.)
5. Fold decisions on §7 back into `IE115-resolution.md` and the spec comments.
