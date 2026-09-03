#!/bin/bash
# Stage the Parakeet int8 ONNX weights into components/ so `snapcraft pack`
# can ship them as a snap model component (see snapcraft.yaml
# `model-components` part).
#
#   ./dev/download-models.sh <base|maxstack>
#
# One encoder ships, never both. README.md has what the two are and how the
# maxstack one is built.
set -euo pipefail

snap_dir="$(cd "$(dirname "$0")/.." && pwd)"
repo_root="$(dirname "$snap_dir")"
out="$snap_dir/components/model-parakeet-int8"
# shellcheck source-path=SCRIPTDIR/../.. source=dev/model-pin.sh
. "$repo_root/dev/model-pin.sh"

base_encoder="encoder-model.int8.onnx"
maxstack_encoder="encoder-model.int8.maxstack.onnx"
qsilu_lib="libqsilu.so"
maxstack_stamp="MAXSTACK_REVISION"

variant="${1:-}"
case "$variant" in
base)
    encoder="$base_encoder"
    skip="$maxstack_encoder $qsilu_lib $maxstack_stamp"
    ;;
maxstack)
    encoder="$maxstack_encoder"
    skip="$base_encoder"
    ;;
*)
    echo "usage: $0 <base|maxstack>" >&2
    exit 2
    ;;
esac

# Upstream release, pinned. Unlike the HF fetchers this one was always pinned
# (a versioned release URL, sha256-verified in the python fetcher); what was
# missing is the staged-directory stamp, so a component staged from an older
# release survived a pin move unnoticed. Keep in step with URL in
# dev/parakeet/fetch_parakeet_onnx.py; test_model_pins.py holds the two together.
rev="murmure-model 1.2.0"

cache="${XDG_CACHE_HOME:-$HOME/.cache}/myna/models/parakeet-tdt-0.6b-v3-int8"

if [ -f "$out/$encoder" ] && pin_is_current "$out" "$rev"; then
    echo "model already present at $out ($variant encoder) - skipping"
    exit 0
fi

# The python fetcher is the guard: it stages only when the XDG cache carries
# this release's stamp, so a cache left from an older pin is re-downloaded and
# sha256-verified rather than hardlinked in blind.
cd "$repo_root/server"
uv run python "$repo_root/dev/parakeet/fetch_parakeet_onnx.py"

if [ "$variant" = maxstack ]; then
    for f in "$maxstack_encoder" "$qsilu_lib" "$maxstack_stamp"; do
        if [ ! -f "$cache/$f" ]; then
            echo "error: $cache/$f missing" >&2
            echo "       build it: make parakeet-maxstack-encoder" >&2
            exit 1
        fi
    done
    # The maxstack encoder is derived from one specific base export and paired
    # with one build of the kernel library; neither is shipped alongside it, so
    # a mismatch has to be caught here.
    built_from="$(sed -n 's/^base: //p' "$cache/$maxstack_stamp")"
    if [ "$built_from" != "$rev" ]; then
        echo "error: $maxstack_encoder was built from ${built_from:-an unrecorded release}," >&2
        echo "       not the pinned $rev - rebuild: make parakeet-maxstack-encoder" >&2
        exit 1
    fi
    stamped_lib_sha="$(sed -n 's/^libqsilu sha256: //p' "$cache/$maxstack_stamp")"
    cache_lib_sha="$(sha256sum "$cache/$qsilu_lib" | cut -d' ' -f1)"
    if [ "$stamped_lib_sha" != "$cache_lib_sha" ]; then
        echo "error: $qsilu_lib does not match the one $maxstack_encoder was built" >&2
        echo "       against ($cache_lib_sha vs $stamped_lib_sha) - rebuild both:" >&2
        echo "       make parakeet-maxstack-encoder" >&2
        exit 1
    fi
fi

if [ -d "$out" ]; then
    staged="$(pin_revision_of "$out")"
    echo "restaging $out (staged at ${staged:-an unpinned release}, pin is $rev)"
    if [ ! -f "$out/$encoder" ]; then
        # Craft never removes: a file dropped from components/ stays in the
        # part's stage/prime dirs and ships anyway. See the model-components
        # part in snap/snapcraft.yaml.
        echo "note: the encoder changes - run \`snapcraft clean model-components\`" >&2
        echo "      before packing, or the old one is packed again anyway" >&2
    fi
    rm -rf "$out"
fi

mkdir -p "$out"
for src in "$cache"/*; do
    name="$(basename "$src")"
    for skipped in $skip; do
        if [ "$name" = "$skipped" ]; then
            continue 2
        fi
    done
    cp -al "$src" "$out/$name" 2>/dev/null || cp -a "$src" "$out/$name"
done
pin_stamp "$out" "$rev"
echo "component ready at $out ($variant encoder)"
