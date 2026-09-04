# Research: Dual-Mode Streaming Transcription

**Date**: 2026-07-27
**Feature**: `specs/007-streaming-mode`

## Decision 1: Wire representation of the committed/unstable discriminant

**Decision**: Add a `"disposition": "committed" | "unstable"` field to the IE115
`…transcription.delta` event. The field is additive — old clients that don't
recognize it ignore it (additive wire stance, `ie115-wire.md` §1). The
`…transcription.completed` event is always implicitly committed (it's the
terminal). Internal dialect: add a `disposition` field to `TranscriptionFinal`.

**Rationale**:
- A field on the existing event is the minimum-viable change (no new event type).
- Explicit > implicit: the canonical/whisper-snap's use of empty-completed-as-reset
  proved that overloading payload emptiness as a semantic signal is fragile (caused
  interop gap #6).
- Additive: old clients (including today's myna-dictate without the streaming
  feature) ignore the field and continue to work — they already treat every delta
  as committed (which is correct in batch mode, where the only delta IS committed).
- The field name `disposition` avoids collision with `type` (reserved for event
  type) and `status` (ambiguous).

**Alternatives considered**:
- Separate event types (`transcription.committed_delta` vs `transcription.unstable_delta`): more explicit but doubles the event vocabulary; violates additive-compat (old decoders would ignore both new types entirely, losing committed text).
- A `stable: bool` field: considered but `disposition` is more extensible (could later carry `retracted` if revision-retraction is ever needed).
- A `revision_id` field on unstable events (to identify what they supersede): deferred — FR-006 requires identification, but a simple `utterance_id` + sequence offset suffices for now (the unstable text always supersedes the previous unstable emission for that utterance).

## Decision 2: Streaming Whisper — LocalAgreement integration approach

**Decision**: Integrate `whisper_streaming` (the LocalAgreement algorithm by Macháček
et al.) as an optional streaming path inside the existing `FasterWhisperAdapter`.
The adapter exposes a `streaming: bool` constructor flag (defaulting to `False`).
When streaming, the adapter emits committed segments (words/phrases the algorithm
has confirmed stable) as `transcription.final` events with `disposition=committed`
mid-utterance, and a final `transcription.done` at end.

**Rationale**:
- LocalAgreement is the established approach for Whisper streaming (used by
  WhisperLive/Collabora, academic papers, multiple open-source implementations).
- It produces a monotonic committed frontier (once text is confirmed, it's never
  retracted) — matching our append-only contract (FR-005).
- The existing `faster-whisper` CTranslate2 backend supports the repeated-decode
  pattern LocalAgreement needs (decode overlapping windows, compare hypotheses).
- The commit granularity is coarser than Nemotron (segments confirmed after 2-3
  re-decode iterations, not per-frame) — this is expected and acceptable; the
  design note (streaming.md §3) already documents the latency tradeoff.

**Alternatives considered**:
- WhisperLive as a subprocess (Collabora's server): rejected — adds a TCP hop,
  unpinned dependency, and the revision-reset semantics we've identified as
  problematic. We own the committed frontier, not them.
- Whisper with a custom VAD chunker (split on silence, decode each chunk
  independently): simpler but loses cross-chunk context and produces worse WER
  at chunk boundaries. LocalAgreement preserves context.
- Defer Whisper streaming entirely (Nemotron only): rejected — Whisper is the
  most accessible model (CPU-viable, MIT license, multilingual). Users without a
  GPU deserve streaming if their CPU can sustain it.

## Decision 3: RTF gate threshold and assessment mechanism

**Decision**: The tier gate threshold is RTF < 1.0 (the model processes audio
faster than it arrives). Assessment uses the existing `dev/matrix.py` RTF
measurement on the real corpus, recorded as a per-model baseline in
`results/streaming-tiers.json`. At session start, the client checks the active
model's recorded tier against the threshold; if no measurement exists, default
to batch (safe). The threshold and the per-model baselines are configurable
(modelctl / snap config), but the default is static per release (measured once
in CI/lab, shipped as a data file).

**Rationale**:
- RTF is already measured by matrix.py and recorded in bench JSONL. No new
  benchmarking infrastructure needed.
- Static baselines (measured in lab, shipped) avoid runtime measurement overhead
  and nondeterminism. Users don't benchmark; they get a pre-assessed tier.
- RTF < 1.0 is the physical minimum for streaming: if inference is slower than
  real-time, the committed frontier falls further behind with each second of
  speech — latency grows unboundedly.
- Default-to-batch is safe: batch works on all tiers, no degradation.

**Alternatives considered**:
- Runtime RTF measurement (first N seconds of each session): adds latency to
  session start, nondeterministic (varies by utterance content, thermal state),
  and requires a "probing" UX state that doesn't exist.
- VRAM/CPU feature detection (GPU model → tier): too coarse; RTF depends on the
  *model* too (tiny vs large), not just the hardware. Per-model measurement is
  needed.
- User-only selection (no auto gate): rejected — most users can't assess whether
  their hardware supports streaming; the auto-gate is the product (Story 2).

## Decision 4: Mode communication to the client

**Decision**: The server's `session.created` greeting carries an additive field
`"streaming": true|false` indicating whether the backend will emit progressive
committed segments in this session. The client uses this to configure its display
mode (streaming indicator vs processing indicator). If the field is absent, the
client assumes batch (backward-compatible with servers that don't support
streaming).

**Rationale**:
- The server knows the mode (it depends on the adapter + tier configuration).
- session.created is already the place where server capabilities are advertised.
- Additive field: old clients ignore it; new clients on old servers get absent →
  batch (correct, since old servers only do batch).

**Alternatives considered**:
- Client requests streaming in session.update → server accepts/rejects: adds a
  round-trip and a rejection path. Simpler to have the server advertise what it
  will do.
- Capabilities discovery (T24) `streaming: bool`: viable long-term but
  capabilities.query is provisional and not exercised by all clients. The
  session.created field is simpler and session-scoped.

## Decision 5: Unstable text handling (deferred display)

**Decision**: For this feature, unstable text events are decoded by the client but
**discarded** (not injected, not displayed). The wire carries them (FR-006) and the
testbed displays them (with `--show-unstable` flag), but the desktop injector and
the GNOME extension ignore them. Hypothesis display (greyed-out unstable text in
the input field) is a separate follow-up feature gated on UD136 design sign-off.

**Rationale**:
- UD136 review thread is contested: some reviewers explicitly rejected in-field
  hypothesis display ([ag]: "It's been excluded. It is a bad experience"); others
  want it eventually. Engineering can't ship it without design resolution.
- Shipping the wire representation now (disposition field) means the follow-up
  feature is a pure client-side change — no protocol work needed later.
- The testbed flag (`--show-unstable`) lets us measure hypothesis quality without
  user-facing commitment.

## Decision 6: Interop report scope and delivery

**Decision**: A markdown document (`docs/interop/canonical-whisper-snap-report.md`)
covering the 6 protocol gaps, proposed resolutions, and recommendations. Delivered
as a link/attachment in the team's communication channel + a GitHub issue on
`canonical/whisper-snap` referencing the relevant gaps. Scope:

1. Endpoint path: recommend standardizing on `/ws` or making it discoverable.
2. Binary frame support: recommend accepting binary PCM alongside base64 (our §5
   evidence: 1.35× wire inflation, ~16 µs/chunk — negligible per session but why
   pay it when binary is trivial).
3. Empty-completed-as-reset: recommend an explicit revision mechanism (disposition
   field or a dedicated event) instead of overloading payload emptiness.
4. model.loaded/unloaded → STATUS alignment: propose converging on a single
   liveness vocabulary (our STATUS or their model.loaded, but not both).
5. session.update reload: flag as a bug (SetConfig unconditionally reloads even
   when config unchanged); recommend a diff check before reload.
6. session.created timing: recommend separating "connection accepted" from "model
   ready" (liveness signal during load — our STATUS{loading} approach).

**Rationale**: These are real integration failures discovered empirically (6 code
fixes needed in our client). Feeding them back serves both teams: they get bug
reports + protocol proposals; we get alignment toward a shared streaming wire.
