#!/bin/bash
# Stage libqwen_asr.so for host benchmark runs (`provision: host`).
#
#   ./dev/build-qwen-c.sh
#   export QWEN_ASR_LIB=$PWD/.cache/qwen-c/libqwen_asr.so
#
# The qwen-c adapter otherwise only finds the library inside an *installed* qwen
# snap, so a host-provisioned target needs `snap install` (sudo) or it fails at
# load time - and a failed load is easy to misread as a terrible model.
#
# This extracts the library from the snap you already packed rather than
# rebuilding it from upstream. Deliberate: a second gcc recipe on the host would
# drift from the `qwen-c-runtime` part in qwen-snap/snap/snapcraft.yaml, and a
# host benchmark measuring different machine code than the snap ships is worse
# than no benchmark. Repack the snap and rerun this to refresh.
#
# Needs no sudo, no apt, no network. `unsquashfs` ships with squashfs-tools.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SNAP=$REPO_ROOT/qwen-snap/qwen_0.1.0-dev_amd64.snap
OUTDIR=$REPO_ROOT/.cache/qwen-c
OUT=$OUTDIR/libqwen_asr.so

if [ ! -f "$SNAP" ]; then
    echo "no snap at $SNAP - build it first:" >&2
    echo "  make snap-qwen" >&2
    exit 1
fi

if ! command -v unsquashfs >/dev/null 2>&1; then
    echo "missing unsquashfs: sudo apt install squashfs-tools" >&2
    exit 1
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

unsquashfs -n -q -d "$work/snap" -f "$SNAP" lib/libqwen_asr.so >/dev/null
mkdir -p "$OUTDIR"
install -m644 "$work/snap/lib/libqwen_asr.so" "$OUT"

# The library links against OpenBLAS, which the snap carries internally. On the
# host it has to come from the distro, so fail loudly here rather than at the
# first benchmark clip.
if ldd "$OUT" | grep -q "not found"; then
    echo "" >&2
    echo "unresolved shared libraries:" >&2
    ldd "$OUT" | grep "not found" >&2
    echo "install the runtime: sudo apt install libopenblas0-pthread" >&2
    exit 1
fi

echo "staged $OUT (from $(basename "$SNAP"))"
echo
echo "point the adapter at it:"
echo "  export QWEN_ASR_LIB=$OUT"
