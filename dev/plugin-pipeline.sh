#!/bin/bash
# Fetch, build, serve, evaluate and gate one text plugin on chug.
#
#   ~/myna-processing-graphs/chug.sh run dev/plugin-pipeline.sh <plugin-id>
#
# <plugin-id> is the `id` (or file stem) of a <snap>-snap/plugins/*.toml. The
# steps are in server/src/myna/benchmarker/plugin_eval.py: licence and NOTICE
# checks, sha256-verified fetch from the Remote Settings CDN into the shared
# model cache, the component staged in a scratch directory, the real provider
# daemon on a Unix socket, `myna-bench text` grading under the plugin's [gate],
# then results/plugins/<id>.json and plugins/<name>.measured.json, after which
# dev/gen-plugins.py regenerates the snap's manifest and catalog from them.
#
# Exit status: 0 gate passed, 1 gate failed (files still written), 2 refused.
# Heavy and network-bound by design: it only runs on chug (no weights on the
# laptop). Environment: MYNA_BERGAMOT, MYNA_MODEL_CACHE, MYNA_RESULTS_DIR.
set -euo pipefail

[ $# -ge 1 ] || { sed -n '2,3p' "$0" >&2; exit 2; }
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
py="${MYNA_PIPELINE_PYTHON:-$HOME/myna-processing-graphs/.venv/bin/python}"
[ -x "$py" ] || py=python3
umask 022

status=0
PYTHONPATH="$repo_root/server/src${PYTHONPATH:+:$PYTHONPATH}" MYNA_REPO_ROOT="$repo_root" \
    "$py" -m myna.benchmarker plugin-eval "$@" || status=$?
[ "$status" -le 1 ] || exit "$status"

for snap_dir in "$repo_root"/*-snap; do
    [ -d "$snap_dir/plugins" ] || continue
    [ -f "$snap_dir/catalog.json" ] || continue
    "$repo_root/dev/gen-plugins.py" "$snap_dir"
done
exit "$status"
