#!/bin/bash
# Fetch the FastConformer .nemo checkpoint into components/ so `snapcraft pack`
# can ship it as the model-streaming-multi component.
#
# NOTE: model.yaml hardcodes the .nemo filename
# (stt_en_fastconformer_hybrid_large_streaming_multi.nemo). If the repo names it
# differently, update both. Run this before packing.
#
#   ./dev/download-models.sh
#   MYNA_MODEL_SRC=~/path/to/models ./dev/download-models.sh   # reuse a local
#       # .nemo (hardlinked in, no download) from $MYNA_MODEL_SRC/model-streaming-multi/
set -euo pipefail

snap_dir="$(cd "$(dirname "$0")/.." && pwd)"
dest="$snap_dir/components/model-streaming-multi"

model="nvidia/stt_en_fastconformer_hybrid_large_streaming_multi"

if ls "$dest"/*.nemo >/dev/null 2>&1; then
    echo "checkpoint already present in $dest — skipping"
    exit 0
fi

# Reuse an already-downloaded checkpoint if MYNA_MODEL_SRC points at one
# (expects $MYNA_MODEL_SRC/model-streaming-multi/*.nemo). Otherwise the HF
# download uses the *shared* cache (default HF_HOME) so it dedups against a copy
# you already have and resumes — do not pin a throwaway per-snap cache here.
src="${MYNA_MODEL_SRC:+$MYNA_MODEL_SRC/model-streaming-multi}"
if [ -n "$src" ] && ls "$src"/*.nemo >/dev/null 2>&1; then
    echo "reusing local checkpoint from $src (no download)"
    mkdir -p "$dest"
    cp -al "$src"/*.nemo "$dest/" 2>/dev/null || cp -a "$src"/*.nemo "$dest/"
    ls -lh "$dest"/*.nemo
    exit 0
fi

if ! command -v hf >/dev/null 2>&1; then
    echo "error: 'hf' CLI not found. Install with: uv tool install 'huggingface_hub[cli]'" >&2
    echo "       (or set MYNA_MODEL_SRC to a directory holding model-streaming-multi/*.nemo)" >&2
    exit 1
fi

echo "downloading $model (.nemo) -> $dest"
hf download "$model" --local-dir "$dest" --include "*.nemo"
echo "done. checkpoint:"
ls -lh "$dest"/*.nemo
