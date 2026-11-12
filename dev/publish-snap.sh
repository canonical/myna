#!/bin/bash
# Upload a packed snap and every component it declares, and release them.
#
#   dev/publish-snap.sh <snap-dir> <channel>
#
# The version is the one the last pack staged (dev/stage-version.sh), so the
# files are that pack's. Components come from snapcraft.yaml, not a glob: a
# .comp left by a component since removed is never uploaded. A -dirty build is
# refused, as no commit reproduces it.
set -euo pipefail

snap_dir="$(cd "$1" && pwd)"
channel=$2
yaml="$snap_dir/snap/snapcraft.yaml"

name=$(awk '/^name:/ {print $2; exit}' "$yaml")
version=$(sed -n 's/.*release version="\([^"]*\)".*/\1/p' "$snap_dir/version/version.metainfo.xml")
if [[ $version == *-dirty ]]; then
    echo "publish-snap: refusing $name $version, built from uncommitted changes" >&2
    exit 1
fi

args=("${name}_${version}_$(dpkg --print-architecture).snap")
while read -r component; do
    args+=(--component "$component=$name+$component.comp")
done < <(awk '/^components:/ {c = 1; next} c && /^[^ #]/ {c = 0}
              c && /^  [a-z0-9-]+:/ {sub(/:.*/, ""); print $1}' "$yaml")

cd "$snap_dir"
log=$(mktemp)
trap 'rm -f "$log"' EXIT
set -x
snapcraft upload --release "$channel" "${args[@]}" 2>&1 | tee "$log"
set +x

# snapcraft exits 0 and says "released" even when the store's review
# failed; the last Status line is the store's verdict. On "ready to
# release!" snapcraft releases the revision itself.
status=$(grep -a '^Status: ' "$log" | tail -n 1)
if [[ $status != "Status: released" && $status != "Status: ready to release!" ]]; then
    echo "publish-snap: the store did not release $name: ${status:-no status}" >&2
    exit 1
fi
