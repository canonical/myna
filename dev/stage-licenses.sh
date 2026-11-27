#!/bin/bash
# Stage the license text into a snap project so it ships with the package.
#
#   dev/stage-licenses.sh <snap-dir>
#
# Copies the tree's LICENSE, and its NOTICE (third-party code) followed by the
# snap's own when it has one (model attribution), and third-party-licenses/,
# into <snap-dir>/licenses/, which the snap's `licenses` part dumps under usr/share/doc/<snap>/. Called from every snap's dev/prepare.sh:
# craft-parts local sources must live inside the project dir, so a part cannot
# reach ../LICENSE itself.
set -euo pipefail

snap_dir="$(cd "$1" && pwd)"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out="$snap_dir/licenses"

rm -rf "$out"
mkdir -p "$out"
# install -m, not cp: a checkout made under umask 077 is 0600, and the snap
# must not ship licence texts its users cannot read.
install -m 0644 "$repo_root/LICENSE" "$out/LICENSE"
# Licence texts of third-party data compiled into a binary (see NOTICE).
install -m 0644 "$repo_root"/third-party-licenses/* "$out/"
{
    cat "$repo_root/NOTICE"
    if [ -f "$snap_dir/NOTICE" ]; then
        echo
        cat "$snap_dir/NOTICE"
    fi
} > "$out/NOTICE"
chmod 0644 "$out/NOTICE"
echo "staged licenses/ into $out"
