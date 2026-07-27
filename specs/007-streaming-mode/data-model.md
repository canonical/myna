# Data Model: Dual-Mode Streaming Transcription

**Date**: 2026-07-27
**Feature**: `specs/007-streaming-mode`

## Entities

### Disposition (enum)

The committed/unstable discriminant carried on every text event.

| Value | Meaning | Client action |
|-------|---------|---------------|
| `committed` | Text is final, append-only, never retracted | Inject into text field |
| `unstable` | Text is provisional, may be revised or superseded | Display as hypothesis (if enabled) or discard |

Default when absent: `committed` (backward-compatible — today's deltas are all committed).

### TranscriptionSegment

A single text event on the wire, extended with streaming fields.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `text` / `delta` / `transcript` | string | yes | The transcribed text content |
| `disposition` | Disposition | no (default: committed) | Whether this text is inject-safe |
| `item_id` | string | yes (IE115) | Per-utterance identifier (already exists) |
| `segment_index` | integer | no | Monotonic index within the utterance's committed segments (aids ordering) |

### StreamingMode (enum)

The mode selector, persisted as a user setting.

| Value | Behavior |
|-------|----------|
| `auto` | Resolve to streaming or batch based on the active model's tier assessment (default) |
| `streaming` | Force streaming regardless of tier (user accepts potential latency) |
| `batch` | Force batch regardless of tier (user prefers all-at-once) |

Persisted via: dconf key (unconfined) or snap config (confined, via T54's config story).

### TierAssessment

A per-model measurement determining streaming viability.

| Field | Type | Description |
|-------|------|-------------|
| `model` | string | Model identifier (e.g., `whisper-small`, `nemotron-streaming-multi`) |
| `hardware` | string | Hardware identifier from matrix.py provenance (machine/cpu/gpu) |
| `rtf` | float | Real-time factor (< 1.0 = streaming viable) |
| `strategy` | string | `streaming` or `batch` — the measured strategy that produced this RTF |
| `measured_at` | ISO 8601 | When the measurement was taken |

Stored in: `results/streaming-tiers.json` (dev/lab); shipped as a data file in the snap (static per release).

### SessionMode (on session.created)

The server-advertised mode for this session.

| Field | Type | Description |
|-------|------|-------------|
| `streaming` | bool | `true` if the server will emit progressive committed segments; `false` for batch |

Carried in: the `session.created` greeting, additively (absent = false = batch).

## State Transitions

### Client FSM — Streaming Path

```
                        ┌─────────────────────────────────────────┐
                        │           session.created                │
                        │         (streaming: true)                │
                        ▼                                          │
┌──────────┐    ┌──────────────┐    ┌─────────────────┐    ┌─────────┐
│  Idle    │───▶│  Connected   │───▶│   Streaming     │───▶│  Done   │
└──────────┘    └──────────────┘    └─────────────────┘    └─────────┘
                                     │ on delta(committed):
                                     │   accumulate + emit to sink
                                     │ on delta(unstable):
                                     │   discard (or show hypothesis)
                                     │ on completed:
                                     │   emit final, terminal
```

The existing FSM regions (Session × Residency) are unchanged. Streaming adds
behavior *within* the Active+Resident state: committed deltas are emitted
progressively to the TextSink rather than accumulated for a single emit at Done.

### Adapter — Streaming Emission

```
┌──────────────────┐      ┌────────────────────┐      ┌──────────┐
│  Receiving audio │─────▶│  Emitting segments  │─────▶│  Done    │
│  (buffering)     │      │  (committed deltas) │      │  (final) │
└──────────────────┘      └────────────────────┘      └──────────┘
                           │ Nemotron: per-frame
                           │ Whisper: per-stable-chunk
                           │   (LocalAgreement window)
```

## Validation Rules

- `disposition` MUST be either `"committed"` or `"unstable"` (no other values).
- If `disposition` is absent, treat as `"committed"` (backward-compat).
- Committed segments MUST NOT be followed by a revision of the same text (server invariant).
- `segment_index` values MUST be monotonically increasing within an utterance's committed segments.
- `item_id` MUST be consistent across all segments of the same utterance.
