#!/bin/bash
# Stage the Rust client workspace into myna-snap/client/ for the `client`
# part (craft-parts local sources must live inside the project directory), and
# record the version the snap adopts.
# Run from anywhere; operates on the repo this script lives in.
set -euo pipefail

snap_dir="$(cd "$(dirname "$0")/.." && pwd)"
repo_root="$(dirname "$snap_dir")"
"$repo_root/dev/stage-licenses.sh" "$snap_dir"

rsync -a --delete \
    --exclude target \
    "$repo_root/client/" "$snap_dir/client/"

"$repo_root/dev/snap-version.sh" client myna-snap >"$snap_dir/client/.snap-version"

echo "staged $repo_root/client → $snap_dir/client ($(cat "$snap_dir/client/.snap-version"))"
