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
repo_root="$(dirname "$snap_dir")"
dest="$snap_dir/components"
# shellcheck source-path=SCRIPTDIR/../.. source=dev/model-pin.sh
. "$repo_root/dev/model-pin.sh"

# Upstream revisions, pinned per size — see dev/model-pin.sh for why, and for
# what the UPSTREAM_REVISION stamp in each staged directory is doing.
declare -A REVISIONS=(
    [0.6B]=5eb144179a02acc5e5ba31e748d22b0cf3e303b0
    [1.7B]=7278e1e70fe206f11671096ffdd38061171dd6e5
)

# Reuse an already-downloaded model tree if MYNA_MODEL_SRC points at one
# (expects $MYNA_MODEL_SRC/Qwen3-ASR-<size>/ to hold the weights). Otherwise `hf
# download --local-dir` fetches straight into the component directory: it keeps
# its own resume ledger under $out/.cache, but it does NOT dedup against
# HF_HOME, so MYNA_MODEL_SRC is the only thing that saves the multi-GB download
# across checkouts.
src_root="${MYNA_MODEL_SRC:-}"

models=("${@:-0.6B 1.7B}")
# shellcheck disable=SC2128  # intentional word-split of the default set
read -r -a models <<<"${models[*]}"

for name in "${models[@]}"; do
    rev="${REVISIONS[$name]:-}"
    [ -n "$rev" ] || { echo "error: no pinned revision for Qwen3-ASR-${name}" >&2; exit 1; }

    out="$dest/Qwen3-ASR-${name}"
    # 0.6B ships one model.safetensors, 1.7B an index plus shards, so what marks
    # a directory complete is the stamp pin_stamp writes after a good download.
    staged="$(pin_revision_of "$out")"
    if [ -n "$staged" ]; then
        if pin_is_current "$out" "$rev"; then
            echo "Qwen3-ASR-${name}: already present at $out — skipping"
            continue
        fi
        echo "Qwen3-ASR-${name}: staged at $staged, pin moved to ${rev:0:12} — restaging"
        rm -rf "$out"
    fi

    src="${src_root:+$src_root/Qwen3-ASR-${name}}"
    if [ -n "$src" ] && { [ -f "$src/model.safetensors" ] ||
                         [ -f "$src/model.safetensors.index.json" ]; }; then
        pin_check_source "$src" "$rev" "$0"
        echo "Qwen3-ASR-${name}: reusing local copy at $src (no download)"
        mkdir -p "$out"
        # Hardlink when on the same filesystem (instant, no extra space); fall
        # back to a plain copy across filesystems. Skip the HF download metadata.
        cp -al "$src/." "$out/" 2>/dev/null || cp -a "$src/." "$out/"
        rm -rf "$out/.cache"
        pin_stamp "$out" "$rev"
        continue
    fi

    if ! command -v hf >/dev/null 2>&1; then
        echo "error: 'hf' CLI not found. Install with: uv tool install 'huggingface_hub[cli]'" >&2
        echo "       (or set MYNA_MODEL_SRC to a directory holding Qwen3-ASR-${name}/)" >&2
        exit 1
    fi
    echo "Qwen3-ASR-${name}: downloading Qwen/Qwen3-ASR-${name}@${rev:0:12} -> $out"
    hf download "Qwen/Qwen3-ASR-${name}" --revision "$rev" --local-dir "$out"
    pin_stamp "$out" "$rev"
done

echo "done. components ready under $dest/"
