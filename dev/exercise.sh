#!/usr/bin/env bash
# exercise — run myna's real use-cases under coverage instrumentation
# (feature 006, FR-004). Both toolchains instrumented:
#
#   Rust:   `cargo llvm-cov run --bin ...` appends .profraw to
#           client/target/llvm-cov-target/ (merged with the test-suite data by
#           the final `cargo llvm-cov report`).
#   Python: `coverage run --parallel-mode --context=usecase:<name>` writes
#           server/.coverage.usecase-* files, combined with the test-suite
#           data below.
#
# Scenarios (each fails loudly on a broken assertion; gated scenarios skip
# with a clear notice and are recorded as unexercised, never as failures):
#   1. fake-adapter dictation, internal dialect (WAV clip)
#   2. fake-adapter dictation, IE115 dialect (WAV clip)
#   3. myna-desktop --dbus publisher path   [gated: MYNA_PIPEWIRE_TESTS]
#   4. live capture                          [gated: MYNA_LIVE_TESTS]
#
# Prerequisites: `workshop run myna cov py-cov` first (or this script runs the
# suites itself when their raw data is missing). Run from anywhere; paths are
# resolved from the script location.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLIENT="$REPO_ROOT/client"
SERVER="$REPO_ROOT/server"
CLIP="$REPO_ROOT/corpus/real/audio/librispeech-2277-149896-0005.wav"
EXPECTED="The quick brown fox jumps over the lazy dog."
WORK="$REPO_ROOT/.coverage-work"
mkdir -p "$WORK"

notice() { printf '\033[1m== %s\033[0m\n' "$*"; }
SERVER_PID=""
cleanup() { [ -n "$SERVER_PID" ] && kill "$SERVER_PID" 2>/dev/null || true; }
trap cleanup EXIT
skip() { printf '\033[33mSKIP\033[0m %s\n' "$*"; }
die() { printf '\033[31mFAIL\033[0m %s\n' "$*" >&2; exit 1; }

command -v cargo-llvm-cov >/dev/null || die "cargo-llvm-cov not installed (workshop provisions it)"

# --- fail-loud prerequisite check (FR-005) -----------------------------------
# Rust: the merged report needs the test-suite .profraw. If absent, run the
# suite now so a standalone `exercise` still yields a complete merged report.
if ! ls "$CLIENT"/target/llvm-cov-target/*.profraw >/dev/null 2>&1; then
  notice "no Rust test coverage data; running the workspace suite first"
  (cd "$CLIENT" && cargo llvm-cov --workspace --summary-only >/dev/null)
fi
# Python: the test-suite data file, stashed by the py-cov action.
if [ ! -f "$SERVER/.coverage.tests" ]; then
  notice "no Python test coverage data; running pytest with coverage first"
  (cd "$SERVER" && uv sync -q && uv run pytest -q --cov=myna --cov-branch --cov-context=test \
    --cov-report= --cov-report=xml:coverage-tests.cobertura.xml >/dev/null)
  mv "$SERVER/.coverage" "$SERVER/.coverage.tests"
fi

start_server() { # <context> <socket>
  local context="$1" sock="$2"
  # Direct venv binary via `env -C`, not a subshell or `uv run`: wrappers
  # fork the server as a child our kill can't reach (same constraint as the
  # e2e tests) — $! must be the server process itself.
  local cov="$SERVER/.venv/bin/coverage"
  [ -x "$cov" ] || cov="coverage"
  env -C "$SERVER" "$cov" run --parallel-mode \
    --data-file="$SERVER/.coverage.usecase-$context" "--context=usecase:$context" \
    -m myna.server --adapter fake --socket "$sock" \
    >"$WORK/server-$context.log" 2>&1 &
  SERVER_PID=$!
  for _ in $(seq 1 100); do [ -S "$sock" ] && return 0; sleep 0.1; done
  die "server ($context) did not bind $sock; log: $WORK/server-$context.log"
}
stop_server() {
  kill -INT "$SERVER_PID" 2>/dev/null || true
  wait "$SERVER_PID" 2>/dev/null || true
}

# --- Scenario 1: internal dialect ---------------------------------------------
notice "scenario 1: fake-adapter dictation, internal dialect"
SOCK="$WORK/internal.sock"; rm -f "$SOCK"
start_server internal "$SOCK"
# myna-dictate is stdin-triggered: Enter starts the utterance; the clip then
# plays out and EOF quits the CLI.
( sleep 1; printf '\n'; sleep 6 ) | \
(cd "$CLIENT" && cargo llvm-cov run --no-report --bin myna-dictate -- \
  --socket "$SOCK" --clip "$CLIP") | tee "$WORK/internal.out"
stop_server
grep -qF "$EXPECTED" "$WORK/internal.out" \
  || die "internal-dialect session missed expected transcript (see $WORK/internal.out)"

# --- Scenario 2: IE115 dialect -------------------------------------------------
notice "scenario 2: fake-adapter dictation, IE115 dialect"
SOCK="$WORK/ie115.sock"; rm -f "$SOCK"
start_server ie115 "$SOCK"
( sleep 1; printf '\n'; sleep 6 ) | \
(cd "$CLIENT" && cargo llvm-cov run --no-report --bin myna-dictate -- \
  --socket "$SOCK" --clip "$CLIP" --dialect ie115) | tee "$WORK/ie115.out"
stop_server
grep -qF "$EXPECTED" "$WORK/ie115.out" \
  || die "IE115-dialect session missed expected transcript (see $WORK/ie115.out)"

# --- Scenario 3: desktop --dbus publisher (gated) ------------------------------
if [ "${MYNA_PIPEWIRE_TESTS:-}" = "1" ] && command -v pw-loopback >/dev/null; then
  notice "scenario 3: myna-desktop --dbus publisher (virtual source)"
  SOCK="$WORK/desktop.sock"; rm -f "$SOCK"
  start_server desktop "$SOCK"
  # shellcheck disable=SC2016  # late expansion of the injected $SOCK/$CLIENT is intentional
  dbus-run-session -- bash -c '
    set -euo pipefail
    pw-loopback -n myna-exercise-src --capture-props="media.class=Audio/Source" &
    LOOPBACK=$!; trap "kill $LOOPBACK 2>/dev/null || true" EXIT
    sleep 1
    cd "'"$CLIENT"'"
    # toggle on, let a session run, toggle off; transcript goes to stdout
    ( sleep 2; printf "\n"; sleep 4; printf "\n"; sleep 2 ) | \
      cargo llvm-cov run --no-report --bin myna-desktop -- \
        --dbus --stdin --socket "'"$SOCK"'" --target myna-exercise-src
  ' | tee "$WORK/desktop.out" || die "desktop publisher scenario failed"
  stop_server
else
  skip "scenario 3 (desktop --dbus publisher): needs MYNA_PIPEWIRE_TESTS=1 and pw-loopback"
fi

# --- Scenario 4: live capture (gated) ------------------------------------------
if [ "${MYNA_LIVE_TESTS:-}" = "1" ]; then
  notice "scenario 4: live capture from the default source"
  SOCK="$WORK/live.sock"; rm -f "$SOCK"
  start_server live "$SOCK"
  (cd "$CLIENT" && timeout 20 cargo llvm-cov run --no-report --bin myna-dictate -- \
    --socket "$SOCK" --mic) || die "live-capture scenario failed"
  stop_server
else
  skip "scenario 4 (live capture): needs MYNA_LIVE_TESTS=1 and capture hardware"
fi

# --- fail-loud merge (FR-005) --------------------------------------------------
notice "merging coverage data"
# coverage --parallel-mode suffixes each --data-file with host.pid.random
for want in internal ie115; do
  ls "$SERVER"/.coverage.usecase-"$want".* >/dev/null 2>&1 \
    || die "missing raw Python coverage for usecase:$want (server crashed? see $WORK/server-$want.log)"
done
ls "$CLIENT"/target/llvm-cov-target/*.profraw >/dev/null 2>&1 \
  || die "missing raw Rust coverage (.profraw) after use-case runs"

COV="$SERVER/.venv/bin/coverage"; [ -x "$COV" ] || COV=coverage
(cd "$SERVER" && "$COV" combine .coverage.tests .coverage.usecase-* >/dev/null)
(cd "$SERVER" && "$COV" xml -o coverage-merged.cobertura.xml >/dev/null)
mkdir -p "$CLIENT/target/coverage"
(cd "$CLIENT" && cargo llvm-cov report --cobertura \
  --output-path target/coverage/rust-merged.cobertura.xml >/dev/null)

notice "merged exports written:"
echo "  client/target/coverage/rust-merged.cobertura.xml"
echo "  server/coverage-merged.cobertura.xml"
echo "next: dev/coverage_populations.py (or \`workshop run myna deadcode\`)"
