#!/bin/bash
# Stage the Nemotron weights into components/ so `snapcraft pack` can ship them
# as snap model components (see snapcraft.yaml `model-components` part).
#
#   ./dev/download-models.sh
#
# The graphs are exported here from NVIDIA's checkpoints, not downloaded:
# needs NeMo and torch, in the exporter's own uv environment, and around 7 GB
# of memory per export.
set -euo pipefail

snap_dir="$(cd "$(dirname "$0")/.." && pwd)"
repo_root="$(dirname "$snap_dir")"
# shellcheck source-path=SCRIPTDIR/../.. source=dev/model-pin.sh
. "$repo_root/dev/model-pin.sh"

models="${XDG_CACHE_HOME:-$HOME/.cache}/myna/models"

# What a component ships, named rather than globbed: the export also leaves
# the fp32 graphs, and craft packs whatever is in components/.
shipped="encoder-model.int8.onnx decoder_joint-model.int8.onnx mel.onnx vocab.txt config.json"

# Keep in step with MODELS, RIGHT_CONTEXT and RECIPE in
# dev/nemotron/export_nemotron_onnx.py; test_model_pins.py holds them together.
declare -A REVISIONS=(
    [en]=nvidia/nemotron-speech-streaming-en-0.6b@7a9b763e6c5fb103da690219c049fac917aa50b1
    [3.5]=nvidia/nemotron-3.5-asr-streaming-0.6b@ea30d66debe3740a08b573244286791d423d6b3e
)
right_context=6
recipe=1

# model -> component id, extra files, license
declare -A COMPONENT=([en]=model-nemotron-en [3.5]=model-nemotron-3-5)
declare -A EXTRA=([en]="" [3.5]="prompt-model.onnx")
declare -A LICENSE=([en]=NVIDIA-Open-Model-License.txt [3.5]=OpenMDW-1.1.txt)

stage() {
    local model="$1"
    local rev="${REVISIONS[$model]} rc $right_context recipe $recipe"
    local src="$models/nemotron-$model-rc$right_context"
    local dst="$snap_dir/components/${COMPONENT[$model]}"

    if pin_is_current "$dst" "$rev"; then
        echo "model already present at $dst - skipping"
        return 0
    fi
    if ! pin_is_current "$src" "$rev"; then
        "$repo_root/dev/nemotron/export_nemotron_onnx.py" "$model"
    fi
    if ! pin_is_current "$src" "$rev"; then
        echo "error: $src is not the $rev export" >&2
        exit 1
    fi
    rm -rf "$dst"
    mkdir -p "$dst"
    for name in $shipped ${EXTRA[$model]}; do
        cp -al "$src/$name" "$dst/$name" 2>/dev/null || cp -a "$src/$name" "$dst/$name"
    done
    cp "$snap_dir/model-licenses/${LICENSE[$model]}" "$dst/LICENSE"
    pin_stamp "$dst" "$rev"
    echo "component ready at $dst"
}

stage en
stage 3.5
