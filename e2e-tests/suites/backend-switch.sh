#!/usr/bin/env bash
# snapshot: installed
# Backend switching on General: with parakeet connected and whisper also
# installed, activating Whisper's radio raises snapd's polkit prompt; a
# dismissed prompt silently reverts the radio and changes nothing, an
# allowed one connects myna:backend to whisper and restarts the daemon.
set -uo pipefail
# shellcheck source=e2e-tests/suites/lib.sh
source "$(dirname "$0")/lib.sh"

P=backend-switch-$REL

assert_on "precondition: backend is parakeet" \
    'snap connections myna | grep -q "myna:backend.*myna-parakeet:provider"'

# A dismissed prompt reverts silently: the connection must not move.
shot --polkit cancel \
    wait:4 waitfor:General \
    click:Whisper wait:4 shot:"$P"-01-cancelled.png \
    || { echo "cancel run failed" >&2; exit 1; }
assert_on "cancel left backend on parakeet" \
    'snap connections myna | grep -q "myna:backend.*myna-parakeet:provider"'

# An allowed prompt switches: connection moves, daemon comes back active.
shot --polkit allow --monitor "$P" \
    wait:4 waitfor:General \
    click:Whisper wait:10 shot:"$P"-02-switched.png \
    || { echo "allow run failed" >&2; exit 1; }
assert_on "backend connected to whisper" \
    'snap connections myna | grep -q "myna:backend.*myna-whisper:provider"'
assert_on "daemon active after switch" \
    'systemctl --user is-active -q snap.myna.myna.service'
suite_status
