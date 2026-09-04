# Interop Report: canonical/whisper-snap ↔ Myna IE115 Streaming

**Date**: 2026-07-27
**Authors**: Myna team (streaming feature 007)
**Audience**: canonical/whisper-snap maintainers
**Status**: Delivered 2026-07-27. Re-run 2026-08-20 against adapter HEAD
`8ae643b` - see "Re-run" at the end, which supersedes the gap status above and
adds a new blocking finding.

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

---

# Re-run: 2026-08-20, adapter HEAD `8ae643b`

**Authors**: Myna team
**Adapter under test**: `reference/whisper-snap` at `8ae643b` (2026-07-31,
"Rename snap to whisper-asr"), built from source. Note the `whisper-asr` store
snap at `latest/edge` is older (`2.0.0-beta.12+d769987`) and predates both
`4edfaf3` and this HEAD, so it will not reproduce these results exactly.
**Backend**: Collabora WhisperLive (`reference/WhisperLive`, `faster_whisper`,
model `base`, CPU int8) run directly from a local venv rather than docker.
**Client**: `myna-dictate`, unmodified.

## New blocking finding: `completed` is per-VAD-segment, not per-commit

This supersedes the 2026-07-27 headline as the most severe issue. It makes the
adapter unusable for real dictation.

### What the wire does

Streaming a 67.0s clip (8 LibriSpeech utterances concatenated, natural pauses
between them) with a test client that deliberately ignores `completed` and keeps
streaming to the end:

- 262 frames total
- 122 `…transcription.completed` frames, of which **112 carry an empty
  transcript** (the gap-3 reset signal) and **10 carry a real transcript**
- the client's single `input_audio_buffer.commit` was frame **259**

The 10 non-empty `completed` frames land one per speech segment, at each pause,
long before any commit:

```
completed: " Mr. Quilter is the apostle of the middle classes, and we are glad to welcome his gospel."
completed: " Nor is Mr. Quilter's manner less interesting than his matter."
completed: " He had written a number of books himself among them a history of ..."
...   (10 total, then the commit at frame 259)
```

After each non-empty `completed` the delta stream restarts from scratch for the
next segment (`" Norris"` → `" Nor is Mr."` → `" Nor is Mr. Quilters."` → …),
confirming these are finalized segments rather than the end of anything.

### Why this breaks clients

`docs/architecture/ie115-wire.md` pins `…transcription.completed` as **the
terminal**: one per `input_audio_buffer.commit`, carrying the full utterance
transcript. A client that honours that contract - ours does - ends the session
at the first non-empty `completed`.

Measured consequence on the 67.0s clip: the session ended after ~8s and returned
**5.9s of transcript, 1 segment of 10**. Roughly 91% of the audio was silently
discarded, and the client exited 0 reporting success. On the adapter side this
shows as `close 1006 (abnormal closure)` on the user connection with no
`onCommit` ever received.

This reproduces immediately on a live microphone: the first natural pause ends
the session mid-dictation. It was missed on 2026-07-27 because that session
tested a single 5.6s clip - one utterance with no internal pause - where the
first non-empty `completed` happened to also be the terminal.

### Proposed resolution

The already-proposed `disposition` field resolves this cleanly, because a
finalized segment is precisely a committed delta:

- segment finalization → `…transcription.delta` with
  `disposition: "committed"` (never revised, append-only, safe to inject)
- `…transcription.completed` → emitted **only** in response to
  `input_audio_buffer.commit`, carrying the whole utterance

That single change closes this finding, the 2026-07-27 delta headline, and
gap 3 together, and removes 122 of the 262 frames from the wire.

## Gap status at `8ae643b`

| # | Gap | Status |
|---|---|---|
| 1 | Endpoint path | **Moved, not closed.** The adapter now serves `/v1/realtime` (`openai/server/server.go`), not `/ws`. Clients still need out-of-band knowledge of the path; our `--ws-path` default is still wrong for you. |
| 2 | Binary frame support | **Open.** `server.go` recognises `websocket.BinaryMessage` only to answer it with `invalid_parameter`. `--base64-audio` remains mandatory. |
| 3 | Empty-completed-as-reset | **Open**, and now quantified: 112 of 262 frames in one session. Subsumed by the `disposition` proposal. |
| 4 | `model.loaded`/`unloaded` ↔ `STATUS` | **Unchanged.** The adapter emits `model.loaded` before `session.created`; no `STATUS` vocabulary. |
| 5 | `session.update` unconditional reload | **Open**, and now measured (below). `Client.SetConfig` still calls `reloadBackend()` with no diff. |
| 6 | `session.created` timing / liveness during load | **Open**, and it is what makes gap 5 destructive. |

## Gaps 5 + 6 measured: a language hint costs the first word, or the session

Our client sends `session.update` only when it has something to say. Sending a
language hint (`--language en`) is enough to trigger the reload, which resets
`modelLoaded` exactly as audio starts. With the same clip and model:

- real-time pacing: first ~600ms lost. `"Mr. Quilter is the apostle …"` came
  back as `"The quilter is the apostle …"`.
- faster-than-real-time pacing: **hard failure**. Every audio frame is rejected
  with `no_model_error` / "no model loaded" and the whole session is lost.

So the 2026-07-27 workaround (client skips an empty `session.update`) only holds
while the client has no configuration to express. Any client that genuinely sets
a language or model still loses audio at the head of its first utterance.

Two independent fixes, either of which helps: diff before reload (gap 5), and
emit a readiness signal clients can gate audio on (gap 6).

## What still works

- Full session round-trip over a Unix socket: connect → base64 audio → deltas →
  `completed` → close, with a correct transcript, provided the utterance
  contains no pause and no `session.update` is sent.
- Streaming mode renders progressive text end to end.
- Model/language negotiation via `--allowed-models` / `--allowed-languages`.

## Minor

- After the terminal `completed` the adapter drops the connection without a
  close handshake (observed as 1006). Harmless for us since the terminal has
  already arrived, but a compliant close is cheap.
- Our own stale references to `/ws` (client help text, `ws_unix_ie115.rs`,
  the `interop_canonical` fixture, feature 007 quickstart) were corrected in
  the same change as this report.

## Ask (unchanged in substance, reordered by severity)

1. Move `completed` to commit-only and emit finalized segments as committed
   deltas. This is the blocker.
2. Adopt `disposition` on deltas (additive, non-breaking).
3. Diff before reload on `session.update`, and signal readiness during load.
4. Accept binary PCM frames alongside base64.
5. Pick a direction on the liveness vocabulary (gap 4).
