#!/bin/bash
# Fetch the Parakeet int8 ONNX weights into components/ so `snapcraft pack`
# can ship them as a snap model component (see snapcraft.yaml
# `model-components` part).
#
# Source: murmure's parakeet-tdt-0.6b-v3-int8 bundle (see
# dev/fetch_parakeet_onnx.py for why not istupakov's HF export). Reuses the
# already-staged XDG cache copy when present (hardlinked, no download).
#
#   ./dev/download-models.sh
set -euo pipefail

snap_dir="$(cd "$(dirname "$0")/.." && pwd)"
repo_root="$(dirname "$snap_dir")"
out="$snap_dir/components/model-parakeet-int8"
# shellcheck source-path=SCRIPTDIR/../.. source=dev/model-pin.sh
. "$repo_root/dev/model-pin.sh"

# Upstream release, pinned. Unlike the HF fetchers this one was always pinned
# (a versioned release URL, sha256-verified in the python fetcher); what was
# missing is the staged-directory stamp, so a component staged from an older
# release survived a pin move unnoticed. Keep in step with URL in
# dev/fetch_parakeet_onnx.py; test_model_pins.py holds the two together.
rev="murmure-model 1.2.0"

if [ -f "$out/encoder-model.int8.onnx" ]; then
    if pin_is_current "$out" "$rev"; then
        echo "model already present at $out — skipping"
        exit 0
    fi
    staged="$(pin_revision_of "$out")"
    echo "model staged at ${staged:-an unpinned release}, pin moved to $rev — restaging"
    rm -rf "$out"
fi

# The python fetcher is the guard: it stages only when the XDG cache carries
# this release's stamp, so a cache left from an older pin is re-downloaded and
# sha256-verified rather than hardlinked in blind.
cache="${XDG_CACHE_HOME:-$HOME/.cache}/myna/models/parakeet-tdt-0.6b-v3-int8"
cd "$repo_root/server"
uv run python "$repo_root/dev/fetch_parakeet_onnx.py"

mkdir -p "$out"
cp -al "$cache/." "$out/" 2>/dev/null || cp -a "$cache/." "$out/"
pin_stamp "$out" "$rev"
echo "component ready at $out"
