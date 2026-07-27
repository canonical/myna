# Data Model: Progressive Streaming Emission

**Feature**: `specs/008-progressive-emission`

Wire-visible event shapes are unchanged from 007
(`specs/007-streaming-mode/contracts/streaming-wire.md`); this document covers
only the new server-side entities.

## StreamingStrategy

A named emission policy for re-decode/chunk-based adapters. Server-selected
(server flag or snap config); never on the wire.

| Field | Type | Notes |
|---|---|---|
| `name` | enum: `local-agreement` \| `tail-mutation` \| `fixed-head` | Default `local-agreement` (subject to Spike S1 gate) |
| `cadence_seconds` | float | How often the re-decode loop runs (~1.0 s of new audio) |
| `window_cap_seconds` | float | Max uncommitted audio window (~30 s); beyond it, force-commit the oldest stable prefix |
| `overlap_seconds` | float | Audio tail carried across a forced cut (~1.0 s; fixed-head) |
| `commit_rule` | (see contracts/emission-semantics.md) | The only thing strategies actually vary |

**Validation rules**
- Strategy fixed for the life of a service instance; no mid-session change.
- `cadence_seconds` > 0; `window_cap_seconds` ≥ 5; `overlap_seconds` < `window_cap_seconds`.

## StreamingSessionState (per active session, in-memory only)

| Field | Type | Notes |
|---|---|---|
| `strategy` | StreamingStrategy | Resolved at session start |
| `uncommitted_window` | bounded PCM buffer | Audio after the committed frontier; dropped as the frontier advances (constitution V) |
| `committed_frontier_seconds` | float | Audio time up to which text is committed |
| `last_hypothesis` | text + word/segment timestamps | Previous pass's output; LocalAgreement's comparison input |
| `unstable_outstanding` | bool | Whether an uncleared unstable delta is on the wire (end-of-audio MUST resolve it — see edge cases) |
| `segment_index` | int | Monotonic committed-segment counter (007 contract) |

**State transitions** (per emission tick):

```text
(new audio ≥ cadence) → re-decode uncommitted window
  → strategy.commit_rule(last_hypothesis, current_hypothesis)
      → Some(prefix): emit committed (segment_index++), advance frontier,
        drop audio before frontier, clear unstable_outstanding
      → None: no-op
  → emit unstable (remainder of current hypothesis), unstable_outstanding = true
(end-of-audio) → final decode → commit remaining text → emit done
```

## EmissionWatermark (per backend × strategy × hardware tier)

| Field | Type | Notes |
|---|---|---|
| `backend` | string | `whisper` \| `nemotron` \| `parakeet` \| `sherpa` |
| `strategy` | string \| null | re-decode strategies; null for native-streaming backends |
| `tier` | string | hardware tier id (matches `results/streaming-tiers.json`) |
| `time_to_first_unstable_s` | float | SC-001 gate |
| `time_to_first_committed_s` | float | SC-001 gate |
| `finalize_latency_s` | float | end-of-audio → terminal event; SC-004 gate |
| `rtf` | float | tier gate threshold (~1.0, tunable; 007) |
| `peak_memory_bytes` | int | bounded-buffer verification |
| `wer_real` / `wer_streaming_delta_pp` | float | SC-003 gate (≤ 2 pp vs batch) |
| `commit_stability` | float | MUST be 1.0 (SC-002) |

Recorded in `results/streaming-watermarks.json` (007 artifact, extended);
consumed by tier gating and the concluding report.

## BackendSnap (packaging entity, new for the two small snaps)

| Field | Type | Notes |
|---|---|---|
| `name` | `parakeet-snap` \| `sherpa-snap` | model-family snap, mirrors whisper-snap/nemotron-snap layout |
| `runtime` | `onnxruntime` \| `sherpa-onnx` | no torch/NeMo dependency (SC-005) |
| `model_component` | int8 ONNX export | parakeet: official multilingual export; sherpa: NeMo-family transducer exported via k2-fsa scripts |
| `installed_size_bytes` | int | SC-005: ≤ 25 % of full NeMo snap, < 1 GB |
| `target_tier` | string | CPU/edge tiers |
