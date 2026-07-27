# Contract: Strategy Selection Surface

**Feature**: `specs/008-progressive-emission`

How operators/packagers select streaming behavior. Server-side only; nothing
here crosses the session wire (strategies are wire-invisible, FR-004).

## `myna-server` CLI (extends the 007 `--streaming` flag)

```text
myna-server --adapter whisper --streaming \
    [--strategy local-agreement|tail-mutation|fixed-head]   # default: local-agreement
    [--stream-cadence-s 1.0] [--stream-window-cap-s 30]

myna-server --adapter nemotron --streaming          # native loop; no --strategy
myna-server --adapter parakeet  --streaming         # fixed-head semantics built in
myna-server --adapter sherpa    --streaming         # native recognizer endpointing
```

- `--strategy` is valid only for the whisper adapter; rejected otherwise
  (clear CLI error).
- `--streaming` off ⇒ batch degenerate (I7), all streaming flags ignored.
- Strategy/cadence/window are fixed at process start; no session.update or
  mid-session mutation.

## Snap configuration

Snaps expose the same knobs via `snap set` (mirroring existing snap config
plumbing): `streaming`, `strategy`, cadence/window caps. Parakeet/sherpa snaps
expose only `streaming` (their emission semantics are intrinsic).

## Capabilities advertisement (existing contract, no change)

- `session.streaming` greeting field (007) reports whether the *service* will
  emit progressively; it does NOT name the strategy.
- Client `--mode auto|streaming|batch` behaves as shipped in 007: `auto`
  follows the greeting + tier gate, `batch` forces degenerate mode,
  `streaming` requests progressive emission.
