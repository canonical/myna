#!/usr/bin/env bash
# Build myna-bench.pyz — the standalone benchmarker zipapp.
#
#   bash dev/build-bench.sh            (or: make build-bench)
#   python3 myna-bench.pyz plan --config bench.yaml
#   sudo python3 myna-bench.pyz run --config bench.yaml
#
# Produces a single self-contained executable that testers can download
# and run without a repo checkout or a virtualenv. Requires:
#   pip install shiv
# or:
#   uv tool install shiv
#
# The resulting .pyz bundles:
#   - myna.core, myna.testbed, myna.server, myna.benchmarker  (from server/src/)
#   - websockets, psutil, pyyaml                              (from PyPI)
#
# myna.benchmarker is the whole benchmarking surface - corpus builders, clip
# scorer, snap sweep, aggregator and environment guard - so this zipapp and the
# in-tree `make bench-run` measure the same thing by construction. It stays
# light because the package leans only on myna.core (websockets), myna.testbed
# (stdlib) and myna.server.lifecycle (stdlib): no torch, no onnxruntime.
#
# Run with sudo for the ``run`` subcommand (snap install/remove). Everything
# else (plan, bench, download-corpus, make-corpus, summarize, check) is
# unprivileged.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${REPO_ROOT}/myna-bench.pyz"

# Build inside a temp venv so we don't pollute the project venv.
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

python3 -m venv "$WORK/venv"
"$WORK/venv/bin/pip" install --quiet shiv websockets psutil pyyaml

echo "building $OUT …"
"$WORK/venv/bin/shiv" \
    --site-packages "$REPO_ROOT/server/src" \
    --entry-point "myna.benchmarker.__main__:main" \
    --python "/usr/bin/env python3" \
    --output-file "$OUT" \
    websockets psutil pyyaml

echo "done: $OUT"
echo
echo "usage:"
echo "  python3 myna-bench.pyz download-corpus --out ./corpus"
echo "  python3 myna-bench.pyz plan --config bench.yaml"
echo "  sudo python3 myna-bench.pyz run --config bench.yaml"
echo "  python3 myna-bench.pyz summarize --in results.jsonl --by-category"
