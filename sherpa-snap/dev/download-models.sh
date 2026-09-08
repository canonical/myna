#!/bin/bash
# Fetch this snap's two model components so `snapcraft pack` can ship them:
# the streaming FastConformer transducer, and the punctuation + truecasing
# model that turns its lowercase output into text a user can type.
#
# Sources:
#   csukuangfj/sherpa-onnx-nemo-streaming-fast-conformer-transducer-en-480ms-int8
#     at the revision pinned below (see dev/fetch_sherpa_model.py). Copies out
#     of the shared HF cache, which snapshot_download keys by revision.
#   k2-fsa/sherpa-onnx release `punctuation-models`, sha256-pinned in
#     dev/fetch_sherpa_punct_model.py (it is not on the Hub).
#
#   ./dev/download-models.sh
set -euo pipefail

snap_dir="$(cd "$(dirname "$0")/.." && pwd)"
repo_root="$(dirname "$snap_dir")"
out="$snap_dir/components/model-fastconformer-480ms"
punct_out="$snap_dir/components/model-punct-en"
# shellcheck source-path=SCRIPTDIR/../.. source=dev/model-pin.sh
. "$repo_root/dev/model-pin.sh"

repo=csukuangfj/sherpa-onnx-nemo-streaming-fast-conformer-transducer-en-480ms-int8
# Upstream revision, pinned — see dev/model-pin.sh. Kept in step with
# REVISION in dev/fetch_sherpa_model.py, which stages the same repo for the
# non-snap adapter runs.
rev=df8ed95e44a70924450381e610770f9d656d1e15

# --- punctuation model ------------------------------------------------------
#
# The pin is the archive's sha256, not a revision: k2-fsa hangs these off a
# `punctuation-models` tag, and a tag can move under a release asset. The
# python fetcher is the guard - it re-downloads and re-verifies whenever the
# XDG cache does not carry this archive's stamp - so this only has to decide
# whether the component is already staged from it.
punct_rev="$(sed -n 's/^SHA256 = "\(.*\)"$/\1/p' "$repo_root/dev/fetch_sherpa_punct_model.py")"
punct_archive="$(sed -n 's/^ARCHIVE = "\(.*\)"$/\1/p' "$repo_root/dev/fetch_sherpa_punct_model.py")"
punct_stamp="$punct_archive sha256:$punct_rev"
punct_cache="${XDG_CACHE_HOME:-$HOME/.cache}/myna/models/$punct_archive"

if [ -f "$punct_out/model.int8.onnx" ] && pin_is_current "$punct_out" "$punct_stamp"; then
    echo "punctuation model already present at $punct_out - skipping"
else
    (cd "$repo_root/server" && uv run python "$repo_root/dev/fetch_sherpa_punct_model.py")
    rm -rf "$punct_out"
    mkdir -p "$punct_out"
    for f in model.int8.onnx bpe.vocab; do
        cp -al "$punct_cache/$f" "$punct_out/$f" 2>/dev/null ||
            cp -a "$punct_cache/$f" "$punct_out/$f"
    done
    pin_stamp "$punct_out" "$punct_stamp"
    echo "component ready at $punct_out"
fi

# --- transducer -------------------------------------------------------------

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
