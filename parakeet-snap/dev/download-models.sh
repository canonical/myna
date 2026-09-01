#!/bin/bash
# Fetch the Parakeet int8 ONNX weights into components/ so `snapcraft pack`
# can ship them as a snap model component (see snapcraft.yaml
# `model-components` part).
#
# Source: murmure's parakeet-tdt-0.6b-v3-int8 bundle (see
# dev/parakeet/fetch_parakeet_onnx.py for why not istupakov's HF export). Reuses the
# already-staged XDG cache copy when present (hardlinked, no download).
#
# The component ships the maxstack encoder ONLY. The base encoder is the input
# dev/parakeet/build_maxstack_encoder.py derives it from, so it stays in the
# cache and is skipped here: staging both put two encoders of the same model in
# every install, and the adapter only ever loads the maxstack one.
#
#   ./dev/download-models.sh
set -euo pipefail

snap_dir="$(cd "$(dirname "$0")/.." && pwd)"
repo_root="$(dirname "$snap_dir")"
out="$snap_dir/components/model-parakeet-int8"
# shellcheck source-path=SCRIPTDIR/../.. source=dev/model-pin.sh
. "$repo_root/dev/model-pin.sh"

base_encoder=encoder-model.int8.onnx
maxstack_encoder=encoder-model.int8.maxstack.onnx
qsilu_lib=libqsilu.so
maxstack_stamp=MAXSTACK_REVISION

# Upstream release, pinned. Unlike the HF fetchers this one was always pinned
# (a versioned release URL, sha256-verified in the python fetcher); what was
# missing is the staged-directory stamp, so a component staged from an older
# release survived a pin move unnoticed. Keep in step with URL in
# dev/parakeet/fetch_parakeet_onnx.py; test_model_pins.py holds the two together.
rev="murmure-model 1.2.0"

# Restage when the pin moved, and also when the staging *shape* is stale: a
# component carrying the base encoder predates maxstack-only and would ship an
# extra 794 MB nothing loads.
if [ -f "$out/$maxstack_encoder" ] || [ -f "$out/$base_encoder" ]; then
    if pin_is_current "$out" "$rev" &&
        [ -f "$out/$maxstack_encoder" ] && [ ! -f "$out/$base_encoder" ]; then
        echo "model already present at $out - skipping"
        exit 0
    fi
    staged="$(pin_revision_of "$out")"
    echo "restaging $out (staged at ${staged:-an unpinned release}, pin is $rev)"
    if [ -f "$out/$base_encoder" ]; then
        # Craft never removes: a file dropped from components/ stays in the
        # part's stage/prime dirs and ships anyway. See the model-components
        # part in snap/snapcraft.yaml.
        echo "note: this drops $base_encoder from the component - run" >&2
        echo "      \`snapcraft clean model-components\` before packing, or the" >&2
        echo "      old encoder is packed again from craft's staged copy" >&2
    fi
    rm -rf "$out"
fi

# The python fetcher is the guard: it stages only when the XDG cache carries
# this release's stamp, so a cache left from an older pin is re-downloaded and
# sha256-verified rather than hardlinked in blind.
cache="${XDG_CACHE_HOME:-$HOME/.cache}/myna/models/parakeet-tdt-0.6b-v3-int8"
cd "$repo_root/server"
uv run python "$repo_root/dev/parakeet/fetch_parakeet_onnx.py"

# The fetcher stages the upstream bundle; the maxstack encoder is built on top
# of it and is not part of any download. Without it there is nothing to ship,
# so say exactly what to run rather than packing a component the adapter
# cannot load.
for f in "$maxstack_encoder" "$qsilu_lib" "$maxstack_stamp"; do
    if [ ! -f "$cache/$f" ]; then
        echo "error: $cache/$f missing - the component ships the maxstack encoder only" >&2
        echo "       build it: dev/parakeet/qsilu/build.sh, then" >&2
        echo "       cd server && uv run python ../dev/parakeet/build_maxstack_encoder.py \\" >&2
        echo "           --model-dir $cache" >&2
        exit 1
    fi
done

# The maxstack encoder is derived from one specific base export, and with the
# base no longer shipped nothing downstream can notice a mismatch. Its stamp
# records what it was built from and which kernel library it was paired with;
# check both here, where a re-fetch under a moved pin would otherwise leave a
# stale derived encoder sitting in the cache.
built_from="$(sed -n 's/^base: //p' "$cache/$maxstack_stamp")"
if [ "$built_from" != "$rev" ]; then
    echo "error: $maxstack_encoder was built from ${built_from:-an unrecorded release}," >&2
    echo "       not the pinned $rev - rebuild it with build_maxstack_encoder.py" >&2
    exit 1
fi
stamped_lib_sha="$(sed -n 's/^libqsilu sha256: //p' "$cache/$maxstack_stamp")"
cache_lib_sha="$(sha256sum "$cache/$qsilu_lib" | cut -d' ' -f1)"
if [ "$stamped_lib_sha" != "$cache_lib_sha" ]; then
    echo "error: $qsilu_lib does not match the one $maxstack_encoder was built" >&2
    echo "       against ($cache_lib_sha vs $stamped_lib_sha) - rebuild both" >&2
    exit 1
fi

mkdir -p "$out"
for src in "$cache"/*; do
    name="$(basename "$src")"
    if [ "$name" = "$base_encoder" ]; then
        continue
    fi
    cp -al "$src" "$out/$name" 2>/dev/null || cp -a "$src" "$out/$name"
done
pin_stamp "$out" "$rev"
echo "component ready at $out"
