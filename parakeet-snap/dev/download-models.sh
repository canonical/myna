#!/bin/bash
# Stage the Parakeet weights into components/ so `snapcraft pack` can ship them
# as snap model components (see snapcraft.yaml `model-components` part): the
# int8 export for the cpu engine, and the fp32 export for nvidia-gpu.
#
#   ./dev/download-models.sh
set -euo pipefail

snap_dir="$(cd "$(dirname "$0")/.." && pwd)"
repo_root="$(dirname "$snap_dir")"
out="$snap_dir/components/model-parakeet-int8"
# shellcheck source-path=SCRIPTDIR/../.. source=dev/model-pin.sh
. "$repo_root/dev/model-pin.sh"

# What the component ships, named rather than globbed: the model cache is also
# where experiments leave their own encoders, and craft packs whatever is in
# components/. Keep in step with MODEL_FILES in
# dev/parakeet/fetch_parakeet_onnx.py; test_model_pins.py holds the two together.
model_files="encoder-model.int8.onnx decoder_joint-model.int8.onnx nemo128.onnx vocab.txt"
encoder="encoder-model.int8.onnx"

# Upstream release, pinned. Unlike the HF fetchers this one was always pinned
# (a versioned release URL, sha256-verified in the python fetcher); what was
# missing is the staged-directory stamp, so a component staged from an older
# release survived a pin move unnoticed. Keep in step with URL in
# dev/parakeet/fetch_parakeet_onnx.py; test_model_pins.py holds the two together.
rev="murmure-model 1.2.0"

cache="${XDG_CACHE_HOME:-$HOME/.cache}/myna/models/parakeet-tdt-0.6b-v3-int8"

stage_int8() {
    if [ -f "$out/$encoder" ] && pin_is_current "$out" "$rev"; then
        echo "model already present at $out - skipping"
        return 0
    fi

    # The python fetcher is the guard: it stages only when the XDG cache carries
    # this release's stamp, so a cache left from an older pin is re-downloaded and
    # sha256-verified rather than hardlinked in blind.
    cd "$repo_root/server"
    uv run python "$repo_root/dev/parakeet/fetch_parakeet_onnx.py"

    if [ -d "$out" ]; then
        staged="$(pin_revision_of "$out")"
        echo "restaging $out (staged at ${staged:-an unpinned release}, pin is $rev)"
        for f in "$out"/*; do
            [ -e "$f" ] || continue
            name="$(basename "$f")"
            case " $model_files $PIN_STAMP_FILE " in
            *" $name "*) continue ;;
            esac
            # Craft never removes: a file dropped from components/ stays in the
            # part's stage/prime dirs and ships anyway. See the model-components
            # part in snap/snapcraft.yaml.
            echo "note: $name is staged but no longer shipped - run" >&2
            echo "      \`snapcraft clean model-components\` before packing, or it is" >&2
            echo "      packed again anyway" >&2
        done
        rm -rf "$out"
    fi

    mkdir -p "$out"
    for name in $model_files; do
        cp -al "$cache/$name" "$out/$name" 2>/dev/null || cp -a "$cache/$name" "$out/$name"
    done
    pin_stamp "$out" "$rev"
    echo "component ready at $out"
}

# The float exports are made here, from NVIDIA's checkpoint, rather than
# downloaded. Keep in step with REVISION and RECIPE in
# dev/parakeet/export_parakeet_onnx.py; test_model_pins.py holds them together.
export_rev="nvidia/parakeet-tdt-0.6b-v3@541d1f99c6b0c3cd0b11a95167540bb8edefd82b recipe 2"
models="${XDG_CACHE_HOME:-$HOME/.cache}/myna/models"

stage_float() {
    local precision=fp32
    local src="$models/parakeet-tdt-0.6b-v3-$precision"
    local dst="$snap_dir/components/model-parakeet-$precision"

    if ! pin_is_current "$src" "$export_rev"; then
        # Needs NeMo and torch, in the script's own uv environment.
        "$repo_root/dev/parakeet/export_parakeet_onnx.py"
    fi
    if pin_is_current "$dst" "$export_rev"; then
        echo "model already present at $dst - skipping"
        return 0
    fi
    if ! pin_is_current "$src" "$export_rev"; then
        echo "error: $src is not the $export_rev export" >&2
        exit 1
    fi
    rm -rf "$dst"
    mkdir -p "$dst"
    for f in "$src"/*; do
        cp -al "$f" "$dst/" 2>/dev/null || cp -a "$f" "$dst/"
    done
    echo "component ready at $dst"
}

stage_int8
stage_float
