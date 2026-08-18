# Model component: Audio8-ASR-0.1B bundle + engine source

The staged ONNX bundle and the publisher's engine source live here as the
source for the `model-audio8-onnx` snap component. They are **not** committed
(CC-BY-NC-4.0, GPLv3 boundary — research.md Decision 2); populate before
packing:

```shell
# from the repo root
uv run python dev/fetch_audio8_model.py \
    --profile snap \
    --accept-license "CC-BY-NC-4.0" \
    --target audio8-snap/components/model-audio8-onnx
```

The directory then holds:

```text
asr_onnx_runtime.py     # publisher's ONNX cache engine (staged source)
hotword/                # engine import dependency (hotword trie; unused by us)
LICENSE                 # CC-BY-NC-4.0 (surfaced for the integrator)
model_bundle/           # metadata.json, tokenizer, int8/int4 graphs, weights
```

At pack time the snapcraft `model-components` part routes it into the
`model-audio8-onnx` component, and the adapter loads it via
`AUDIO8_MODEL_DIR` (see models/audio8-asr-0.1b/model.yaml). The fp32 graphs
are excluded (reference-only, ~2 GB — research.md Decision 10).
