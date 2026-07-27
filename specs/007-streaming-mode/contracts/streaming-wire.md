# Wire Contract: Streaming Transcription Events (IE115 dialect)

**Date**: 2026-07-27
**Feature**: `specs/007-streaming-mode`
**Amends**: `docs/architecture/ie115-wire.md` §3 (event mapping)

## Additive field: `disposition`

Added to `conversation.item.input_audio_transcription.delta` events. Additive —
old clients ignore it (per the unversioned-additive wire stance).

### Delta with disposition (server → client)

```json
{
  "type": "conversation.item.input_audio_transcription.delta",
  "item_id": "item_abc123",
  "content_index": 0,
  "delta": "Many little wrinkles ",
  "disposition": "committed",
  "segment_index": 0
}
```

```json
{
  "type": "conversation.item.input_audio_transcription.delta",
  "item_id": "item_abc123",
  "content_index": 0,
  "delta": "gathered between his eyes",
  "disposition": "unstable"
}
```

### Field definitions

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `disposition` | `"committed"` \| `"unstable"` | No (default: `"committed"`) | Whether this text is append-only (safe to inject) or provisional (may be revised) |
| `segment_index` | integer | No | Monotonically increasing index for committed segments within this utterance. Absent on unstable deltas. |

### Completed (unchanged — always committed)

```json
{
  "type": "conversation.item.input_audio_transcription.completed",
  "item_id": "item_abc123",
  "content_index": 0,
  "transcript": "Many little wrinkles gathered between his eyes as he contemplated this and his brow moistened."
}
```

The `completed` event is always the utterance terminal and is implicitly
`disposition: committed`. It carries the full transcript (concatenation of all
committed segments). Its `transcript` field is never empty in a successful
session — an empty completed is **not** a valid terminal (see interop gap #6:
the canonical/whisper-snap's use of empty-completed-as-reset is a protocol
defect, not a feature).

## Additive field on session.created: `streaming`

```json
{
  "type": "session.created",
  "session": {
    "type": "realtime",
    "streaming": true,
    "audio": {
      "input": {
        "format": { "type": "audio/pcm", "rate": 16000 },
        "transcription": { "model": "nemotron", "language": "en" }
      }
    }
  },
  "protocol_version": "1"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `session.streaming` | bool | No (default: false) | Whether this session will emit progressive committed deltas before completed |

## Internal dialect mapping

| IE115 field | Internal vocab field | Notes |
|-------------|---------------------|-------|
| `delta.disposition` | `TranscriptionFinal.disposition` | enum `Committed` / `Unstable` on the Rust `TranscriptionFinal` / Python `TranscriptionEvent` |
| `delta.segment_index` | `TranscriptionFinal.segment_index` | Optional integer |
| `session.streaming` | `SessionCreated.streaming` | bool on the session greeting |

## Revision semantics (unstable text)

When an unstable delta arrives, it supersedes the **most recent unstable delta**
for the same `item_id`. There is no explicit "retraction" event — each new
unstable delta simply replaces the previous one. A committed delta following an
unstable delta makes the committed text permanent and invalidates the unstable
hypothesis.

Sequence example (Whisper streaming, one utterance):

```
→ delta { disposition: "committed", delta: "Many ",         segment_index: 0 }
→ delta { disposition: "unstable",  delta: "little wrinkles" }
→ delta { disposition: "unstable",  delta: "little wrinkles gathered" }     ← replaces previous unstable
→ delta { disposition: "committed", delta: "little wrinkles ",  segment_index: 1 }  ← commits; unstable cleared
→ delta { disposition: "unstable",  delta: "gathered between" }
→ delta { disposition: "committed", delta: "gathered between his eyes ", segment_index: 2 }
→ completed { transcript: "Many little wrinkles gathered between his eyes ..." }
```

The client's text field at each step (committed-only mode):
1. `Many `
2. `Many ` (unstable discarded)
3. `Many ` (unstable discarded)
4. `Many little wrinkles `
5. `Many little wrinkles ` (unstable discarded)
6. `Many little wrinkles gathered between his eyes `
7. Final: full transcript committed

## Backward compatibility

- Servers that don't support streaming never set `session.streaming` or
  `disposition` — clients see the existing batch behavior unchanged.
- Clients that don't understand `disposition` ignore it — they treat all deltas
  as committed (correct for batch; for streaming, they'll inject everything
  including unstable text, which is degraded but not broken).
- The `segment_index` field is purely informational (ordering aid for display);
  no behavior depends on it.
