#!/bin/bash
# Build libqsilu.so against the onnxruntime release headers (version must
# match the installed python onnxruntime - custom-op ABI is version-pinned).
#
#   ORT_INCLUDE=/path/to/onnxruntime-linux-x64-1.27.0/include ./build.sh
#
# Deliberately no -march/-mtune: this needs to run on whatever CPU a user
# has, not just the reference machine, so it targets the compiler's default
# x86-64 baseline; hot loops are written to auto-vectorize at whatever level
# the baseline allows and stay correct (scalar) everywhere else.
set -eu
cd "$(dirname "$0")"
: "${ORT_INCLUDE:?set ORT_INCLUDE to the onnxruntime release include/ dir}"
g++ -O3 -Wall -Wextra -fPIC -shared -std=c++17 \
  -I "$ORT_INCLUDE" \
  silu_qop.cc -o libqsilu.so
echo "built $(pwd)/libqsilu.so"
