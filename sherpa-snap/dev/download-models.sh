#!/bin/bash
# Fetch the sherpa-onnx streaming FastConformer transducer into components/
# so `snapcraft pack` can ship it as a snap model component.
#
# Source: csukuangfj/sherpa-onnx-nemo-streaming-fast-conformer-transducer-en-480ms-int8
# at the revision pinned below (see dev/fetch_sherpa_model.py). Copies out of
# the shared HF cache, which snapshot_download keys by revision.
#
#   ./dev/download-models.sh
set -euo pipefail

snap_dir="$(cd "$(dirname "$0")/.." && pwd)"
repo_root="$(dirname "$snap_dir")"
out="$snap_dir/components/model-fastconformer-480ms"
# shellcheck source-path=SCRIPTDIR/../.. source=dev/model-pin.sh
. "$repo_root/dev/model-pin.sh"

repo=csukuangfj/sherpa-onnx-nemo-streaming-fast-conformer-transducer-en-480ms-int8
# Upstream revision, pinned — see dev/model-pin.sh. Kept in step with
# REVISION in dev/fetch_sherpa_model.py, which stages the same repo for the
# non-snap adapter runs.
rev=df8ed95e44a70924450381e610770f9d656d1e15

if [ -f "$out/encoder.int8.onnx" ]; then
    if pin_is_current "$out" "$rev"; then
        echo "model already present at $out — skipping"
        exit 0
    fi
    staged="$(pin_revision_of "$out")"
    echo "model staged at ${staged:-an unpinned revision}, pin moved to ${rev:0:12} — restaging"
    rm -rf "$out"
fi

cd "$repo_root/server"
cache="$(uv run python -c "
import sys
from huggingface_hub import snapshot_download
print(snapshot_download(sys.argv[1], revision=sys.argv[2]))" "$repo" "$rev")"

mkdir -p "$out"
cp -aL "$cache/." "$out/"
rm -rf "$out/test_wavs" "$out/.cache"
pin_stamp "$out" "$rev"
echo "component ready at $out"
