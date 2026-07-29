#!/usr/bin/env bash
# Re-baseline streaming-mode watermarks on the 26-28 s concatenated streams
# after the character-level frontier-anchored dedupe fix (d7d9c72).
#
# Runs whisper-tiny through all three streaming strategies + batch against
# corpus/real/manifest-streams.json, appending results to a fresh JSONL.
#
# Usage: cd /home/charles/Projects/myna && bash dev/rebaseline-streaming-watermarks.sh

set -euo pipefail

REPO_ROOT=/home/charles/Projects/myna
SERVER_DIR=$REPO_ROOT/server
SOCKET=/tmp/myna-baseline.sock
OUT=$REPO_ROOT/results/bench-008-rebaseline.jsonl
MANIFEST=$REPO_ROOT/corpus/real/manifest-streams.json
LABEL_PREFIX=whisper-tiny
BENCH=$REPO_ROOT/dev/bench.py
AGG=$REPO_ROOT/dev/aggregate.py

# Fresh output — wipe previous rebaseline runs to avoid duplicate-key confusion
rm -f "$OUT"

cleanup() {
    echo "cleaning up server..."
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
    rm -f "$SOCKET"
}
trap cleanup EXIT

run_sweep() {
    local strategy="$1"   # batch | local-agreement | tail-mutation | fixed-head
    local label="$2"

    echo ""
    echo "============================================================"
    echo "  Strategy: $strategy  ->  label: $label"
    echo "============================================================"

    if [ "$strategy" = "batch" ]; then
        streaming_flag=""
    else
        streaming_flag="--streaming"
    fi

    # Start the server
    rm -f "$SOCKET"
    cd "$SERVER_DIR"
    uv run python -m myna.server \
        --socket "$SOCKET" \
        --adapter whisper \
        --model tiny \
        --device cpu \
        --preload \
        $streaming_flag \
        ${streaming_flag:+--strategy "$strategy"} \
        &
    SERVER_PID=$!
    echo "server started (PID $SERVER_PID), waiting for socket..."

    # Wait for the socket to appear (server may take a moment to start)
    for i in $(seq 1 30); do
        if [ -S "$SOCKET" ]; then
            echo "socket ready after ${i}s"
            break
        fi
        sleep 1
    done
    if [ ! -S "$SOCKET" ]; then
        echo "ERROR: socket never appeared"
        exit 1
    fi

    # Give the server a moment to finish preloading
    sleep 2

    # Run the bench — use the server's uv environment (has websockets etc.)
    cd "$SERVER_DIR"
    PYTHONPATH=src uv run python "$BENCH" \
        --socket "$SOCKET" \
        --manifest "$MANIFEST" \
        --label "$label" \
        $streaming_flag \
        --out "$OUT"

    # Kill the server
    echo "stopping server..."
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
    rm -f "$SOCKET"
    echo "done with $strategy"
}

# ── Sweep ──────────────────────────────────────────────────────────────────

run_sweep batch              "$LABEL_PREFIX/batch"
run_sweep local-agreement    "$LABEL_PREFIX/local-agreement"
run_sweep tail-mutation      "$LABEL_PREFIX/tail-mutation"
run_sweep fixed-head         "$LABEL_PREFIX/fixed-head"

echo ""
echo "============================================================"
echo "  All runs complete."
echo "  Results: $OUT"
echo "============================================================"

# Print a quick summary
cd "$SERVER_DIR"
PYTHONPATH=src uv run python "$AGG" --in "$OUT"