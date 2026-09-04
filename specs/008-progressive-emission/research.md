# Research: Progressive Streaming Emission

**Date**: 2026-07-27
**Feature**: `specs/008-progressive-emission`

API surface verified locally on this machine: faster-whisper 1.2.1 and
nemo_toolkit 2.7.3 both importable (no GPU on this box — hardware spikes run on
the NVIDIA PC).

## Decision 1: The 007 wire contract is used unchanged — strategies are wire-invisible

**Decision**: No new event types, no new fields, no protocol bump. All three
whisper strategies, the nemotron native loop, and both
new snaps express emission through the existing
`disposition: committed|unstable` + `segment_index` contract with
supersede-most-recent-unstable revision semantics
(`specs/007-streaming-mode/contracts/streaming-wire.md`).

**Rationale**:
- The 007 contract was designed for exactly this: unstable text may be revised
  freely (tail-mutation needs nothing more), committed text is append-only
  (LocalAgreement prefix commit and fixed-head both fit), and
  commit-clears-unstable gives the terminal transition.
- The colleague's snap proved the failure mode of *less* information on the
  wire (restated hypotheses read as committed). We already carry more.
- Strategy is a server-side/operator choice (server flag or snap config);
  clients stay strategy-agnostic — the FSM shipped in 007 needs no change.

**Alternatives considered**:
- A `strategy` field on the wire for observability: rejected — additive but
  pointless for clients; bench harness records strategy out-of-band.
- Per-session client strategy negotiation: rejected — operator/tier decision,
  adds session.setup complexity for no user value.

## Decision 2: Whisper strategies are an in-adapter seam, not a new dependency

**Decision**: Implement a rolling re-decode loop in the whisper adapter with a
`StreamingStrategy` seam and three strategies; default **local-agreement**.
The loop re-decodes the uncommitted window on a cadence (~1 s of new audio),
and the strategy decides what to commit:

- **local-agreement**: decode with `word_timestamps=True`; commit the longest
  prefix of words whose (word, timestamp) sequence agrees with the previous
  pass's hypothesis (Macháček et al.'s LocalAgreement-2 policy, implemented
  in-house against faster-whisper `Word(start, end, word, probability)`).
  Unstable delta = the uncommitted remainder of the current hypothesis.
- **tail-mutation**: WhisperLive-style — commit all complete segments except
  the trailing one; emit the trailing segment as unstable (may be revised
  wholesale). Simplest correct implementation; serves as the fallback if Spike
  S1 fails and as the comparison baseline in the report.
- **fixed-head**: murmure-style VAD/energy chunking — cut the buffer at pauses
  (arm ~15 s, cut on ~500 ms silence, force-cut ~60 s with ~1 s overlap,
  dedupe at merge), decode each finalized chunk once, commit immediately.
  Cheapest compute (no re-decode); coarsest latency.

Buffer discipline: committed-prefix advancement drops audio before the
committed frontier (bounded memory, constitution V); the uncommitted window is
capped (~30 s) with force-commit of the oldest stable prefix beyond that.

**Rationale**:
- faster-whisper 1.2.1 already provides every input the strategies need
  (verified): word timestamps, `no_speech_prob`/`avg_logprob` confidence,
  `vad_filter` with `VadOptions` (Silero) for fixed-head, `chunk_length` /
  `clip_timestamps` windowing. Zero new dependencies for strategies.
- The `whisper_streaming` package (007 research Decision 2's planned vehicle)
  is research-grade code with its own buffering/CLI assumptions; adapting it
  behind our event contract costs more than implementing the (small)
  agreement policy against inputs we control. We keep commit semantics exactly
  matching our append-only invariant.
- Tail-mutation exists regardless: it subsumes the WhisperLive algorithm
  in-adapter (Decision 4) and is the S1 fallback.

**Alternatives considered**:
- Vendoring `whisper_streaming`: rejected (dependency weight, license review,
  contract mismatch).
- Segment-level agreement instead of word-level: kept as S1 fallback — if word
  timestamps prove unstable, agreement compares segment text prefixes.

## Decision 3: Spike S1 — faster-whisper word-timestamp stability (GATE for local-agreement default)

**Decision**: Timeboxed spike (≤ 1 day) before strategy implementation. On the
real corpus (`corpus/english/`, ≥ 10 clips, 8–30 s): for each clip, decode
growing prefixes (2 s, 4 s, … full) with `word_timestamps=True`; between
successive passes measure (a) word-sequence agreement of the overlapping
prefix, (b) timestamp drift per agreed word, (c) how far behind speech the
committed frontier would lag at a 1 s cadence.

**Go / no-go**:
- **Go** (≥ ~90 % of overlapping words agree between adjacent passes, drift
  ≲ 0.3 s): local-agreement ships as default (FR-003).
- **No-go**: default flips to tail-mutation; local-agreement ships behind the
  flag using segment-text-prefix agreement; finding recorded in the report.

**Rationale**: LocalAgreement's commit guarantee is only as strong as the
stability of what it compares. faster-whisper word timestamps are DTW-derived
from cross-attention — known to jitter on re-decode of growing windows. This
is the single assumption the whisper story hinges on; cheap to measure, and it
runs on CPU (no GPU needed).

## Decision 4: WhisperLive subprocess driver — REJECTED (clarification 2026-07-27)

**Decision**: No external WhisperLive driver. The tail-mutation strategy
(Decision 2) implements the same algorithm in-adapter — rolling re-decode,
commit all complete segments except the trailing one, trailing segment as
unstable, stuck-partial escape — so a managed subprocess would add packaging
and failure-mode complexity with no new capability.

**Rationale**:
- WhisperLive's backend heuristic was verified in
  `reference/WhisperLive/whisper_live/backend/base.py` `update_segments` and
  is ~50 lines of policy on top of the same faster-whisper decode we already
  run — trivially reproduced, no embeddable API needed.
- The interop lessons (disposition dropped at the wire, empty-completed-as-
  reset) are already captured in 007's interop report; re-running their
  backend through our edge would re-measure what we already documented.
- One less subprocess lifecycle, one less failure mode, one less snap payload
  (offline invariant).
- Caveat carried forward: the tail-mutation commit guarantee (limited right
  context at commit time) is the weakest of the three strategies — measured
  under SC-002 like any strategy, and called out in the concluding report.

**Alternatives considered** (before rejection): managed subprocess (rejected —
above); library import (rejected — threaded websocket server, not embeddable);
network service (rejected — offline/no-network invariant).

## Decision 5: Nemotron uses NeMo's cache-aware incremental path (no custom backend)

**Decision**: Replace the offline `model.transcribe()` call (streaming mode
only) with NeMo 2.7.3's streaming utilities — verified present:
`CacheAwareStreamingAudioBuffer(model)`,
`FrameBatchASR` (`set_frame_reader`, `transcribe(tokens_per_chunk, delay)`,
`reset`), `BatchedFrameASRRNNT` / `BatchedFrameASRTDT` (the served model is a
hybrid RNNT/TDT). The `att_context_size` dial (already plumbed) stays the
latency/accuracy knob. Unstable partials per step; committed segments at the
decoder's natural hypothesis boundaries; finalize on end-of-audio.

**Rationale**:
- The served checkpoint (`stt_en_fastconformer_hybrid_large_streaming_multi`)
  is cache-aware by construction; frame-once is its native mode. This is the
  only path that satisfies SC-004 (length-independent latency).
- NeMo ships the loop; we don't maintain a decode implementation (contrast
  with murmure, which hand-rolls TDT because it avoids NeMo entirely — that
  trade is examined in Decision 6, not here).

**Alternatives considered**:
- Hand-rolled RNNT step loop (murmure-style): rejected for the full-fat snap —
  duplicates what NeMo already maintains, for no size win (NeMo is already the
  dependency).
- Keep commit-on-finalize + finer sentence split: rejected — doesn't move
  time-to-first-committed (the actual FR-008 gap).

## Decision 6: Spike S2 — NeMo 2.7.3 streaming_utils live-feed pattern (GATE for US2)

**Decision**: Timeboxed spike (≤ 1 day, NVIDIA PC) to pin the exact incremental
feed pattern. Open questions: (a) `FrameBatchASR.transcribe(tokens_per_chunk,
delay)` is shaped for offline *simulation* (it pulls from a frame reader over a
file) — determine the supported pattern for *pushing* live PCM
(`CacheAwareStreamingAudioBuffer.append_audio` + explicit step, or a custom
frame reader fed by our async iterator); (b) partial-transcript extraction per
step and its stability (is the stepwise hypothesis directly emittable as
unstable?); (c) measured finalize latency and per-chunk cost at
`att_context_size` settings ([70,70] vs [70,1]-style) on a 30 s real clip;
(d) whether hybrid TDT decoding needs `BatchedFrameASRTDT` specifically.

**Go / no-go**:
- **Go**: documented push pattern decodes a realtime-fed 30 s clip with
  per-step partials, frame-once cost, ≤ 1 s finalize → implement per Decision 5.
- **No-go** (API only supports file-pull simulation cleanly): fall back to a
  thin custom frame reader adapting our async PCM iterator to NeMo's pull
  interface; if that also fights the API, descope to chunked re-decode with
  the cache-aware model (still better than offline transcribe) and record the
  finding.

**Rationale**: The adapter docstring has called this "the follow-up" since T09;
the API has drifted across NeMo 2.x. One measured day on the target hardware
settles the integration shape before tasks are written against it.

## Decision 7: Parakeet-class snap — int8 ONNX, murmure-informed chunked commit, Python adapter first

**Decision**: New `parakeet-snap` serving Parakeet TDT 0.6B v3 (multilingual,
25-lang) int8 ONNX via onnxruntime, through a new `myna.testbed` adapter
(`parakeet.py`). Emission: fixed-head chunked commit (Decision 2's strategy,
sharing `testbed/streaming/` window machinery) — murmure's constants as
starting points, re-validated on our corpora. Decode: greedy TDT step loop
ported from murmure's Rust (`reference/murmure/src-tauri/src/engine/engine.rs`)
to numpy/onnxruntime — it is itself a port of NVIDIA's reference decoder.

**Rationale**:
- Size: int8 ONNX + onnxruntime is hundreds of MB vs the multi-GB NeMo+torch
  snap (SC-005). CPU-viable — this is the CPU-tier streaming answer.
- Python-first matches the harness tier and the other snaps' `myna-server`
  packaging; a Rust engine (murmure's actual form) is a later optimization,
  not required for the investigation's conclusion.
- Fixed-head (not re-decode): TDT decode is cheap and chunk-final; murmure
  proves the UX on the same model class.

**Alternatives considered**:
- Rust `ort` engine in the snap: rejected for this feature (scope; harness-tier
  Python proves the tier story).
- NeMo export of FastConformer to ONNX ourselves for this snap: Parakeet's
  official export is better tested; our own export is the sherpa snap's job
  (Decision 8), so both export paths get evaluated without duplication.

## Decision 8: sherpa-onnx snap — turnkey streaming recognizer, exported NeMo-family model

**Decision**: New `sherpa-snap` using sherpa-onnx's Python bindings
(`OnlineRecognizer`) with a NeMo-family streaming transducer exported to ONNX
per k2-fsa's export scripts. sherpa's chunked push API + built-in endpointing
map directly: partial results → unstable, endpoint-detected segments →
committed.

**Rationale**:
- This is the "turnkey" arm of the investigation: no decode loop to maintain,
  streaming is the runtime's native mode, footprint is small (SC-005).
- Running it against the same corpora/watermarks gives the concluding report
  (SC-008) a genuine build-vs-adopt comparison for small transducer snaps
  (custom murmure-style adapter vs sherpa runtime).

**Alternatives considered**:
- sherpa-onnx with a Zipformer (icefall) model instead of a NeMo-family export:
  kept as fallback if the NeMo export fights us; the report should compare
  like-for-like model class where possible.
- Skipping this snap (Parakeet snap only): rejected — the user explicitly
  scoped both, and the build-vs-adopt data point is the conclusion's value.

## Decision 9: Emission watermarks extend the existing tier infrastructure

**Decision**: `dev/bench.py` / `dev/matrix.py` record per backend×strategy×tier:
time-to-first-unstable, time-to-first-committed, finalize latency, RTF, peak
memory — alongside `results/streaming-tiers.json` and
`results/streaming-watermarks.json` (007). Tier gating consumes them (FR-010);
SC-001/004/005 are checked from these artifacts.

**Rationale**: 007 built the tier-gate consumers but with watermarks measured
on commit-on-finalize stand-ins; real emission changes the numbers and makes
the gate meaningful for the first time.
