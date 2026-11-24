#!/bin/bash
# Stage the myna wheel and its pinned requirements into wheels/ for the
# server-app part, and the CUDA runtime's wheels into wheelhouse-cuda/ for
# the onnxruntime-cuda part.
# Run from anywhere; operates on the repo this script lives in.
set -euo pipefail

snap_dir="$(cd "$(dirname "$0")/.." && pwd)"
repo_root="$(dirname "$snap_dir")"
"$repo_root/dev/stage-licenses.sh" "$snap_dir"
"$repo_root/dev/stage-version.sh" "$snap_dir" server parakeet-snap

cd "$repo_root/server"
rm -f "$snap_dir"/wheels/myna-*.whl
uv build --wheel --out-dir "$snap_dir/wheels"
# Every package the daemon imports, at the version and hashes server/uv.lock
# pins (pip installs it with --require-hashes). core24's Python is 3.12.
uv export --frozen --extra parakeet --no-dev --no-emit-project --python 3.12 \
    --no-header --output-file "$snap_dir/wheels/requirements.txt" >/dev/null
ls -l "$snap_dir/wheels"

# The CUDA stack is 1.5 GB of wheels. They are kept in one cache shared by every
# checkout and hardlinked in, so a new worktree does not download them again,
# and the part installs them with --no-index. `pip download` reuses a file
# already in --dest, so only what is missing is fetched.
wheels="${XDG_CACHE_HOME:-$HOME/.cache}/myna/wheels"
house="$snap_dir/wheelhouse-cuda"
report="$(mktemp)"
trap 'rm -f "$report"' EXIT
target=(--only-binary=:all: --python-version 3.12 --implementation cp --abi cp312)
if [ "$(dpkg --print-architecture)" = arm64 ]; then
    # onnxruntime-gpu's aarch64 wheels are manylinux_2_34; core24 has glibc 2.39.
    tags="manylinux_2_34 manylinux_2_28 manylinux_2_27 manylinux_2_17 manylinux2014"
    machine=aarch64
else
    tags="manylinux_2_28 manylinux_2_27 manylinux_2_17 manylinux2014 manylinux_2_12"
    machine=x86_64
fi
for tag in $tags; do
    target+=(--platform "${tag}_$machine")
done
wanted=(--no-deps --requirement "$snap_dir/runtimes/onnxruntime-cuda/requirements.txt")
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
