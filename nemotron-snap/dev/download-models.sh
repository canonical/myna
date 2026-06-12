#!/bin/bash
# Fetch the FastConformer .nemo checkpoint into components/ so `snapcraft pack`
# can ship it as the model-streaming-multi component. Resumable; skips if present.
#
# NOTE: model.yaml hardcodes the .nemo filename
# (stt_en_fastconformer_hybrid_large_streaming_multi.nemo). If the repo names it
# differently, update both. Run this before packing.
set -euo pipefail

snap_dir="$(cd "$(dirname "$0")/.." && pwd)"
dest="$snap_dir/components/model-streaming-multi"
export HF_HOME="${HF_HOME:-$snap_dir/.hf-cache}"

model="nvidia/stt_en_fastconformer_hybrid_large_streaming_multi"

if ! command -v hf >/dev/null 2>&1; then
    echo "error: 'hf' CLI not found. Install with: uv tool install 'huggingface_hub[cli]'" >&2
    exit 1
fi

if ls "$dest"/*.nemo >/dev/null 2>&1; then
    echo "checkpoint already present in $dest — skipping"
    exit 0
fi

echo "downloading $model (.nemo) -> $dest"
hf download "$model" --local-dir "$dest" --include "*.nemo"
echo "done. checkpoint:"
ls -lh "$dest"/*.nemo
