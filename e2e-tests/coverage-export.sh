#!/usr/bin/env bash
# Merge the profiles an instrumented e2e run wrote and export them as the
# Cobertura file dev/patch_cov.py counts. Runs in the workshop that built the
# binaries (run-suite.sh calls it with E2E_COVERAGE=1): llvm-cov needs those
# exact objects to read the profiles.
#
# Usage: coverage-export.sh PROFDIR OUT.xml CLIENT_TREE BINARY...
set -euo pipefail

PROF=$1 OUT=$2 TREE=$3; shift 3
shopt -s nullglob
RAW=("$PROF"/*.profraw)
[ ${#RAW[@]} -gt 0 ] || { echo "coverage-export: no profiles in $PROF" >&2; exit 1; }

TOOLS=$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin
OBJECTS=("$1"); shift
for bin; do OBJECTS+=(-object "$bin"); done

"$TOOLS/llvm-profdata" merge -sparse -o "$PROF/merged.profdata" "${RAW[@]}"
"$TOOLS/llvm-cov" export -format=lcov -instr-profile="$PROF/merged.profdata" \
    -ignore-filename-regex='/\.cargo/|/rustc/' "${OBJECTS[@]}" > "$PROF/e2e.lcov"
python3 -I "$(dirname "$0")/../dev/lcov_cobertura.py" "$PROF/e2e.lcov" "$OUT" --root /project/client --client-tree "$TREE"
echo "coverage-export: ${#RAW[@]} profiles -> $OUT"
