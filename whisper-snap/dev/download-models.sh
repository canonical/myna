#!/bin/bash
# Fetch CTranslate2 Whisper weights into components/ so `snapcraft pack` can
# ship them as snap model components (see snapcraft.yaml `model-components`
# part and components/). Resumable; re-running skips models already present.
#
# Weights are Systran/faster-whisper-* (MIT) — redistributable as components.
# Output dirs are gitignored; run this before packing.
#
#   ./dev/download-models.sh            # tiny base small (the cpu engine set)
#   ./dev/download-models.sh small      # just one
set -euo pipefail

snap_dir="$(cd "$(dirname "$0")/.." && pwd)"
dest="$snap_dir/components"

# Pin a cache so re-runs are resumable and don't touch the user's default HF_HOME.
export HF_HOME="${HF_HOME:-$snap_dir/.hf-cache}"

models=("${@:-tiny base small}")
# shellcheck disable=SC2128  # intentional word-split of the default set
read -r -a models <<<"${models[*]}"

if ! command -v hf >/dev/null 2>&1; then
    echo "error: 'hf' CLI not found. Install with: uv tool install 'huggingface_hub[cli]'" >&2
    exit 1
fi

for name in "${models[@]}"; do
    out="$dest/model-${name}-ct2"
    if [ -f "$out/model.bin" ]; then
        echo "model-${name}: already present at $out — skipping"
        continue
    fi
    echo "model-${name}: downloading Systran/faster-whisper-${name} -> $out"
    hf download "Systran/faster-whisper-${name}" --local-dir "$out"
done

echo "done. components ready under $dest/"
