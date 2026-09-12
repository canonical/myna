#!/bin/bash
# Lint every inference snap's engine/runtime/model manifests with modelctl's
# own validator (`modelctl debug lint-package`, inference-snaps-cli v2.0.0-beta.11+).
#
# Downloads the pinned release tarball (the same one the snaps stage) into a
# cache dir and runs it over each snap package directory. Catches schema drift
# at the source: missing runtime `name`, retired model `id`, unsupported
# capabilities, components not declared in snapcraft.yaml.
set -euo pipefail

MODELCTL_RELEASE="v2.0.0-beta.12"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/myna/modelctl-${MODELCTL_RELEASE}"

ARCH="$(dpkg --print-architecture 2>/dev/null || echo amd64)"
TARBALL="inference-snaps-cli-linux-${ARCH}.tar.xz"
URL="https://github.com/canonical/inference-snaps-cli/releases/download/${MODELCTL_RELEASE}/${TARBALL}"

if [ ! -x "$CACHE_DIR/bin/modelctl" ]; then
    mkdir -p "$CACHE_DIR"
    echo "Fetching modelctl ${MODELCTL_RELEASE} (${ARCH})"
    curl -fsSL "$URL" | tar -Jx -C "$CACHE_DIR"
fi

SNAP_DIRS=(
    whisper-snap
    parakeet-snap
    sherpa-snap
    funasr-snap
    qwen-snap
    nemotron-snap
    audio8-snap
)

failed=0
for dir in "${SNAP_DIRS[@]}"; do
    "$CACHE_DIR/bin/modelctl" debug lint-package "$REPO_ROOT/$dir" || failed=1
done

[ "$failed" -eq 0 ] || { echo "lint-package: failures above"; exit 1; }
echo "lint-package: all snaps clean (modelctl ${MODELCTL_RELEASE})"
