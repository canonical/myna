#!/bin/bash
# Build libqsilu.so against the onnxruntime release headers (version must
# match the installed python onnxruntime - custom-op ABI is version-pinned).
#
#   ./build.sh                                     # fetch the pinned headers
#   ORT_INCLUDE=/path/to/onnxruntime-*/include ./build.sh
#
# Deliberately no -march/-mtune: this needs to run on whatever CPU a user
# has, not just the reference machine, so it targets the compiler's default
# x86-64 baseline; hot loops are written to auto-vectorize at whatever level
# the baseline allows and stay correct (scalar) everywhere else.
set -eu
cd "$(dirname "$0")"

# The onnxruntime server/uv.lock resolves; test_model_pins.py holds them together.
ort_version=1.27.0
ort_sha256=547e40a48f1fe73e3f812d7c88a948612c23f896b91e4e2ee1e232d7b468246f

if [ -z "${ORT_INCLUDE:-}" ]; then
    if [ "$(uname -m)" != "x86_64" ]; then
        echo "error: no pinned header tarball for $(uname -m) - set ORT_INCLUDE to" >&2
        echo "       the include/ dir of an onnxruntime $ort_version release" >&2
        exit 1
    fi
    cache="${XDG_CACHE_HOME:-$HOME/.cache}/myna/onnxruntime-$ort_version"
    ORT_INCLUDE="$cache/include"
    if [ ! -f "$ORT_INCLUDE/onnxruntime_lite_custom_op.h" ]; then
        url="https://github.com/microsoft/onnxruntime/releases/download/v$ort_version/onnxruntime-linux-x64-$ort_version.tgz"
        tmp="$(mktemp -d)"
        trap 'rm -rf "$tmp"' EXIT
        echo "fetching onnxruntime $ort_version headers"
        curl -fsSL -o "$tmp/ort.tgz" "$url"
        digest="$(sha256sum "$tmp/ort.tgz" | cut -d' ' -f1)"
        if [ "$digest" != "$ort_sha256" ]; then
            echo "error: sha256 mismatch: $digest != $ort_sha256 (pinned $url)" >&2
            exit 1
        fi
        rm -rf "$cache"
        mkdir -p "$cache"
        tar -xzf "$tmp/ort.tgz" -C "$cache" --strip-components=1 \
            "onnxruntime-linux-x64-$ort_version/include"
    fi
fi

g++ -O3 -Wall -Wextra -fPIC -shared -std=c++17 \
  -I "$ORT_INCLUDE" \
  silu_qop.cc -o libqsilu.so
echo "built $(pwd)/libqsilu.so"
