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
repo_root="$(dirname "$snap_dir")"
dest="$snap_dir/components/model-streaming-multi"
# shellcheck source-path=SCRIPTDIR/../.. source=dev/model-pin.sh
. "$repo_root/dev/model-pin.sh"

model="nvidia/stt_en_fastconformer_hybrid_large_streaming_multi"
# Upstream revision, pinned — see dev/model-pin.sh for why, and for what the
# UPSTREAM_REVISION stamp in the staged directory is doing.
rev=ae98143333690bd7ced4bc8ec16769bcb8918374

if ls "$dest"/*.nemo >/dev/null 2>&1; then
    if pin_is_current "$dest" "$rev"; then
        echo "checkpoint already present in $dest — skipping"
        exit 0
    fi
    staged="$(pin_revision_of "$dest")"
    echo "checkpoint staged at ${staged:-an unpinned revision}, pin moved to ${rev:0:12} — restaging"
    rm -rf "$dest"
fi

# Reuse an already-downloaded checkpoint if MYNA_MODEL_SRC points at one
# (expects $MYNA_MODEL_SRC/model-streaming-multi/*.nemo). Otherwise `hf download
# --local-dir` fetches straight into the component directory: it resumes via its
# own ledger under $dest/.cache, but it does NOT dedup against HF_HOME, so
# MYNA_MODEL_SRC is the only thing that saves the download across checkouts.
src="${MYNA_MODEL_SRC:+$MYNA_MODEL_SRC/model-streaming-multi}"
if [ -n "$src" ] && ls "$src"/*.nemo >/dev/null 2>&1; then
    pin_check_source "$src" "$rev" "$0"
    echo "reusing local checkpoint from $src (no download)"
    mkdir -p "$dest"
    cp -al "$src"/*.nemo "$dest/" 2>/dev/null || cp -a "$src"/*.nemo "$dest/"
    pin_stamp "$dest" "$rev"
    ls -lh "$dest"/*.nemo
    exit 0
fi

if ! command -v hf >/dev/null 2>&1; then
    echo "error: 'hf' CLI not found. Install with: uv tool install 'huggingface_hub[cli]'" >&2
    echo "       (or set MYNA_MODEL_SRC to a directory holding model-streaming-multi/*.nemo)" >&2
    exit 1
fi

echo "downloading $model@${rev:0:12} (.nemo) -> $dest"
hf download "$model" --revision "$rev" --local-dir "$dest" --include "*.nemo"
pin_stamp "$dest" "$rev"
echo "done. checkpoint:"
ls -lh "$dest"/*.nemo
