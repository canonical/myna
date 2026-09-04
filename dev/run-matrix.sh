#!/bin/bash
# Drive the bench across engine/model configs and aggregate (T11).
#
# Switches the whisper snap's active engine + model, restarts the daemon,
# runs a full --batch sweep tagged with the config, and finally prints the
# comparison table. Requires sudo (snap config + restart).
#
#   ./dev/run-matrix.sh [socket]
#
# Edit CONFIGS to taste; each line is "<engine> <model>". Models must be
# installed components (tiny/base/small); nvidia-gpu also needs the
# faster-whisper-cuda component installed.
set -euo pipefail

SOCKET="${1:-/var/snap/myna-whisper/common/run/ubustt.sock}"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$REPO_ROOT/results/bench.jsonl"

CONFIGS=(
  "cpu tiny"
  "cpu small"
  "nvidia-gpu small"
)

# Short clip used to measure cold load (model load + a small decode) right
# after each restart, before the warm sweep.
WARMUP_CLIP="commands-minimal"

wait_for_socket() {
  for _ in $(seq 1 30); do
    [ -S "$SOCKET" ] && return 0
    sleep 1
  done
  echo "socket $SOCKET did not appear" >&2
  return 1
}

echo "resetting $OUT"
rm -f "$OUT"

for cfg in "${CONFIGS[@]}"; do
  read -r engine model <<<"$cfg"
  echo "=== $engine/$model ==="
  sudo myna-whisper.whisper use-engine "$engine" --assume-yes
  sudo myna-whisper.whisper use-model "$model" --assume-yes
  sudo snap restart myna-whisper.server
  wait_for_socket
  # Cold sample: the very first request loads the model. Captured as its own
  # tagged record (--cold) so it doesn't pollute the warm sweep.
  uv run python "$REPO_ROOT/dev/bench.py" \
    --socket "$SOCKET" --label "$engine/$model" --batch --cold "$WARMUP_CLIP"
  # Warm sweep: model now resident in the daemon. Real per-clip latency + WER.
  uv run python "$REPO_ROOT/dev/bench.py" \
    --socket "$SOCKET" --label "$engine/$model" --batch
done

echo
echo "===================== MATRIX ====================="
uv run python "$REPO_ROOT/dev/aggregate.py" --by-category
