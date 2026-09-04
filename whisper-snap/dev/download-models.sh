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
repo_root="$(dirname "$snap_dir")"
dest="$snap_dir/components"
# shellcheck source-path=SCRIPTDIR/../.. source=dev/model-pin.sh
. "$repo_root/dev/model-pin.sh"

# Upstream revisions, pinned per size — see dev/model-pin.sh for why, and for
# what the UPSTREAM_REVISION stamp in each staged directory is doing.
declare -A REVISIONS=(
    [tiny]=d90ca5fe260221311c53c58e660288d3deb8d356
    [base]=ebe41f70d5b6dfa9166e2c581c45c9c0cfc57b66
    [small]=536b0662742c02347bc0e980a01041f333bce120
)

# Reuse an already-downloaded model tree if MYNA_MODEL_SRC points at one
# (expects $MYNA_MODEL_SRC/model-<size>-ct2/model.bin). Otherwise `hf download
# --local-dir` fetches straight into the component directory: it keeps its own
# resume ledger under $out/.cache, but it does NOT dedup against HF_HOME, so
# MYNA_MODEL_SRC is the only thing that saves the bytes across checkouts.
src_root="${MYNA_MODEL_SRC:-}"

models=("${@:-tiny base small}")
# shellcheck disable=SC2128  # intentional word-split of the default set
read -r -a models <<<"${models[*]}"

for name in "${models[@]}"; do
    rev="${REVISIONS[$name]:-}"
    [ -n "$rev" ] || { echo "error: no pinned revision for model-${name}" >&2; exit 1; }

    out="$dest/model-${name}-ct2"
    if [ -f "$out/model.bin" ]; then
        if pin_is_current "$out" "$rev"; then
            echo "model-${name}: already present at $out — skipping"
            continue
        fi
        staged="$(pin_revision_of "$out")"
        echo "model-${name}: staged at ${staged:-an unpinned revision}, pin moved to ${rev:0:12} — restaging"
        rm -rf "$out"
    fi

    src="${src_root:+$src_root/model-${name}-ct2}"
    if [ -n "$src" ] && [ -f "$src/model.bin" ]; then
        pin_check_source "$src" "$rev" "$0"
        echo "model-${name}: reusing local copy at $src (no download)"
        mkdir -p "$out"
        cp -al "$src/." "$out/" 2>/dev/null || cp -a "$src/." "$out/"
        rm -rf "$out/.cache"
        pin_stamp "$out" "$rev"
        continue
    fi

    if ! command -v hf >/dev/null 2>&1; then
        echo "error: 'hf' CLI not found. Install with: uv tool install 'huggingface_hub[cli]'" >&2
        echo "       (or set MYNA_MODEL_SRC to a directory holding model-${name}-ct2/)" >&2
        exit 1
    fi
    echo "model-${name}: downloading Systran/faster-whisper-${name}@${rev:0:12} -> $out"
    hf download "Systran/faster-whisper-${name}" --revision "$rev" --local-dir "$out"
    pin_stamp "$out" "$rev"
done

echo "done. components ready under $dest/"
