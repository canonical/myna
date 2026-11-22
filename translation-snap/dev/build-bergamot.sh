#!/bin/bash
# Build myna-bergamot (bergamot-translator plus our line-at-a-time front end)
# without Intel MKL, and install it with its licence texts.
#
#   dev/build-bergamot.sh <bergamot-translator source> <install dir>
#
# The snap's `bergamot` part runs this; it is a script so the same recipe can be
# run by hand to iterate without packing a snap. The PyPI `bergamot` wheel
# embeds MKL (proprietary), so the runtime is built from source: Marian's
# generic CPU path (USE_WASM_COMPATIBLE_SOURCE=ON), no MKL, no Python binding.
set -euo pipefail

src="$(cd "$1" && pwd)"
dest="$2"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
jobs="${CRAFT_PARALLEL_BUILD_COUNT:-$(nproc)}"

cp "$here/bergamot/myna-bergamot.cpp" "$src/app/myna-bergamot.cpp"
if ! grep -q myna-bergamot "$src/app/CMakeLists.txt"; then
    cat >>"$src/app/CMakeLists.txt" <<'CMAKE'
add_executable(myna-bergamot myna-bergamot.cpp)
target_link_libraries(myna-bergamot PRIVATE bergamot-translator)
CMAKE
fi

# x86-64-v2 (SSE4.2): every amd64 CPU the snap supports. -v3 would need AVX2.
arch=()
[ "$(dpkg --print-architecture)" = amd64 ] && arch=(-DBUILD_ARCH=x86-64-v2)

# cmake 4 refuses the 3.x minimum of the bundled pcre2, which is configured
# at build time, so this must reach the build step too.
export CMAKE_POLICY_VERSION_MINIMUM=3.5
cmake -S "$src" -B "$src/build" -G "Unix Makefiles" \
    -DCMAKE_BUILD_TYPE=Release \
    -DUSE_WASM_COMPATIBLE_SOURCE=ON -DUSE_MKL=OFF -DCOMPILE_PYTHON=OFF \
    -DCOMPILE_UNIT_TESTS=OFF -DCOMPILE_TESTS=OFF \
    "${arch[@]}"
cmake --build "$src/build" --target myna-bergamot --parallel "$jobs"

install -D --mode=755 --strip "$src/build/app/myna-bergamot" "$dest/bin/myna-bergamot"

# What the binary statically contains (from its link line and symbols), with
# the licence text of each. A missing file fails the build: the notices are
# part of the product.
lic="$dest/usr/share/doc/myna-translation/bergamot"
marian=3rd_party/marian-dev
while read -r from to; do
    [ -n "$from" ] || continue
    install -D --mode=644 "$src/$from" "$lic/$to"
done <<LICENSES
LICENSE bergamot-translator/LICENSE
$marian/LICENSE.md marian-dev/LICENSE.md
3rd_party/ssplit-cpp/LICENSE.md ssplit-cpp/LICENSE.md
3rd_party/ssplit-cpp/src/3rd-party/pcre2-10.39/LICENCE pcre2/LICENCE
$marian/src/3rd_party/sentencepiece/LICENSE sentencepiece/LICENSE
$marian/src/3rd_party/intgemm/LICENSE intgemm/LICENSE
$marian/src/3rd_party/ruy/LICENSE ruy/LICENSE
$marian/src/3rd_party/yaml-cpp/LICENSE yaml-cpp/LICENSE
$marian/src/3rd_party/CLI/LICENSE CLI11/LICENSE
$marian/src/3rd_party/spdlog/LICENSE spdlog/LICENSE
$marian/src/3rd_party/pathie-cpp/LICENSE pathie-cpp/LICENSE
$marian/src/3rd_party/cnpy/LICENSE cnpy/LICENSE
$marian/src/3rd_party/mio/LICENSE mio/LICENSE
$marian/src/3rd_party/phf/LICENSE phf/LICENSE
$marian/src/3rd_party/zlib/README zlib/README
LICENSES
