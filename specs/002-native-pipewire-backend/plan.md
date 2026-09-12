# Implementation Plan: Native PipeWire Capture Backend

**Branch**: `002-native-pipewire-backend` | **Date**: 2026-07-15 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `/specs/002-native-pipewire-backend/spec.md`

## Summary

Replace the `pw-record` subprocess capture backend with a native `pipewire-rs`
backend behind the existing `CaptureBackend` seam in `rust/myna-audio`. The new
backend captures live audio in-process (no fork/exec), selects the input node by
stable `node.name`, picks/downmixes specific channel indices on multi-channel
interfaces, and lets the PipeWire graph resample/downmix to the negotiated
`AudioFormat`. It adds a **live** device-enumeration capability (list current
input devices + notify an observer as devices appear/disappear). The consumer
contract (`AudioSource`/`CaptureStream`), the adapter core (`CaptureSource`), the
bounded ring, the stats tap (`AudioStats`), and the `ScriptedBackend` fake
fixture are all reused unchanged. The subprocess backend (`PwRecordBackend`) is
removed once the native backend proves out on hardware; `myna-cli --mic` switches
to the native backend.

## Technical Context

**Language/Version**: Rust (stable, workspace edition 2021, `rust-version = 1.75`)

**Primary Dependencies**: `pipewire` (pipewire-rs, the safe Rust bindings over
libpipewire 0.3 — system libpipewire-0.3 present, v1.6.4), plus the existing
workspace crates: `myna-core` (consumer contract + `AudioFormat`/`PcmChunk`),
`bytes`, `tokio`, `futures-util`, `thiserror`. Device enumeration surfaces via
the PipeWire registry.

**Storage**: N/A — audio lives only in the bounded in-memory ring; nothing on
disk (constitution Principle V, spec FR-013).

**Testing**: `cargo test` (hermetic unit/behavioral suite driven by
`ScriptedBackend` — unchanged) + a hardware/virtual-audio integration suite,
env-gated, exercised against a PipeWire null-sink/loopback graph on the VM and
against real hardware without code changes (constitution Principle II).

**Target Platform**: Ubuntu Desktop (current LTS+), PipeWire primary audio
server, PulseAudio compat maintained.

**Project Type**: Rust workspace library crate (`rust/myna-audio`) + its consumer
(`rust/myna-cli`). Single-project layout.

**Performance Goals**: No capture-path latency or resource regression versus the
subprocess backend beyond declared watermark tolerance (spec SC-008); stop
honored within 250 ms (SC-009, FR-012); dropped-audio duration is 0 in a healthy
session (SC-006). Capture is 1×-realtime; the PCM data rate is ~32 KB/s at the
default 16 kHz mono S16LE.

**Constraints**: Offline-first, no network on the capture path; never persist
audio; produce EXACTLY the negotiated format (backend owns conversion; the
consumer/inference-backend never resample); `push` must never block (callable
from a PipeWire realtime callback) — overflow is the ring's drop-oldest problem.

**Scale/Scope**: One new backend module + one enumeration module in an existing
~7-file crate; one consumer call-site switch in `myna-cli`; removal of one
module. No change to the workspace's other crates' public APIs.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

Constitution v1.3.0. This is a shipped Rust system component, so all principles
apply in full (the Python-harness carve-out does not apply here).

| Principle | Gate | Status |
|---|---|---|
| I. Red-Green TDD (new work post-ratification) | Every behavior-bearing change lands test-first; the native backend and the enumeration API get failing tests before implementation. Contract guarantees (format-exact output, fault-is-one-Err, stop promptness, live enumeration events) encoded as executable tests first. | PASS (planned) |
| II. Integration-Test Readiness on Real Audio Stacks | Backend sits behind the existing `CaptureBackend` trait (swappable); hermetic tests use `ScriptedBackend` with no audio server; the native backend's integration tests run env-gated against a virtual PipeWire graph (null-sink/loopback) on a VM **and** on real hardware with no code change. | PASS (by design) |
| III. Performance Watermarks & Regression Sensitivity | Capture-path watermarks (peak/steady memory, CPU, stop latency, ring occupancy/dropped) recorded as checked-in baselines on the reference environments; a perf test flags drift beyond declared per-metric tolerance. SC-006/008/009 are the measurable targets. | PASS (planned) |
| IV. Workshop-Based Development Environment | libpipewire-0.3 headers + audio tooling (`pw-cli`/`pw-loopback`) + the Rust toolchain MUST be expressible in the Workshop definition. **The repo has no `workshop.yaml` yet** — see Complexity Tracking; this feature adds/introduces the PipeWire dev dependency, so the Workshop definition must gain it in the same PR that introduces the dependency. | GATED — tracked |
| V. Privacy-First, Offline-First Audio | Bounded in-memory ring only; discard on session end; no network; stats tap carries levels/counters, never samples. Unchanged invariants, re-verified for the native path. | PASS (by design) |

**Post-Phase-1 re-check**: see the end of this file — re-evaluated after the
design artifacts; no new violations introduced.

## Project Structure

### Documentation (this feature)

```text
specs/002-native-pipewire-backend/
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── contracts/           # Phase 1 output (Rust API contracts for the crate)
│   ├── capture-backend.md
│   └── device-enumeration.md
├── checklists/
│   └── requirements.md  # from /speckit-specify
└── tasks.md             # /speckit-tasks output (NOT created here)
```

### Source Code (repository root)

```text
rust/
├── myna-core/                 # UNCHANGED consumer contract (AudioSource, CaptureStream,
│   └── src/                   #   CaptureError, StopHandle, AudioFormat, PcmChunk)
├── myna-audio/
│   ├── Cargo.toml             # + pipewire dependency
│   ├── src/
│   │   ├── lib.rs             # re-exports: -PwRecordBackend, +PipeWireBackend, +device enum API
│   │   ├── backend.rs         # UNCHANGED seam (CaptureBackend/CaptureSpec/Producer)
│   │   ├── source.rs          # UNCHANGED adapter core (CaptureSource)
│   │   ├── ring.rs            # UNCHANGED bounded ring
│   │   ├── stats.rs           # UNCHANGED stats tap
│   │   ├── fake.rs            # UNCHANGED ScriptedBackend fixture
│   │   ├── pipewire.rs        # NEW: PipeWireBackend (native capture)
│   │   ├── devices.rs         # NEW: live input-device enumeration + change observer
│   │   └── pw_record.rs       # REMOVED at end of feature
│   └── tests/
│       ├── adapter.rs         # UNCHANGED drop-in behavioral suite
│       └── pipewire_hw.rs     # NEW: env-gated integration suite (virtual-audio VM + hardware)
└── myna-cli/
    └── src/main.rs            # switch --mic from PwRecordBackend to PipeWireBackend; add device-list flag
```

**Structure Decision**: Single Rust workspace, existing `rust/myna-audio` crate.
The feature is additive-then-subtractive within one crate plus a one-line
consumer switch: add `pipewire.rs` + `devices.rs` behind the untouched
`CaptureBackend` seam, prove them, then delete `pw_record.rs`. No new crate — the
enumeration API is small and PipeWire-specific and belongs with the native
backend it shares a connection model with.

## Complexity Tracking

> Only rows that need constitutional justification.

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|-------------------------------------|
| No `workshop.yaml` exists yet (Principle IV mandates one) | The constitution mandates a Workshop-defined environment, but the repo predates that principle and none is checked in. This feature introduces the first hard system dependency that *needs* declaring (libpipewire-0.3 dev headers + audio tooling), so it is the natural point to add a minimal Workshop definition covering the Rust toolchain + PipeWire. | Doing nothing violates Principle IV outright; deferring pushes a known-missing artifact past the PR that first needs it. A full multi-SDK Workshop definition is out of scope — the minimal one covering this crate's build+test deps satisfies the gate without over-building. Scoped as a foundational task in tasks.md. |
| Native backend can't be exercised by the hermetic (`ScriptedBackend`) suite | Real PipeWire capture, node/channel selection, and registry enumeration only exist against a running audio server. | Mocking libpipewire would test the mock, not the integration where audio bugs live (Principle II rationale). The env-gated integration suite against a virtual-audio graph is the correct home; hermetic coverage stays on the seam via `ScriptedBackend`. |

## Constitution re-check (post-design)

Re-evaluated after Phase 1 (research + data-model + contracts + quickstart):

- **I. TDD** — both contracts (`capture-backend.md`, `device-enumeration.md`) are
  written as row-per-guarantee test tables, so tests precede code. PASS.
- **II. Integration readiness** — design keeps hermetic coverage on the untouched
  seam (`ScriptedBackend`) and puts real-PipeWire behavior in one env-gated suite
  that runs identically on the virtual-audio VM and on hardware. PASS.
- **III. Watermarks** — quickstart step 5 + SC-006/008/009 pin the capture-path
  baselines and tolerances; a perf test is a planned task. PASS.
- **IV. Workshop** — still the one open gate: the `pipewire` dependency must be
  declared in a `workshop.yaml` (absent today). Tracked in Complexity Tracking
  and scheduled as a foundational task **in the same increment** that adds the
  dependency, per the Tech-Constraints rule. No design change needed; GATED until
  that task lands.
- **V. Privacy/offline** — no new persistence or network; enumeration is
  read-only; stats stay levels/counters. PASS.

No principle is violated by the design; the sole tracked item (IV) is a
known-missing artifact this feature is the correct occasion to add.
