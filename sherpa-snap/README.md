# sherpa-snap — sherpa-onnx streaming inference snap (UbuSTT, 008 US4)

The turnkey small streaming snap: a NeMo-family streaming FastConformer
transducer (k2-fsa int8 ONNX export, English, 480 ms latency variant) served
via sherpa-onnx's `OnlineRecognizer` — **native chunked streaming**:
continuous partial hypotheses (unstable) plus endpoint-driven committed
segments, no custom decode loop. ~201 MB installed (base + model component)
vs the full NeMo snap's 6.4 GB (SC-005).

## Build

```bash
./dev/prepare.sh            # stage the myna wheel into wheels/
./dev/download-models.sh    # stage components/model-fastconformer-480ms
snapcraft pack
```

## Install (sideload)

```bash
sudo snap install --dangerous \
    ./sherpa_*.snap \
    ./sherpa+model-fastconformer-480ms.comp
sudo snap connect sherpa:hardware-observe
# socket: /var/snap/sherpa/common/run/ubustt.sock (ws+unix session API)
```

Note: this model emits lowercase, unpunctuated text (English only) — a
quality/footprint data point for the 008 concluding report, not a packaging
defect. sherpa-onnx's native module resolves `libonnxruntime.so` from the
baked-in venv (symlink primed at build time; see snapcraft.yaml).
