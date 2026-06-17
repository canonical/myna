#!/bin/bash
# Fetch Qwen3-ASR weights into components/ so `snapcraft pack` can ship them as
# snap model components (see snapcraft.yaml `model-components` part).
#
# Weights are Qwen/Qwen3-ASR-* (Apache-2.0) — redistributable as components.
# Output dirs are gitignored; run this before packing.
#
#   ./dev/download-models.sh                          # 0.6B and 1.7B
#   ./dev/download-models.sh 0.6B                      # just one
#   MYNA_MODEL_SRC=~/path/to/models ./dev/download-models.sh 0.6B   # reuse a
#       # local copy (hardlinked in, no multi-GB download) if it has the weights
set -euo pipefail

snap_dir="$(cd "$(dirname "$0")/.." && pwd)"
dest="$snap_dir/components"

# Reuse an already-downloaded model tree if MYNA_MODEL_SRC points at one
# (expects $MYNA_MODEL_SRC/Qwen3-ASR-<size>/model.safetensors). Otherwise the HF
# download uses the *shared* cache (default HF_HOME) so it dedups against models
# you already have and resumes — do not pin a throwaway per-snap cache here.
src_root="${MYNA_MODEL_SRC:-}"

models=("${@:-0.6B 1.7B}")
# shellcheck disable=SC2128  # intentional word-split of the default set
read -r -a models <<<"${models[*]}"

for name in "${models[@]}"; do
    out="$dest/Qwen3-ASR-${name}"
    if [ -f "$out/model.safetensors" ]; then
        echo "Qwen3-ASR-${name}: already present at $out — skipping"
        continue
    fi

    src="${src_root:+$src_root/Qwen3-ASR-${name}}"
    if [ -n "$src" ] && [ -f "$src/model.safetensors" ]; then
        echo "Qwen3-ASR-${name}: reusing local copy at $src (no download)"
        mkdir -p "$out"
        # Hardlink when on the same filesystem (instant, no extra space); fall
        # back to a plain copy across filesystems. Skip the HF download metadata.
        cp -al "$src/." "$out/" 2>/dev/null || cp -a "$src/." "$out/"
        rm -rf "$out/.cache"
        continue
    fi

    if ! command -v hf >/dev/null 2>&1; then
        echo "error: 'hf' CLI not found. Install with: uv tool install 'huggingface_hub[cli]'" >&2
        echo "       (or set MYNA_MODEL_SRC to a directory holding Qwen3-ASR-${name}/)" >&2
        exit 1
    fi
    echo "Qwen3-ASR-${name}: downloading Qwen/Qwen3-ASR-${name} -> $out"
    hf download "Qwen/Qwen3-ASR-${name}" --local-dir "$out"
done

echo "done. components ready under $dest/"
