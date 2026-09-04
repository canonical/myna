#!/bin/bash
# Stage the myna wheel *and* the whole [nemotron] dependency closure into
# wheels/ for the nemo-cuda component.
# Run from anywhere; operates on the repo this script lives in.
#
# Why pre-download instead of letting the part pip-install from PyPI: the
# closure is torch + the nvidia-* CUDA wheels, several GB. pip's own cache
# lives inside the snapcraft build instance, so it survives repeat builds but
# not `lxc delete` - and pruning build instances is routine when the LXD pool
# fills. Downloading on the host instead puts the bytes behind ~/.cache/pip,
# where they survive anything done to the build container, and makes the
# nemo-cuda part build offline.
#
#   ./dev/prepare.sh            # incremental; re-resolves, reuses the pip cache
#   ./dev/prepare.sh --wheel-only   # just the myna wheel (deps already staged)
set -euo pipefail

snap_dir="$(cd "$(dirname "$0")/.." && pwd)"
repo_root="$(dirname "$snap_dir")"
wheels="$snap_dir/wheels"

cd "$repo_root/server"
# The venv interpreter, not the system one: core24 ships python3.12 and the
# downloaded wheels must match the build container's ABI tags.
python="$repo_root/server/.venv/bin/python"
[ -x "$python" ] || { echo "no server/.venv - run 'uv sync' first" >&2; exit 1; }

uv build --wheel --out-dir "$wheels"

if [ "${1:-}" = "--wheel-only" ]; then
    echo "staged the myna wheel only; dependency closure left as-is"
    # find, not `ls | head` (SC2012): the staged closure can be hundreds of
    # wheels, so the listing stays capped.
    find "$wheels" -maxdepth 1 -name '*.whl' -printf '%f\n' | sort | head
    exit 0
fi

echo "resolving the [nemotron] closure into $wheels (first run downloads several GB)"
# No --only-binary=:all:. A couple of the closure's members (wget, and
# nemo-toolkit itself) publish sdists only, so demanding wheels everywhere makes
# the resolve impossible. pip prefers wheels regardless, which is where all the
# weight is: torch and the nvidia-* CUDA packages.
"$python" -m pip download \
    --dest "$wheels" \
    "$wheels"/myna-0.0.1-py3-none-any.whl'[nemotron]'

echo
echo "staged $(find "$wheels" -name '*.whl' | wc -l) wheels" \
     "+ $(find "$wheels" \( -name '*.tar.gz' -o -name '*.zip' \) | wc -l) sdists," \
     "$(du -sh "$wheels" | cut -f1) total"
