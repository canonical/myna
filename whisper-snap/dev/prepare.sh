#!/bin/bash
# Stage the myna wheel into wheels/ for the server-app part.
# Run from anywhere; operates on the repo this script lives in.
set -euo pipefail

snap_dir="$(cd "$(dirname "$0")/.." && pwd)"
repo_root="$(dirname "$snap_dir")"

cd "$repo_root"
uv build --wheel --out-dir "$snap_dir/wheels"
ls -l "$snap_dir/wheels"
