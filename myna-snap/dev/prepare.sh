#!/bin/bash
# Stage the Rust client workspace into myna-snap/client/ for the `client`
# part (craft-parts local sources must live inside the project directory).
# Run from anywhere; operates on the repo this script lives in.
set -euo pipefail

snap_dir="$(cd "$(dirname "$0")/.." && pwd)"
repo_root="$(dirname "$snap_dir")"

rsync -a --delete \
    --exclude target \
    "$repo_root/client/" "$snap_dir/client/"

echo "staged $repo_root/client → $snap_dir/client"
