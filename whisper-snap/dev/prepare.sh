#!/bin/bash
# Stage the myna wheel and its pinned requirements into wheels/ for the
# server-app part.
# Run from anywhere; operates on the repo this script lives in.
set -euo pipefail

snap_dir="$(cd "$(dirname "$0")/.." && pwd)"
repo_root="$(dirname "$snap_dir")"
"$repo_root/dev/stage-licenses.sh" "$snap_dir"
"$repo_root/dev/stage-version.sh" "$snap_dir" server whisper-snap

cd "$repo_root/server"
rm -f "$snap_dir"/wheels/myna-*.whl
uv build --wheel --out-dir "$snap_dir/wheels"
# Every package the daemon imports, at the version and hashes server/uv.lock
# pins (pip installs it with --require-hashes). core24's Python is 3.12.
uv export --frozen --extra whisper --no-dev --no-emit-project --python 3.12 \
    --no-header --output-file "$snap_dir/wheels/requirements.txt" >/dev/null
ls -l "$snap_dir/wheels"
