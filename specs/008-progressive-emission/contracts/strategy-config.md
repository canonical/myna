# Contract: Strategy Selection Surface

**Feature**: `specs/008-progressive-emission`

Operator-facing streaming controls. Server-side only; nothing here crosses the
session wire.

## `myna-server` CLI

```text
myna-server --adapter whisper --streaming \
    [--stream-cadence-s 1.0] [--stream-window-cap-s 30] [--stream-beam-size 1]

myna-server --adapter parakeet --streaming \
    [--stream-arm-s 15] [--stream-silence-cut-s 0.5] [--stream-force-cut-s 60] \
    [--stream-partial-cadence-s 2.0] [--stream-partial-tail-s 0]
```

- Whisper uses local-agreement.
- Parakeet uses SilenceCut chunked commit: a pause after the armed window
  commits a chunk, and between cuts the uncommitted window is re-decoded every
  `--stream-partial-cadence-s` seconds and emitted as unstable display text
  (`0` shows nothing until the next cut; `--stream-partial-tail-s` caps a tick
  at the last N seconds of the window, `0` being the whole window).
- FunASR advertises no streaming and commits on finalize.
- `--streaming` off is batch mode on every adapter; streaming flags are ignored.
- Values are fixed at process start.

## Snap configuration

Parakeet exposes the SilenceCut knobs through modelctl config (snapd config is
not a second store; see `docs/configuration-api.md` §3.3):

```sh
sudo myna-parakeet.parakeet set \
    stream-arm-seconds=5 \
    stream-silence-cut-seconds=0.5 \
    stream-force-cut-seconds=60
sudo snap restart myna-parakeet.server
```

The packaged defaults are 15 / 0.5 / 60 seconds.

## Capabilities advertisement

- `session.streaming` reports whether the service emits progressively.
- Client `--mode auto|streaming|batch` behaves as in 007: `auto` follows the
  greeting and tier gate; `batch` forces batch display; `streaming` requests
  progressive emission.
