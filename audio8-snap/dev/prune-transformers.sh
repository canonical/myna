#!/bin/bash
# Cut transformers down to the one architecture this snap loads.
# $1 is the site-packages dir, $2 the python to gate with.
#
# The staged engine (asr_onnx_runtime.py) reaches transformers for exactly one
# import, `from transformers import WhisperFeatureExtractor` - a numpy mel
# frontend. Everything else it needs is onnxruntime and tokenizers. The other
# 500-odd architectures under models/ come along at ~80 MB and are never
# touched: transformers resolves them through _LazyModule, so an absent
# directory costs nothing until something asks for that model by name.
#
# The gate runs the frontend rather than importing it, since a lazy re-export
# can import cleanly and still fail once the mel filters are built.
set -euo pipefail

site="$1"
python="$2"
models="$site/transformers/models"

[ -d "$models" ] || {
	echo "prune-transformers: no transformers/models at $models" >&2
	exit 1
}

find "$models" -mindepth 1 -maxdepth 1 \
	! -name whisper ! -name auto ! -name '__init__.py' ! -name '__pycache__' \
	-exec rm -rf {} +

PYTHONPATH="$site${PYTHONPATH:+:$PYTHONPATH}" "$python" - <<'EOF'
import numpy as np
from transformers import WhisperFeatureExtractor

got = WhisperFeatureExtractor()(
    np.zeros(16000, dtype=np.float32), sampling_rate=16000, return_tensors="np"
)["input_features"].shape
if got != (1, 80, 3000):
    raise SystemExit(f"prune-transformers: mel frontend returned {got}")
print("prune-transformers: WhisperFeatureExtractor still produces", got)
EOF
