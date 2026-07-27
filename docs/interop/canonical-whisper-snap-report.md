# Interop Report: canonical/whisper-snap ↔ Myna IE115 Streaming

**Date**: 2026-07-27
**Authors**: Myna team (streaming feature 007)
**Audience**: canonical/whisper-snap maintainers
**Status**: Delivered

## Summary

We built a streaming transcription mode with an explicit **committed/unstable
discriminant** on the IE115 wire (`specs/007-streaming-mode/contracts/streaming-wire.md`)
and validated it end-to-end against your `whisperlive-adapter` (Go) + WhisperLive
docker backend. The session surfaced one protocol-level issue that blocks safe
streaming injection, plus the 6 interop gaps previously identified. This report
documents findings with captured session evidence and proposes resolutions.

## The headline finding: deltas are restated hypotheses, not committed segments

### Captured session (2026-07-27)

Client: `myna-dictate --dialect ie115 --base64-audio --ws-path /ws`
Clip: LibriSpeech `2277-149896-0005.wav` (~5.6s)
Adapter deltas received, in order:

```
delta: " Many little"
delta: " wrinkles gathered."
delta: " Many little wrinkles gathered between his eyes."
delta: " Many little wrinkles gathered between his eyes as he contemplated."
delta: " Many little wrinkles gathered between his eyes as he contemplated this."
delta: " Many little wrinkles gathered between his eyes as he contemplated this and his brow moisted."
delta: " Many little wrinkles gathered between his eyes as he contemplated this and his brow moistened."
completed: " Many little wrinkles gathered between his eyes as he contemplated this and his brow moistened."
```

### Why this is a protocol problem

1. **Deltas restate the growing hypothesis.** Each delta re-sends the full
   partial transcript, not an append-only chunk. A client treating deltas as
   committed text injects "Many little wrinkles gathered. Many little wrinkles
   gathered between his eyes. ..." — duplicated garbage.
2. **Retractions happen.** "moisted" was revised to "moistened" mid-stream.
   Any client that committed the earlier delta has wrong text in the field that
   cannot be un-typed.
3. **The wire gives no way to tell.** Without an explicit discriminant, the
   client must guess from payload shape — the fragile heuristic anti-pattern
   we documented as gap #3.

### Proposed resolution

Add `"disposition": "committed" | "unstable"` to `…transcription.delta`
(additive — old clients ignore it; full contract in
`specs/007-streaming-mode/contracts/streaming-wire.md`):

- WhisperLive hypothesis revisions → `disposition: "unstable"` (client shows as
  transient or discards; each unstable delta supersedes the previous one for
  the same `item_id`).
- Text that will never be revised → `disposition: "committed"` (client injects
  immediately; append-only guaranteed).
- Absent field defaults to `committed` (backward-compat: today's adapters are
  batch-degenerate, where this default is correct).

Our client implements this today and falls back to "all deltas committed"
against disposition-less servers — which is exactly the degraded mode your
adapter currently triggers. With the field populated, both clients behave
correctly.

## The 6 interop gaps (updated with session evidence)

### Gap 1 — Endpoint path
Our client requires `--ws-path /ws`; the adapter serves only there. **Proposal:**
standardize on `/ws`, or serve both `/` and `/ws`, or make the path
discoverable. Low stakes but a real friction point for every new client.

### Gap 2 — Binary frame support
The adapter requires base64 `input_audio_buffer.append` (rejects raw WS binary).
Measured cost of base64: 1.35× wire inflation, ~16 µs/chunk encode. Negligible
per session, but **binary is trivial to accept** (frame-type sniff) and keeps
the door open for leaner clients. **Proposal:** accept both, like our server.

### Gap 3 — Empty-completed-as-reset
The adapter sends `…completed` with an empty `transcript` as a revision-reset
signal. This overloads payload emptiness with semantics and is the same class
of fragility as the delta issue above: our client must special-case "empty
completed = ignore". **Proposal:** superseded by the `disposition` field —
unstable deltas carry revisions; `completed` is always the terminal (never
empty in a successful session).

### Gap 4 — model.loaded/unloaded ↔ STATUS alignment
Both vocabularies exist for model liveness. Our client maps
`model.loaded → ready` and `model.unloaded → preparing`, but the two signals
can interleave confusingly (unloaded arriving after session created).
**Proposal:** converge on one liveness vocabulary — either our additive
`STATUS{state: loading|ready|transcribing}` or your `model.loaded/unloaded`,
not both.

### Gap 5 — session.update unconditional reload
`SetConfig` reloads the backend even when config is unchanged, killing the
connection and losing buffered audio. Our client now skips `session.update`
when it has nothing to say, but a client that genuinely wants to set
language/model mid-connection still loses audio. **Proposal:** diff before
reload — no-op when config is unchanged.

### Gap 6 — session.created timing / liveness during load
On a cold backend the greeting arrives before the model is ready, with no
signal in between; a client gating audio on readiness has nothing to wait on.
**Proposal:** emit liveness during load (our `STATUS{loading} → STATUS{ready}`
sequence), so clients can gate audio on model residency rather than racing.

## What we verified works

- Full session round-trip: connect → base64 audio → deltas → completed → close ✓
- Model/language negotiation (`--allowed-models`, `--allowed-languages`) ✓
- Idle-unload / reload cycle (model.unloaded → model.loaded mapping) ✓
- Transcript quality on real speech: WhisperLive `small` produced a
  recognizable transcript ("moisted"→"moistened" self-correction visible in
  the hypothesis stream) ✓

## Ask

1. Review the `disposition` proposal (contract doc linked above) — it's
   additive, so adoption is non-breaking.
2. Confirm whether WhisperLive's delta stream can distinguish "hypothesis"
   from "confirmed" text; if not, marking all deltas `unstable` and only
   `completed` as committed is already a safe improvement.
3. Pick a direction on gap 4 (liveness vocabulary) so both implementations
   converge.

## References

- Streaming wire contract: `specs/007-streaming-mode/contracts/streaming-wire.md`
- Feature spec: `specs/007-streaming-mode/spec.md` (FR-004, FR-005, FR-006)
- Session capture: this report, "Captured session" above (2026-07-27,
  adapter logs at /tmp/adapter.log, backend at /tmp/docker.log)
