# parakeet-snap — Parakeet TDT int8 ONNX inference snap (UbuSTT, 008 US3)

The small CPU-tier transducer snap: NVIDIA Parakeet TDT 0.6B v3 (25 languages,
punctuation) as an int8 ONNX export served via onnxruntime — no torch, ~787 MB
installed (base + model component) vs the full NeMo snap's 6.4 GB (SC-005).

Streaming: **chunked progressive commit** (`--streaming`, SilenceCut — pauses
cut utterance-like chunks, each decoded once and committed while you keep
speaking; no partial hypotheses by design). Batch mode on request
(`myna-server --adapter parakeet` without `--streaming`).

## Build

```bash
./dev/prepare.sh            # stage the myna wheel into wheels/
./dev/download-models.sh    # stage components/model-parakeet-int8 (murmure's
                            # robust re-quantization — see dev/fetch_parakeet_onnx.py)
snapcraft pack
```

## Install (sideload)

```bash
sudo snap install --dangerous \
    ./parakeet_*.snap \
    ./parakeet+model-parakeet-int8.comp
sudo snap connect parakeet:hardware-observe
# socket: /var/snap/parakeet/common/run/ubustt.sock (ws+unix session API)
```

Weights: murmure's `parakeet-tdt-0.6b-v3-int8` bundle (CC-BY-4.0). The
`ubustt-socket` slot exposes the session socket to confined clients (the
`myna` orchestrator snap plugs it, T14c).
