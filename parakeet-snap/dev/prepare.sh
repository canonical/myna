#!/bin/bash
# Stage the myna wheel into wheels/ for the server-app part, and the CUDA
# runtime's wheels into wheelhouse-cuda/ for the onnxruntime-cuda part.
# Run from anywhere; operates on the repo this script lives in.
set -euo pipefail

snap_dir="$(cd "$(dirname "$0")/.." && pwd)"
repo_root="$(dirname "$snap_dir")"
"$repo_root/dev/stage-licenses.sh" "$snap_dir"

cd "$repo_root/server"
uv build --wheel --out-dir "$snap_dir/wheels"
ls -l "$snap_dir/wheels"

# The myna wheel is linked in too: craft rebuilds a part only when its own
# source changes, and a wheel read from outside it left the component packing
# the previous build's server code.
#
# The CUDA stack is 1.5 GB of wheels. They are kept in one cache shared by every
# checkout and hardlinked in, so a new worktree does not download them again,
# and the part installs them with --no-index. `pip download` reuses a file
# already in --dest, so only what is missing is fetched.
wheels="${XDG_CACHE_HOME:-$HOME/.cache}/myna/wheels"
house="$snap_dir/wheelhouse-cuda"
report="$(mktemp)"
trap 'rm -f "$report"' EXIT
target=(--only-binary=:all: --python-version 3.12 --implementation cp --abi cp312
    --platform manylinux_2_28_x86_64 --platform manylinux_2_27_x86_64
    --platform manylinux_2_17_x86_64 --platform manylinux2014_x86_64
    --platform manylinux_2_12_x86_64)
wanted=(--requirement "$snap_dir/runtimes/onnxruntime-cuda/requirements.txt"
    "$snap_dir"/wheels/myna-*.whl)
mkdir -p "$wheels"
python3 -m pip download --quiet --dest "$wheels" "${target[@]}" "${wanted[@]}"
# Resolve the closure offline against the cache, then link exactly that.
python3 -m pip install --quiet --dry-run --ignore-installed --report "$report" \
    --no-index --find-links "$wheels" --target "$house.resolve" "${target[@]}" "${wanted[@]}"
rm -rf "$house" "$house.resolve"
mkdir -p "$house"
python3 -c '
import json, sys, urllib.parse, urllib.request
for item in json.load(open(sys.argv[1]))["install"]:
    print(urllib.request.url2pathname(urllib.parse.urlparse(item["download_info"]["url"]).path))
' "$report" | xargs -I{} ln {} "$house/"
echo "wheelhouse-cuda: $(find "$house" -name '*.whl' | wc -l) wheels"
