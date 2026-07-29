#!/bin/bash
# Fetch the sherpa-onnx streaming FastConformer transducer into components/
# so `snapcraft pack` can ship it as a snap model component.
#
# Source: csukuangfj/sherpa-onnx-nemo-streaming-fast-conformer-transducer-en-480ms-int8
# (see dev/fetch_sherpa_model.py). Reuses the HF cache copy (hardlinked).
#
#   ./dev/download-models.sh
set -euo pipefail

snap_dir="$(cd "$(dirname "$0")/.." && pwd)"
repo_root="$(dirname "$snap_dir")"
out="$snap_dir/components/model-fastconformer-480ms"

if [ -f "$out/encoder.int8.onnx" ]; then
    echo "model already present at $out — skipping"
    exit 0
fi

cd "$repo_root/server"
cache="$(uv run python -c "
from huggingface_hub import snapshot_download
print(snapshot_download('csukuangfj/sherpa-onnx-nemo-streaming-fast-conformer-transducer-en-480ms-int8'))")"

mkdir -p "$out"
cp -aL "$cache/." "$out/"
rm -rf "$out/test_wavs" "$out/.cache"
echo "component ready at $out"
