#!/bin/bash
# Fetch the Parakeet int8 ONNX weights into components/ so `snapcraft pack`
# can ship them as a snap model component (see snapcraft.yaml
# `model-components` part).
#
# Source: murmure's parakeet-tdt-0.6b-v3-int8 bundle (see
# dev/fetch_parakeet_onnx.py for why not istupakov's HF export). Reuses the
# already-staged XDG cache copy when present (hardlinked, no download).
#
#   ./dev/download-models.sh
set -euo pipefail

snap_dir="$(cd "$(dirname "$0")/.." && pwd)"
repo_root="$(dirname "$snap_dir")"
out="$snap_dir/components/model-parakeet-int8"

if [ -f "$out/encoder-model.int8.onnx" ]; then
    echo "model already present at $out — skipping"
    exit 0
fi

# Reuse the staged cache if it exists; otherwise fetch (resumable, sha256).
cache="${XDG_CACHE_HOME:-$HOME/.cache}/myna/models/parakeet-tdt-0.6b-v3-int8"
if [ ! -f "$cache/encoder-model.int8.onnx" ]; then
    cd "$repo_root/server"
    uv run python "$repo_root/dev/fetch_parakeet_onnx.py"
fi

mkdir -p "$out"
cp -al "$cache/." "$out/" 2>/dev/null || cp -a "$cache/." "$out/"
echo "component ready at $out"
