# Implementation Plan: Dual-Mode Streaming Transcription

**Branch**: `007-streaming-mode` | **Date**: 2026-07-27 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `specs/007-streaming-mode/spec.md`

## Summary

Add a streaming transcription mode alongside the existing batch mode. Streaming
emits committed text segments progressively during speech (append-only, never
retracted); batch remains unchanged (single segment at end). Mode is automatically
selected by hardware tier (RTF gate) and user-overridable. The IE115 wire gains an
explicit committed/unstable discriminant. An interop report feeds the 6 protocol
gaps discovered against the canonical/whisper-snap adapter back to the colleagues.

## Technical Context

**Language/Version**: Rust (stable, 2024 edition) for the client; Python 3.12 for
the server/adapters (evaluation harness tier — exempt from TDD per constitution).

**Primary Dependencies**: tokio, tokio-tungstenite, serde_json (Rust client);
faster-whisper, nemo_toolkit (Python adapters); the existing `myna.core` event/wire
framework.

**Storage**: N/A (in-memory session state only; mode setting persisted via
dconf/snap config, already wired by T54/myna-snap).

**Testing**: `cargo test` (Rust — TDD per constitution); `pytest` (Python — harness
tier, tests optional); `dev/matrix.py` for RTF measurement; integration tests
against live `myna-server` and the canonical/whisper-snap fixture.

**Target Platform**: Ubuntu Desktop (current LTS+) with PipeWire; snapped.

**Project Type**: Desktop application + inference service (split across 2 processes).

**Performance Goals**: Time-to-first-committed ≤ 3 s (Nemotron/GPU), ≤ 5 s
(Whisper/GPU). Streaming WER within 2 pp of batch. RTF gate threshold ~1.0 (tunable).

**Constraints**: No network; no persisted audio; committed text never retracted;
privacy-first (constitution V); offline model only.

**Scale/Scope**: Single-user desktop dictation. One session at a time. Two adapters
(Nemotron, Whisper) gain streaming; Qwen-C deferred (streaming not yet proven for
its architecture).

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Status | Notes |
|-----------|--------|-------|
| I. Red-Green TDD | ✅ Pass | All new Rust code (client streaming FSM, wire decoder changes, mode setting) is TDD. Python adapter streaming is harness-tier — exempt. |
| II. Integration-Test Readiness | ✅ Pass | Integration tests run against: (a) our myna-server with streaming adapters on a WAV source, (b) the canonical/whisper-snap fixture (T63). Both runnable on a VM or hardware. Backend behind `BackendClient` trait (already swappable). |
| III. Performance Watermarks | ✅ Pass | New metrics: `time_to_first_committed`, `commit_stability` (from streaming.md); RTF baselines from `dev/matrix.py` drive the tier gate. Watermarks recorded per model×tier. |
| IV. Workshop Dev Env | ✅ Pass | No new system deps beyond what Workshop already provides (Rust, PipeWire, Python+uv). Streaming adapters use the same model deps (whisper/nemotron extras). |
| V. Privacy-First Offline | ✅ Pass | No change to audio handling (bounded in-memory, never persisted). Committed text on the wire is the same sensitivity as today's `final`. The D-Bus publisher stays content-free (state + level only). |
| Staged Delivery | ✅ Plan | See branch staging below — 4 increments, each independently testable. |
| Commit Communication | ✅ | No AI attribution. |

**No violations. No Complexity Tracking entries required.**

## Project Structure

### Documentation (this feature)

```text
specs/007-streaming-mode/
├── plan.md              # This file
├── spec.md              # Feature specification
├── research.md          # Phase 0: protocol design decisions
├── data-model.md        # Phase 1: entities & state
├── quickstart.md        # Phase 1: validation guide
├── contracts/           # Phase 1: wire protocol contract
│   └── streaming-wire.md
├── checklists/
│   └── requirements.md
└── tasks.md             # Phase 2 (speckit-tasks)
```

### Source Code (repository root)

```text
# Wire protocol & events (shared contract)
server/src/myna/core/
├── events.py            # Add: committed/unstable discriminant on Progress/Final
├── wire_ie115.py        # Add: encode/decode the discriminant field
└── streaming.py         # NEW: streaming session state (segment accumulator)

# Adapters (gain streaming mode)
server/src/myna/testbed/
├── whisper.py           # Add: LocalAgreement streaming emission path
└── nemotron.py          # Add: native transducer streaming (frame-by-frame commit)

# Rust client (consumer)
client/myna-core/src/
└── events.rs            # Add: Disposition enum (Committed/Unstable) on events

client/myna-orchestrator/src/
├── backend/
│   └── ws_unix_ie115.rs # Add: decode disposition field; handle unstable events
├── fsm.rs              # Add: streaming-aware state (accumulate committed segments)
└── runner.rs           # Add: emit committed segments progressively to sink

client/myna-cli/src/
└── main.rs             # Add: streaming display (» committed, ~ unstable)

# Testbed & measurement
dev/
├── matrix.py           # Add: streaming strategy axis, time_to_first_committed
└── bench.py            # Add: per-segment timing in JSONL records

# Interop deliverable
docs/
└── interop/
    └── canonical-whisper-snap-report.md  # FR-013 interop report
```

**Structure Decision**: No new crates or major restructuring. Streaming is an
incremental capability added to existing adapters (Python), wire codecs (both
languages), and the client FSM (Rust). The key new abstraction is the
`disposition` field on text events — a single additive field on an existing
frame shape.

## Branch Staging Plan

| Branch | Increment | Test Gates | Merge Order |
|--------|-----------|-----------|-------------|
| `007a-wire-discriminant` | Wire protocol: add `disposition: committed\|unstable` field to delta/completed events (Python codec + Rust decoder); additive, backward-compatible | Hermetic: golden-frame tests (both languages); contract test parity; existing suite green | 1st |
| `007b-streaming-adapters` | Server: Nemotron native streaming + Whisper LocalAgreement streaming behind a mode flag (`--streaming`); emit committed segments progressively | Integration: `dev/bench.py` streaming strategy axis; time_to_first_committed measured; WER within 2pp of batch on real corpus | 2nd |
| `007c-client-streaming` | Client FSM: accumulate committed segments, emit to sink progressively; mode negotiation (session.created carries mode); testbed display | Hermetic: FSM tests for streaming paths; integration: myna-dictate → streaming myna-server round-trip | 3rd |
| `007d-tier-gate-interop` | RTF tier gate (auto/streaming/batch setting); interop report; canonical/whisper-snap alignment | Integration: matrix.py streaming tier; interop test vs canonical/whisper-snap; report delivered | 4th |

