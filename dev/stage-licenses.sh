#!/bin/bash
# Stage the license text into a snap project so it ships with the package.
#
#   dev/stage-licenses.sh <snap-dir>
#
# Copies the tree's LICENSE, and the snap's NOTICE when it has one (model
# attribution), into <snap-dir>/licenses/, which the snap's `licenses` part
# dumps under usr/share/doc/<snap>/. Called from every snap's dev/prepare.sh:
# craft-parts local sources must live inside the project dir, so a part cannot
# reach ../LICENSE itself.
set -euo pipefail

snap_dir="$(cd "$1" && pwd)"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out="$snap_dir/licenses"

rm -rf "$out"
mkdir -p "$out"
cp "$repo_root/LICENSE" "$out/LICENSE"
if [ -f "$snap_dir/NOTICE" ]; then
    cp "$snap_dir/NOTICE" "$out/NOTICE"
fi
echo "staged licenses/ into $out"
