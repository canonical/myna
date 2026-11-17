#!/bin/bash
# Stage the IndicConformer weights into components/ so `snapcraft pack` can
# ship them as snap model components (see snapcraft.yaml `model-components`).
#
#   ./dev/download-models.sh
#
# The graphs are exported here from AI4Bharat's checkpoint, not downloaded:
# needs NeMo and torch, in the exporter's own uv environment, and around 4 GB
# of memory.
set -euo pipefail

snap_dir="$(cd "$(dirname "$0")/.." && pwd)"
repo_root="$(dirname "$snap_dir")"
# shellcheck source-path=SCRIPTDIR/../.. source=dev/model-pin.sh
. "$repo_root/dev/model-pin.sh"

models="${XDG_CACHE_HOME:-$HOME/.cache}/myna/models"

# What a component ships, named rather than globbed: the export also leaves
# the fp32 graphs and the RNN-T decoder_joint, and craft packs whatever is in
# components/. The adapter serves the CTC head by default.
shipped="encoder-model.int8.onnx ctc-model.onnx mel-model.onnx vocab.txt config.json"

# Keep in step with MODELS and RECIPE in
# dev/indicconformer/export_indicconformer_onnx.py; test_model_pins.py holds
# them together.
model=large
url=https://objectstore.e2enetworks.net/indicconformer/models/indicconformer_stt_hi_hybrid_rnnt_large.nemo
sha256=7cad1308751a56aed1fda1f30c9078077b1906a2e04454f92128d327561d9008
recipe=2

stage() {
    local language="$1"
    local rev="$url sha256:$sha256 $language recipe $recipe"
    local src="$models/indicconformer-$model-$language"
    local dst="$snap_dir/components/model-indicconformer-$language"

    if pin_is_current "$dst" "$rev"; then
        echo "model already present at $dst - skipping"
        return 0
    fi
    if ! pin_is_current "$src" "$rev"; then
        "$repo_root/dev/indicconformer/export_indicconformer_onnx.py" "$model" "$language"
    fi
    if ! pin_is_current "$src" "$rev"; then
        echo "error: $src is not the $rev export" >&2
        exit 1
    fi
    rm -rf "$dst"
    mkdir -p "$dst"
    for name in $shipped; do
        cp -al "$src/$name" "$dst/$name" 2>/dev/null || cp -a "$src/$name" "$dst/$name"
    done
    chmod a+r "$dst"/*
    cp "$snap_dir/model-licenses/AI4Bharat-MIT.txt" "$dst/LICENSE"
    pin_stamp "$dst" "$rev"
    echo "component ready at $dst"
}

stage hi
