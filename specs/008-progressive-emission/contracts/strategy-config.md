# Contract: Strategy Selection Surface

**Feature**: `specs/008-progressive-emission`

Operator-facing streaming controls. Server-side only; nothing here crosses the
session wire.

## `myna-server` CLI

```text
myna-server --adapter whisper --streaming \
    [--stream-cadence-s 1.0] [--stream-window-cap-s 30] [--stream-beam-size 1]

myna-server --adapter parakeet --streaming \
    [--stream-arm-s 15] [--stream-silence-cut-s 0.5] [--stream-force-cut-s 60]

myna-server --adapter nemotron --streaming   # native loop
myna-server --adapter sherpa   --streaming   # native recognizer endpointing
```

- Whisper uses local-agreement.
- Parakeet uses SilenceCut chunked commit: no unstable partials; a pause after
  the armed window commits a chunk.
- `--streaming` off is batch mode on every adapter; streaming flags are ignored.
- Values are fixed at process start.

## Snap configuration

Parakeet exposes the SilenceCut knobs through snapd config:

```sh
sudo snap set parakeet \
    stream-arm-seconds=5 \
    stream-silence-cut-seconds=0.5 \
    stream-force-cut-seconds=60
sudo snap restart parakeet.server
```

The packaged defaults are 15 / 0.5 / 60 seconds. Sherpa's endpointing is
runtime-native and has no equivalent cadence knob.

## Capabilities advertisement

- `session.streaming` reports whether the service emits progressively.
- Client `--mode auto|streaming|batch` behaves as in 007: `auto` follows the
  greeting and tier gate; `batch` forces batch display; `streaming` requests
  progressive emission.
