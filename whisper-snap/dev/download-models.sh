#!/bin/bash
# Fetch CTranslate2 Whisper weights into components/ so `snapcraft pack` can
# ship them as snap model components (see snapcraft.yaml `model-components`
# part and components/).
#
# Weights are Systran/faster-whisper-* (MIT) — redistributable as components.
# Output dirs are gitignored; run this before packing.
#
#   ./dev/download-models.sh                          # tiny base small
#   ./dev/download-models.sh small                    # just one
#   MYNA_MODEL_SRC=~/path/to/models ./dev/download-models.sh small  # reuse a
#       # local copy (hardlinked in, no download) if it has model-<size>-ct2/
set -euo pipefail

snap_dir="$(cd "$(dirname "$0")/.." && pwd)"
dest="$snap_dir/components"

# Reuse an already-downloaded model tree if MYNA_MODEL_SRC points at one
# (expects $MYNA_MODEL_SRC/model-<size>-ct2/model.bin). Otherwise the HF
# download uses the *shared* cache (default HF_HOME) so it dedups against models
# you already have and resumes — do not pin a throwaway per-snap cache here.
src_root="${MYNA_MODEL_SRC:-}"

models=("${@:-tiny base small}")
# shellcheck disable=SC2128  # intentional word-split of the default set
read -r -a models <<<"${models[*]}"

for name in "${models[@]}"; do
    out="$dest/model-${name}-ct2"
    if [ -f "$out/model.bin" ]; then
        echo "model-${name}: already present at $out — skipping"
        continue
    fi

    src="${src_root:+$src_root/model-${name}-ct2}"
    if [ -n "$src" ] && [ -f "$src/model.bin" ]; then
        echo "model-${name}: reusing local copy at $src (no download)"
        mkdir -p "$out"
        cp -al "$src/." "$out/" 2>/dev/null || cp -a "$src/." "$out/"
        rm -rf "$out/.cache"
        continue
    fi

    if ! command -v hf >/dev/null 2>&1; then
        echo "error: 'hf' CLI not found. Install with: uv tool install 'huggingface_hub[cli]'" >&2
        echo "       (or set MYNA_MODEL_SRC to a directory holding model-${name}-ct2/)" >&2
        exit 1
    fi
    echo "model-${name}: downloading Systran/faster-whisper-${name} -> $out"
    hf download "Systran/faster-whisper-${name}" --local-dir "$out"
done

echo "done. components ready under $dest/"
