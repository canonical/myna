# Contract: Strategy Selection Surface

**Feature**: `specs/008-progressive-emission`

How operators/packagers select streaming behavior. Server-side only; nothing
here crosses the session wire (strategies are wire-invisible, FR-004).

## `myna-server` CLI (extends the 007 `--streaming` flag)

```text
myna-server --adapter whisper --streaming \
    [--stream-cadence-s 1.0] [--stream-window-cap-s 30] [--stream-beam-size 1]

myna-server --adapter nemotron --streaming          # native loop
myna-server --adapter sherpa    --streaming         # native recognizer endpointing
```

- The whisper commit strategy is **local-agreement**; the 2026-07-28 triage
  (emission-semantics.md) removed the `--strategy` selector along with
  tail-mutation/fixed-head.
- `--streaming` off ⇒ batch degenerate (I7), all streaming flags ignored.
- Cadence/window/beam are fixed at process start; no session.update or
  mid-session mutation.

## Snap configuration

Snaps expose the same knobs via `snap set` (mirroring existing snap config
plumbing): `streaming`, cadence/window caps. Small-transducer snaps expose
only `streaming` (their emission semantics are intrinsic).

## Capabilities advertisement (existing contract, no change)

- `session.streaming` greeting field (007) reports whether the *service* will
  emit progressively.
- Client `--mode auto|streaming|batch` behaves as shipped in 007: `auto`
  follows the greeting + tier gate, `batch` forces degenerate mode,
  `streaming` requests progressive emission.
