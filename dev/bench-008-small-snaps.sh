#!/usr/bin/env bash
# Watermark sweep for the 008 small transducer backends (US3/US4): parakeet
# (chunked-commit) and sherpa (native streaming), batch + streaming, on the
# 26-28 s concatenated streams — mirrors dev/rebaseline-streaming-watermarks.sh.
#
# Usage: cd /home/charles/Projects/myna && bash dev/bench-008-small-snaps.sh

set -euo pipefail

REPO_ROOT=/home/charles/Projects/myna
SERVER_DIR=$REPO_ROOT/server
SOCKET=/tmp/myna-bench-small.sock
OUT=$REPO_ROOT/results/bench-008-small-snaps.jsonl
MANIFEST=$REPO_ROOT/corpus/english/manifest-streams.json
BENCH=$REPO_ROOT/dev/bench.py
AGG=$REPO_ROOT/dev/aggregate.py

rm -f "$OUT"

cleanup() {
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
    rm -f "$SOCKET"
}
trap cleanup EXIT

run_sweep() {
    local adapter="$1" mode="$2" label="$3"
    local streaming_flag=""
    [ "$mode" = "streaming" ] && streaming_flag="--streaming"

    echo ""
    echo "============================================================"
    echo "  $adapter / $mode  ->  $label"
    echo "============================================================"

    rm -f "$SOCKET"
    cd "$SERVER_DIR"
    uv run python -m myna.server \
        --socket "$SOCKET" \
        --adapter "$adapter" \
        --preload \
        $streaming_flag \
        &
    SERVER_PID=$!

    for _ in $(seq 1 60); do
        [ -S "$SOCKET" ] && break
        sleep 1
    done
    [ -S "$SOCKET" ] || { echo "ERROR: socket never appeared"; exit 1; }
    sleep 2

    cd "$SERVER_DIR"
    PYTHONPATH=src uv run python "$BENCH" \
        --socket "$SOCKET" \
        --manifest "$MANIFEST" \
        --label "$label" \
        $streaming_flag \
        --out "$OUT"

    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
    rm -f "$SOCKET"
}

run_sweep parakeet batch     "parakeet-tdt-0.6b-v3-int8/batch"
run_sweep parakeet streaming "parakeet-tdt-0.6b-v3-int8/chunked-commit"
run_sweep sherpa   batch     "fastconformer-480ms-int8/batch"
run_sweep sherpa   streaming "fastconformer-480ms-int8/native-transducer"

echo ""
echo "============================================================"
echo "  All runs complete. Results: $OUT"
echo "============================================================"

cd "$SERVER_DIR"
PYTHONPATH=src uv run python "$AGG" --in "$OUT"
