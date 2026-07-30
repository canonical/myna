# parakeet-snap — Parakeet TDT int8 ONNX inference snap

Small CPU-tier speech-to-text snap: NVIDIA Parakeet TDT 0.6B v3 (25 languages,
punctuation) as an int8 ONNX export served via onnxruntime. No torch; roughly
787 MB installed with the model component.

Streaming is enabled by default: SilenceCut emits committed chunks at pauses.
It does not emit unstable partials.

## Build

```bash
./dev/prepare.sh
./dev/download-models.sh
snapcraft pack
```

## Install

```bash
sudo snap install --dangerous \
    ./parakeet_*.snap \
    ./parakeet+model-parakeet-int8.comp
```

The snap is CPU-only and does not require `hardware-observe`. Session socket:

```text
/var/snap/parakeet/common/run/ubustt.sock
```

## Streaming cadence

Defaults:

- `stream-arm-seconds=15` — audio required before a pause can commit
- `stream-silence-cut-seconds=0.5` — pause length that commits
- `stream-force-cut-seconds=60` — maximum uncommitted window

Example for earlier chunks:

```bash
sudo snap set parakeet stream-arm-seconds=5
sudo snap restart parakeet.server
```

Lower values commit sooner but decode more often and use less right context.
