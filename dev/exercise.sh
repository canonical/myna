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
# Scenarios (each fails loudly on a broken assertion; a scenario whose service
# did not come up skips with a clear notice, never a failure):
#   1. fake-adapter dictation, internal dialect (WAV clip)
#   2. fake-adapter dictation, IE115 dialect (WAV clip)
#   3. myna-desktop, the packaged daemon's own entry path       [needs the graph]
#   4. capture from a real PipeWire source                      [needs the graph]
#   5. fake-adapter dictation from a corpus manifest (multi-clip)
#   6. entry-point surfaces: --help, argument errors, --toggle, shortcut install
#
# Scenarios 3 and 4 need a PipeWire graph, a session bus and an IBus daemon.
# This script stands them up the way `cov` does, by re-executing itself under
# dev/gated-tests.sh, rather than testing a gate a developer has to remember to
# export: the gate approach meant scenario 3 had never run once, and every
# myna-desktop entry point read as dead code in the populations report.
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
cleanup() { if [ -n "$SERVER_PID" ]; then kill "$SERVER_PID" 2>/dev/null || true; fi }
trap cleanup EXIT
skip() { printf '\033[33mSKIP\033[0m %s\n' "$*"; }
die() { printf '\033[31mFAIL\033[0m %s\n' "$*" >&2; exit 1; }

command -v cargo-llvm-cov >/dev/null || die "cargo-llvm-cov not installed (workshop provisions it)"
# Corpora are generated, never committed: without this the dictation scenarios
# fail deep inside the client as an opaque "audio device unavailable".
[ -f "$CLIP" ] || die "missing $CLIP - provision the corpus first: workshop run myna corpus"

# --- stand the services up (features 002/003) --------------------------------
# gated-tests.sh publishes a private PipeWire graph with a virtual mic that
# carries audio, a private session bus and a private IBus daemon, exports a
# gate per service that came up, and re-execs us inside all of it. Everything
# it touches is scratch: a developer running this on their desktop keeps their
# own audio graph and input method. A service that does not come up leaves its
# gate unset and its scenario skipping, exactly as it does offline.
if [ -z "${MYNA_EXERCISE_GATED:-}" ] && [ "${MYNA_EXERCISE_NO_GATES:-}" != "1" ]; then
  export MYNA_EXERCISE_GATED=1
  exec "$REPO_ROOT/dev/gated-tests.sh" "${BASH_SOURCE[0]}" "$@"
fi

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

# --- Scenario 3: myna-desktop, the packaged daemon's entry path ----------------
# This is the binary the snap ships. Running it is the only way its argument
# parsing, banner, activation wiring and controller loop are executed at all:
# the unit suite reaches parse_args_from(), never main().
if [ "${MYNA_PIPEWIRE_TESTS:-}" = "1" ] && [ "${MYNA_DBUS_TESTS:-}" = "1" ]; then
  notice "scenario 3: myna-desktop --stdin against $MYNA_PIPEWIRE_TARGET"
  SOCK="$WORK/desktop.sock"; rm -f "$SOCK"
  start_server desktop "$SOCK"
  # Toggle on, let a session run, toggle off, then EOF to quit. The quit has to
  # be a clean exit: the instrumented binary writes its .profraw from an atexit
  # handler, and a signal would kill it first and score the whole run as zero.
  ( sleep 2; printf '\n'; sleep 5; printf '\n'; sleep 3 ) | \
  (cd "$CLIENT" && cargo llvm-cov run --no-report --bin myna-desktop -- \
    --stdin --socket "$SOCK" --target "$MYNA_PIPEWIRE_TARGET") \
    | tee "$WORK/desktop.out" || die "desktop scenario failed (see $WORK/desktop.out)"
  stop_server
  # The transcript is injected into IBus, not printed, so there is nothing on
  # stdout to match: what this asserts is that the daemon reached its run loop
  # with a working injector. The publish and the injection themselves are the
  # dbus_hw / ibus_hw suites' assertions, which `cov` runs against these same
  # services.
  grep -q "myna-desktop" "$WORK/desktop.out" \
    || die "myna-desktop printed no banner (see $WORK/desktop.out)"
  if grep -q "cannot connect to IBus" "$WORK/desktop.out"; then
    die "myna-desktop could not reach the IBus daemon (see $WORK/desktop.out)"
  fi
else
  skip "scenario 3 (myna-desktop): needs a PipeWire graph and a session bus"
fi

# --- Scenario 4: capture from a real PipeWire source ---------------------------
# The virtual mic gated-tests.sh publishes is a real capture path through
# myna-audio's native backend - the only thing hardware adds is the driver
# underneath it. MYNA_LIVE_TESTS=1 aims the same scenario at the machine's
# default device instead, for a maintainer with a microphone in front of them.
MIC_TARGET=()
MIC_WHAT=""
if [ "${MYNA_LIVE_TESTS:-}" = "1" ]; then
  MIC_WHAT="the default device"
elif [ "${MYNA_PIPEWIRE_TESTS:-}" = "1" ]; then
  MIC_TARGET=(--target "$MYNA_PIPEWIRE_TARGET")
  MIC_WHAT="$MYNA_PIPEWIRE_TARGET"
fi
if [ -n "$MIC_WHAT" ]; then
  notice "scenario 4: live capture from $MIC_WHAT"
  SOCK="$WORK/live.sock"; rm -f "$SOCK"
  start_server live "$SOCK"
  ( sleep 1; printf '\n'; sleep 5; printf '\n'; sleep 2 ) | \
  (cd "$CLIENT" && cargo llvm-cov run --no-report --bin myna-dictate -- \
    --socket "$SOCK" --mic "${MIC_TARGET[@]}") \
    | tee "$WORK/live.out" || die "live-capture scenario failed (see $WORK/live.out)"
  stop_server
else
  skip "scenario 4 (live capture): needs a PipeWire graph, or MYNA_LIVE_TESTS=1"
fi

# --- Scenario 5: corpus manifest (multi-clip session) --------------------------
# `--corpus` is how the evaluation harness drives the CLI, and it is a distinct
# path from `--clip`: a manifest read, then several utterances in one session.
# The manifest is written here rather than pointed at corpus/real/manifest.json
# so the scenario costs two utterances whatever tier is provisioned.
notice "scenario 5: fake-adapter dictation from a corpus manifest"
CORPUS_DIR="$WORK/corpus"; rm -rf "$CORPUS_DIR"; mkdir -p "$CORPUS_DIR"
cp "$CLIP" "$CORPUS_DIR/clip.wav"
cat >"$CORPUS_DIR/manifest.json" <<JSON
{"clips": [{"path": "clip.wav", "text": "$EXPECTED"},
           {"path": "clip.wav", "text": "$EXPECTED"}]}
JSON
SOCK="$WORK/corpus.sock"; rm -f "$SOCK"
start_server corpus "$SOCK"
( sleep 1; printf '\n'; sleep 6; printf '\n'; sleep 6 ) | \
(cd "$CLIENT" && cargo llvm-cov run --no-report --bin myna-dictate -- \
  --socket "$SOCK" --corpus "$CORPUS_DIR") | tee "$WORK/corpus.out"
stop_server
grep -qF "$EXPECTED" "$WORK/corpus.out" \
  || die "corpus session missed expected transcript (see $WORK/corpus.out)"

# --- Scenario 6: entry-point surfaces ------------------------------------------
# The one-shot modes and the argument-error paths. Each is a real invocation of
# a shipped binary, which is the point: --help, a rejected combination and the
# shortcut installer are all reachable only through main(). Every one of these
# is expected to exit non-zero at least some of the time, so none of them may
# fail the run - the assertion is on what they printed.
notice "scenario 6: entry-point surfaces (--help, argument errors, --toggle)"
run_cli() { # <binary> <outfile> <args...>
  local bin="$1" out="$2"; shift 2
  (cd "$CLIENT" && cargo llvm-cov run --no-report --bin "$bin" -- "$@") \
    >"$WORK/$out" 2>&1 || true
}

run_cli myna-dictate cli-help.out --help
grep -qi "usage" "$WORK/cli-help.out" || die "myna-dictate --help printed no usage"

# Mutually exclusive selection: the validation in parse_args, not parse_args_from.
run_cli myna-dictate cli-badargs.out --socket "$WORK/none.sock" --mic --clip "$CLIP"
grep -qF -- "--mic and --clip/--corpus are mutually exclusive" "$WORK/cli-badargs.out" \
  || die "myna-dictate accepted --mic with --clip (see $WORK/cli-badargs.out)"

run_cli myna-desktop desktop-help.out --help
grep -qi "usage" "$WORK/desktop-help.out" || die "myna-desktop --help printed no usage"

# No backend at all: the daemon must refuse rather than start half-configured.
run_cli myna-desktop desktop-nosocket.out --stdin
grep -qF -- "is required to run the daemon" "$WORK/desktop-nosocket.out" \
  || die "myna-desktop started with no backend (see $WORK/desktop-nosocket.out)"

# --toggle with nothing listening: exercises control_path + the client leg.
run_cli myna-desktop desktop-toggle.out --toggle --control-socket "$WORK/absent.sock"

# The shortcut installer shells out to gsettings. Under gated-tests.sh both
# XDG_CONFIG_HOME and the session bus are scratch, so the dconf write lands in
# a temporary directory and the developer's real keybindings are untouched. It
# reports failure cleanly where the GNOME schemas are absent, which is a path
# worth covering too.
run_cli myna-desktop desktop-shortcut.out --install-shortcut '<Super>t'
grep -qiE "bound|failed to set" "$WORK/desktop-shortcut.out" \
  || die "myna-desktop --install-shortcut said nothing (see $WORK/desktop-shortcut.out)"

# --- fail-loud merge (FR-005) --------------------------------------------------
notice "merging coverage data"
# coverage --parallel-mode suffixes each --data-file with host.pid.random
# Scenario 5 always runs, so its server data is required too; the gated
# scenarios are not listed - a graph that did not come up is a skip.
for want in internal ie115 corpus; do
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
