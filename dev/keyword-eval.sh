#!/usr/bin/env bash
# keyword-eval - score keyword biasing against an in-tree server, off then on.
#
#   dev/keyword-eval.sh [LABEL] [-- <extra myna.server args>]
#   make bench-keywords KEYWORD_LABEL=boost-2 KEYWORD_SERVER_ARGS='--streaming'
#
# Starts `python -m myna.server --adapter parakeet` from this checkout on a
# private socket (the shipped int8 weights by default), sweeps the frozen
# keyword tier (corpus/english/manifest-keywords.json, built from the cached
# dev-clean tarball on first use) twice - keywords scored but not sent, then
# sent - and prints both rows side by side. Rows land in
# results/keywords.jsonl as <LABEL>/off and <LABEL>/on; a new LABEL adds rows,
# so earlier runs stay comparable in the same table.
#
# Runs on the host, not in the workshop: the weights live in ~/.cache/myna. The
# server gets an isolated uv environment, so it never touches server/.venv.
# Pass --streaming among the server args to measure the streaming path; the
# bench side follows.
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
label="${1:-parakeet}"
[ $# -gt 0 ] && shift
[ "${1:-}" = "--" ] && shift
server_args=("$@")
model="${MYNA_KEYWORD_MODEL:-$HOME/.cache/myna/models/parakeet-tdt-0.6b-v3-int8}"
manifest="$repo/corpus/english/manifest-keywords.json"
out="$repo/results/keywords.jsonl"
run=(uv run --project "$repo/server" --isolated --no-dev --extra parakeet --extra benchmarker python)

bench_args=()
for arg in "${server_args[@]}"; do
    [ "$arg" = "--streaming" ] && bench_args+=(--streaming)
done

if [ ! -f "$manifest" ]; then
    "${run[@]}" -m myna.benchmarker download-corpus --select keywords \
        --tarball "$repo/.cache/librispeech/dev-clean.tar.gz" \
        --out "$repo/corpus/english" --manifest-name manifest-keywords.json
fi

sockdir="$(mktemp -d)"
server_pid=""
cleanup() {
    [ -n "$server_pid" ] && kill "$server_pid" 2>/dev/null && wait "$server_pid" 2>/dev/null
    rm -rf "$sockdir"
}
trap cleanup EXIT

"${run[@]}" -m myna.server --adapter parakeet --model "$model" --preload \
    --socket "$sockdir/myna.sock" "${server_args[@]}" &
server_pid=$!
for _ in $(seq 120); do
    [ -S "$sockdir/myna.sock" ] && break
    kill -0 "$server_pid" 2>/dev/null || { echo "server exited before listening" >&2; exit 1; }
    sleep 1
done
[ -S "$sockdir/myna.sock" ] || { echo "server did not listen within 120 s" >&2; exit 1; }

for mode in off on; do
    send=()
    [ "$mode" = on ] && send=(--send-keywords)
    "${run[@]}" -m myna.benchmarker bench --socket "$sockdir/myna.sock" \
        --manifest "$manifest" --label "$label/$mode" --out "$out" \
        "${bench_args[@]}" "${send[@]}"
done
"${run[@]}" -m myna.benchmarker summarize --in "$out" --sort label
