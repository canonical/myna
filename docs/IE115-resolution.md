# IE115 — Reconciliation outcome (Workstream F close-out)

**Date:** 2026-07-01
**Status:** Resolved — records the team decisions from the 2026-06-24 and
2026-07-01 syncs. Supersedes the *proposals* in `IE115-deviations.md` (which
remain as the reasoning behind each position).
**Authors:** Claude, with Charles

## Direction (decided 2026-07-01)

IE115 will be **a subset of the OpenAI Realtime Transcription API, chosen to suit
us** — not a clean-room local protocol. Rationale:

1. Reuse existing OpenAI-compatible client implementations against a local server.
2. Backend-location flexibility — local *and* remote (a DGX Spark / a box in the
   basement over the network), not local-only.
3. Attract industry / Intel contribution (Windows/WSL developers).
4. Directive (Mark, via Farshid): go with the industry — extend and tweak, but do
   **not** reinvent standards or spend effort in standards bodies.

**The unlock:** a subset can still *add* things without breaking compatibility,
so long as they are (a) additive server events that unaware clients ignore, or
(b) a separate API. This resolves the earlier "strict subset vs local needs"
tension — the two aren't in conflict.

## How our push-backs landed

`IE115-deviations.md` §refs in brackets.

| Push-back                                                       | Decision (2026-06/07)                                                                                                              | Status                       |
|-----------------------------------------------------------------|------------------------------------------------------------------------------------------------------------------------------------|------------------------------|
| Drop `conversation.item.*` graph → flat `transcription.*` [1.1] | Keep OpenAI's shape for compatibility                                                                                              | **Overruled** (compat)       |
| Flat transcription-only session config [1.6]                    | Keep OpenAI's nested shape; Charles withdrew                                                                                       | **Overruled** (compat)       |
| Negotiate audio format, don't hardcode 24 kHz [1.2]             | Server announces its format + **rejects** off-format; client (audio adapter) owns resampling; 16 kHz is our default                | **Accepted**                 |
| Binary PCM frames, not base64 [1.3]                             | Frame-type dispatch hatch agreed (binary later, JSON/base64 for the PoC); document in Considerations                               | **Deferred w/ hatch**        |
| Server VAD optional, off by default [1.4]                       | Turn detection off by default (already an IE115 deviation)                                                                         | **Aligned**                  |
| Drop `obfuscation` / `usage` [1.5]                              | Not in our subset                                                                                                                  | **Accepted** (out of subset) |
| Model-loading liveness signal [3.2]                             | **Adopted** — "model-ready is always an event"; loading is a lifecycle state, not an error; pre-ready audio dropped (client fault) | **Accepted**                 |
| Capabilities discovery [3.3]                                    | **Adopted** — as a *separate* models/capabilities API (mirrors OpenAI `/models`), not folded into session config                   | **Accepted**                 |
| Confidence / logprobs optional [2]                              | Optional for the PoC (behind `include`); front end must handle present *and* absent                                                | **Accepted**                 |
| `prompt` / biasing kept [2]                                     | Kept                                                                                                                               | **Accepted**                 |
| `output_language` / translation [2]                             | **Out of scope** — transcription only; a future/separate project (post-processing / desktop-agent)                                 | **Deferred (scoped out)**    |
| Segment timestamps optional, default off [2]                    | Leaning keep-for-conformance; opt-in via `include` is compatible                                                                   | **Open (minor)**             |
| Protocol versioning [3.1]                                       | Not discussed 2026-07-01                                                                                                           | **Open**                     |
| Error-code taxonomy (terminal/recoverable/retryable/fault) [4]  | Still undefined                                                                                                                    | **Open (T31)**               |

## New items from the 2026-07-01 sync

- **Overload / lag signal** — Matias wants the API to surface "we're falling
  behind / had to drop a chunk" (Ableton-style indicator). Same additive-event
  category as the liveness event; not yet designed.
- **Multi-model loading** — a backend serving two clients that request different
  models (sizes / quantizations / fine-tunes of one family). Decided
  **optional / implementation-defined**: the `model` field lets a client ask; the
  server chooses to load-a-second or reject. Not required for this project.
- **GPU memory pressure** — Charles flagged that Ubuntu has no graceful signal
  when VRAM is exhausted (screen glitches). With multi-model now optional, this
  needs a memory-pressure story before that feature is built. **Unresolved.**
- **Idle model unload** — supported by inference-snap runtimes that allow it
  (llama.cpp yes, OpenVINO no — upstream feature request); default ~10 min,
  configurable. Matches the prototype's load/unload-on-idle.

## Deliverable produced on this branch

- `docs/architecture/ie115-lifecycle.md` — ASCII state + event diagrams for the
  async session lifecycle (the action item assigned to Charles + Farshid),
  covering model-ready gating, pre-ready audio, mid-stream errors, and
  commit-drain. It surfaces the open items above for the spec edit.

## Ownership going forward

- **Inference snap (server side):** Ivano — packaging an IE115-conformant server,
  Whisper first, CPU-first PoC.
- **Audio adapter (client capture / resampling / VAD):** Matias — status
  unconfirmed as of this close-out.
- **Orchestrator subsystem:** Charles (per JB) — starting now. Note the blockers:
  the spec is not finalized (liveness-event shape, error taxonomy, pre-ready
  handling all open), and the audio-adapter interface it must consume depends on
  Matias's progress. Design against the lifecycle in
  `docs/architecture/ie115-lifecycle.md` and keep the audio-adapter boundary
  behind an interface (per `docs/audio-adapter-api.md`) so it can be stubbed
  until the real adapter lands.
