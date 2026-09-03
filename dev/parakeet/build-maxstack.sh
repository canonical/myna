#!/bin/bash
# Build the maxstack encoder into the Parakeet model cache: fetch or build
# every input build_maxstack_encoder.py needs, then run it. Costs a ~330 MB
# corpus download and a calibration pass peaking at several GB of RSS.
#
#   dev/parakeet/build-maxstack.sh [--model-dir DIR]
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"
model_dir="${XDG_CACHE_HOME:-$HOME/.cache}/myna/models/parakeet-tdt-0.6b-v3-int8"

while [ $# -gt 0 ]; do
    case "$1" in
    --model-dir)
        model_dir="$2"
        shift 2
        ;;
    -h | --help)
        sed -n '2,6p' "$0"
        exit 0
        ;;
    *)
        echo "unknown argument: $1" >&2
        exit 2
        ;;
    esac
done

echo "== input 1/3: pinned upstream export =="
cd "$repo_root/server"
uv run --extra parakeet python "$repo_root/dev/parakeet/fetch_parakeet_onnx.py"

echo "== input 2/3: custom-op kernels =="
if [ -f "$here/qsilu/libqsilu.so" ]; then
    echo "libqsilu.so already built - skipping"
else
    "$here/qsilu/build.sh"
fi

echo "== input 3/3: calibration corpus =="
if compgen -G "$repo_root/corpus/real/audio/*.wav" >/dev/null; then
    echo "corpus/real already present - skipping"
else
    cd "$repo_root/server"
    uv run --extra parakeet python "$repo_root/dev/fetch_real_corpus.py"
fi

echo "== build =="
cd "$repo_root/server"
uv run --extra parakeet python "$repo_root/dev/parakeet/build_maxstack_encoder.py" --model-dir "$model_dir"
