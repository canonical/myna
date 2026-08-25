# sherpa-snap — sherpa-onnx streaming inference snap

Small CPU-tier streaming snap: a NeMo-family FastConformer transducer (k2-fsa
int8 ONNX export, English, 480 ms latency) served by sherpa-onnx
`OnlineRecognizer`.

Streaming is enabled by default: the recognizer emits unstable partials plus
endpoint-driven committed segments. The model is English-only and emits
lowercase, unpunctuated text.

## Build

```bash
./dev/prepare.sh
./dev/download-models.sh
snapcraft pack
```

## Install

```bash
sudo snap install --dangerous \
    ./myna-sherpa_*.snap \
    ./myna-sherpa+model-fastconformer-480ms.comp
```

The snap is CPU-only and does not require `hardware-observe`. Session socket:

```text
/var/snap/myna-sherpa/common/run/ubustt.sock
```
